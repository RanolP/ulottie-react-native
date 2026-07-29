//! Emitting the JS module for a planned [`Scene`].
//!
//! The runtime is a set of small ES modules with globally-unique top-level
//! names, so "bundling" is a topological concatenation with `import`/`export`
//! lines stripped — no IIFE namespaces, no indirection through namespace
//! objects, and the minifier gets one flat scope to mangle.
//!
//! Which modules get concatenated is decided by the scene's [`Caps`], not by
//! hoping a tree-shaker finds the dead code afterwards.

use crate::scene::{op, Caps, Scene};
use crate::{MarkupMode, RuntimeMode};

use super::codegen;
use super::layers::Exprs;
use super::pretty;
use super::runtime::minify;
use super::shake;

// ---------------------------------------------------------------------------
// Module registry — listed in dependency order.
// ---------------------------------------------------------------------------

pub struct Mod {
    /// Module specifier relative to the runtime root, e.g. `ops/txt.js`.
    pub name: &'static str,
    pub src: &'static str,
}

macro_rules! modules {
    ($($name:literal => $path:literal),* $(,)?) => {
        &[$(Mod { name: $name, src: include_str!($path) }),*]
    };
}

const MODS: &[Mod] = modules![
    "num.js"     => "../../runtime/num.js",
    "kfval.js"   => "../../runtime/kfval.js",
    "play.js"    => "../../runtime/play.js",
    "vlq.js"     => "../../runtime/vlq.js",
    "wire.js"    => "../../runtime/wire.js",
    "col.js"     => "../../runtime/col.js",
    "scale.js"   => "../../runtime/scale.js",
    "set.js"     => "../../runtime/set.js",
    "rec.js"     => "../../runtime/rec.js",
    "ids.js"     => "../../runtime/ids.js",
    "tpl.js"     => "../../runtime/tpl.js",
    "sprite.js"  => "../../runtime/sprite.js",
    "css.js"     => "../../runtime/css.js",
    "ease.js"    => "../../runtime/ease.js",
    "spatial.js" => "../../runtime/spatial.js",
    "kfpath.js"  => "../../runtime/kfpath.js",
    "expr.js"    => "../../runtime/expr.js",
    "kf.js"      => "../../runtime/kf.js",
    "path.js"    => "../../runtime/path.js",
    "geom.js"    => "../../runtime/geom.js",
    "trim.js"    => "../../runtime/trim.js",
    "ops/tx.js"       => "../../runtime/ops/tx.js",
    "ops/txt.js"      => "../../runtime/ops/txt.js",
    "ops/opacity.js"  => "../../runtime/ops/opacity.js",
    "ops/display.js"  => "../../runtime/ops/display.js",
    "ops/shape.js"    => "../../runtime/ops/shape.js",
    "ops/rect.js"     => "../../runtime/ops/rect.js",
    "ops/ellipse.js"  => "../../runtime/ops/ellipse.js",
    "ops/fill.js"     => "../../runtime/ops/fill.js",
    "ops/stroke.js"   => "../../runtime/ops/stroke.js",
    "ops/grad.js"     => "../../runtime/ops/grad.js",
    "ops/layer.js"    => "../../runtime/ops/layer.js",
    "core.js"    => "../../runtime/core.js",
];

/// Every runtime module, for callers that need to publish the tree (the dev
/// server serves it so extern-mode imports resolve).
pub fn modules() -> &'static [Mod] {
    MODS
}

/// The binder each op code resolves to, and the module it lives in.
const BINDERS: [(u8, Caps, &str, &str); 12] = [
    (op::TRANSFORM, Caps::TRANSFORM, "bTransform", "ops/tx.js"),
    (op::TRANSLATE, Caps::TRANSLATE, "bTranslate", "ops/txt.js"),
    (op::OPACITY, Caps::OPACITY, "bOpacity", "ops/opacity.js"),
    (op::DISPLAY, Caps::DISPLAY, "bDisplay", "ops/display.js"),
    (op::SHAPE, Caps::SHAPE, "bShape", "ops/shape.js"),
    (op::RECT, Caps::RECT, "bRect", "ops/rect.js"),
    (op::ELLIPSE, Caps::ELLIPSE, "bEllipse", "ops/ellipse.js"),
    (op::FILL, Caps::FILL, "bFill", "ops/fill.js"),
    (op::STROKE, Caps::STROKE, "bStroke", "ops/stroke.js"),
    (op::GRADIENT, Caps::GRADIENT, "bGradient", "ops/grad.js"),
    (op::LAYER_TX, Caps::LAYER_TX, "bLayerTx", "ops/layer.js"),
    (op::LAYER_OP, Caps::LAYER_OP, "bLayerOpacity", "ops/layer.js"),
];

/// Roots of the reachability walk: the mount entry point, every binder the scene
/// actually uses, and every runtime function the emitted expression bodies call.
///
/// The expression helpers come in as roots rather than through `GATED`, because
/// the layer pass reports exactly which ones it wrote into the bodies. A root is
/// live whatever the capability gates say, so retention there is exact by
/// construction instead of inferred from a word scan of the finished text.
fn roots(caps: Caps, helpers: &[&'static str]) -> Vec<&'static str> {
    let mut r = vec!["mount"];
    if caps.contains(Caps::EXPRESSIONS) {
        r.push("makeExpr");
    }
    r.extend_from_slice(helpers);
    if caps.contains(Caps::TEMPLATES) {
        r.push("expand");
    }
    if caps.contains(Caps::EXTRACTED) {
        r.push("fromSprite");
    }
    if caps.contains(Caps::TIME_REMAP) {
        r.push("resolve");
    }
    for (_, cap, name, _) in BINDERS {
        if caps.contains(cap) {
            r.push(name);
        }
    }
    r
}

/// Every top-level declaration in the runtime, in dependency order.
fn all_declarations() -> Vec<shake::Decl> {
    MODS.iter().flat_map(|m| shake::declarations(m.src)).collect()
}

/// The runtime this scene needs, shaken down to reachable declarations.
fn bundle(caps: Caps, helpers: &[&'static str]) -> String {
    let kept = shake::shake(all_declarations(), &roots(caps, helpers), caps);
    let mut out = String::with_capacity(16384);
    for d in &kept {
        out.push_str(&d.text);
    }
    out
}

/// Names of the runtime declarations a scene retains. Reported in unminified
/// output so a review diff shows when a change starts (or stops) pulling
/// something in.
pub fn retained_symbols(caps: Caps, helpers: &[&'static str]) -> Vec<String> {
    shake::shake(all_declarations(), &roots(caps, helpers), caps)
        .into_iter()
        .map(|d| d.name)
        .collect()
}

/// The runtime a capability set pulls in, unminified — module comments and
/// all, in dependency order.
///
/// Capability-only, so the expression helpers a particular module's bodies call
/// are not part of the figure. That is what it is for: what a *feature* costs,
/// not what one animation ended up shipping.
pub fn runtime_pretty(caps: Caps) -> String {
    bundle(caps, &[])
}

/// Minified source of the runtime a capability set pulls in.
pub fn runtime_source(caps: Caps) -> String {
    let mut src = bundle(caps, &[]);
    src.push_str(&format!("export {{ {} }};\n", roots(caps, &[]).join(", ")));
    minify(&src).unwrap_or(src)
}

/// Minified size of the runtime a capability set pulls in. Used to report what
/// each optional feature costs.
pub fn runtime_size(caps: Caps) -> usize {
    runtime_size_with(caps, &[])
}

/// What one module's runtime actually weighs, helpers included.
///
/// The expression helpers are roots rather than capabilities, so a capability
/// set no longer describes the whole slice — measuring by caps alone reported
/// `lyAt`, `lyPos` and the space walks as free.
pub fn runtime_size_with(caps: Caps, helpers: &[&'static str]) -> usize {
    let mut src = bundle(caps, helpers);
    // Anchor the entry points before minifying. The shaker strips `export`
    // keywords, so without this the module has no exports and no side effects
    // and the minifier correctly deletes all of it — which reported every
    // runtime as 0 bytes, and so every feature as costing nothing.
    src.push_str(&format!("export {{ {} }};\n", roots(caps, helpers).join(", ")));
    minify(&src).unwrap_or(src).len()
}

/// The whole runtime, every capability on, unminified.
///
/// No compiled module imports this — output imports the entry points it binds
/// and a bundler assembles the rest. It exists so size reporting can show the
/// upper bound on what a page could ever load.
pub fn driver_source() -> String {
    let mut src = bundle(Caps::all(), EXPR_HELPERS);
    src.push_str(&binder_table(Caps::all()));
    src
}

/// Every runtime symbol the expression bodies can be rewritten to call, plus the
/// fallback surface. Only the "everything on" reports use this; a real module
/// takes the exact set the layer pass reports for it.
const EXPR_HELPERS: &[&str] = &[
    "lyAt", "lyRel", "lyParent", "lyPos", "lyAnchor", "lyScale", "lyRot",
    "lyOpacity", "lyPath", "lyPoints", "lyClosed", "lyEffect",
    "toComp", "fromCompToSurface",
];

/// Minified counterpart of [`driver_source`], for size reporting.
pub fn build_driver() -> String {
    let mut src = driver_source();
    // Every entry point, not just `mount`/`B`. The optional capabilities reach
    // the runtime through the `ext` argument rather than an import, so
    // exporting only `mount` let the minifier drop the expression engine,
    // template expansion and sprite sourcing — and the "all capabilities"
    // figure then understated the runtime by roughly 40%.
    src.push_str(&format!(
        "export {{ {}, B }};\n",
        roots(Caps::all(), EXPR_HELPERS).join(", ")
    ));
    minify(&src).unwrap_or(src)
}

// ---------------------------------------------------------------------------
// Module emission
// ---------------------------------------------------------------------------

/// The expression helpers a scene's bodies call, as shake roots.
fn helpers_of(exprs: Option<&Exprs>) -> Vec<&'static str> {
    exprs.map(|e| e.helpers.iter().copied().collect()).unwrap_or_default()
}

pub fn emit(
    scene: &Scene,
    mode: RuntimeMode,
    compress: bool,
    exprs: Option<&Exprs>,
    markup_mode: &MarkupMode,
) -> anyhow::Result<String> {
    if !compress {
        return readable(scene, mode, exprs, markup_mode);
    }

    let markup = js_string(&carried(scene, markup_mode));
    let mut src = String::with_capacity(scene.inline.len() + 4096);

    // Nothing varies over time: the module is the markup plus an inert player.
    // No runtime, no data table, no animation frame is ever scheduled.
    if scene.is_static() {
        src.push_str(&format!("const M={markup};\n"));
        src.push_str("export const markup=M;\n");
        src.push_str(&sprite_export(markup_mode));
        src.push_str(&static_player(scene, markup_mode));
        return Ok(minify(&src).unwrap_or(src));
    }

    // An animation the generator can express becomes code instead of data —
    // no payload, no binder table, no closure per property. It only ever
    // applies to self-contained modules: an extern one shares the interpreter
    // with every other animation on the page, so the trade runs the other way
    // round there.
    //
    // Both are built and the smaller wins, the same way `Instancing::Auto`
    // picks. Generated code is dramatically smaller for the animations the
    // runtime dominates and dramatically *larger* for one like `ripple`, where
    // 230 bindings each unroll — so which is better is a measurement, not a
    // rule, and it is cheap to just take it.
    let candidate = if mode == RuntimeMode::Embedded && matches!(markup_mode, MarkupMode::Inline) {
        codegen::try_emit(scene).map(|code| generated(scene, &markup, code, exprs))
    } else {
        None
    };

    let data = serde_json::to_string(&scene.data)?;

    let helpers = helpers_of(exprs);
    match mode {
        RuntimeMode::Extern => {
            src.push_str(&extern_imports(
                caps_of(scene, markup_mode),
                RUNTIME_BASE,
                &helpers,
            ));
            src.push_str(&binder_table(scene.caps));
        }
        RuntimeMode::Embedded => {
            src.push_str(&bundle(caps_of(scene, markup_mode), &helpers));
            src.push_str(&binder_table(scene.caps));
        }
    }
    src.push_str(&format!("const M={markup};\nconst D={data};\n"));
    // Factored-out subtrees are the module's own strings, not payload: naming
    // them here keeps them out of the pool, and the pool then has nothing left
    // in it at all for most animations.
    if !scene.data.tpl.is_empty() {
        let items: Vec<String> = scene.data.tpl.iter().map(|m| js_string(m)).collect();
        src.push_str(&format!("const TPL=[{}];\n", items.join(",")));
    }
    if !scene.data.strings.is_empty() {
        let items: Vec<String> = scene.data.strings.iter().map(|m| js_string(m)).collect();
        src.push_str(&format!("const SP=[{}];\n", items.join(",")));
    }
    if let Some(e) = exprs {
        src.push_str(&e.src);
    }
    src.push_str("export const markup=M;\n");
    src.push_str(&sprite_export(markup_mode));
    src.push_str(&format!(
        "export const init=(c,o)=>mount(M,D,B,c,o{});\n",
        extensions(scene.caps, &scene.data.tpl, !scene.data.strings.is_empty(), exprs, markup_mode)
    ));

    let interpreted = minify(&src).unwrap_or(src);
    Ok(match candidate {
        Some(g) if g.len() < interpreted.len() => g,
        _ => interpreted,
    })
}

/// Whether a self-contained build of this scene would be code-generated.
///
/// Answers the same question `emit` does — the generator can express it *and*
/// the result is smaller — without keeping both strings around.
pub fn is_generated(scene: &Scene, exprs: Option<&Exprs>) -> bool {
    if scene.is_static() {
        return false;
    }
    let markup = js_string(&carried(scene, &MarkupMode::Inline));
    match codegen::try_emit(scene) {
        Some(code) => {
            let g = generated(scene, &markup, code, exprs);
            let mut src = String::new();
            src.push_str(&bundle(
                caps_of(scene, &MarkupMode::Inline),
                &helpers_of(exprs),
            ));
            src.push_str(&binder_table(scene.caps));
            if let Ok(data) = serde_json::to_string(&scene.data) {
                src.push_str(&format!("const M={markup};\nconst D={data};\n"));
            }
            if !scene.data.tpl.is_empty() {
                let items: Vec<String> =
                    scene.data.tpl.iter().map(|m| js_string(m)).collect();
                src.push_str(&format!("const TPL=[{}];\n", items.join(",")));
            }
            if !scene.data.strings.is_empty() {
                let items: Vec<String> =
                    scene.data.strings.iter().map(|m| js_string(m)).collect();
                src.push_str(&format!("const SP=[{}];\n", items.join(",")));
            }
            if let Some(e) = exprs {
                src.push_str(&e.src);
            }
            src.push_str("export const markup=M;\n");
            src.push_str(&format!(
                "export const init=(c,o)=>mount(M,D,B,c,o{});\n",
                extensions(scene.caps, &scene.data.tpl, !scene.data.strings.is_empty(), exprs, &MarkupMode::Inline)
            ));
            g.len() < minify(&src).unwrap_or(src).len()
        }
        None => false,
    }
}

/// Assemble a generated module: the helpers it reaches, the markup, and an
/// `init` that builds `apply` in straight-line code and hands it to the shared
/// player.
///
/// The contrast with the interpreter path above is the point — there is no
/// payload string, no binder table, and no `mount`. What ships is this
/// animation and nothing else.
fn generated(
    scene: &Scene,
    markup: &str,
    code: codegen::Generated,
    exprs: Option<&Exprs>,
) -> String {
    let mut src = String::with_capacity(4096);

    // Only the helpers the generated body actually calls, plus the player.
    let mut roots = vec!["player"];
    if !scene.data.tpl.is_empty() {
        roots.push("expand");
    }
    roots.extend(code.needs.iter().copied());
    // The expression bodies name their own helpers; a generated module reaches
    // them from the body text alone, which the shaker cannot see.
    roots.extend(helpers_of(exprs));
    src.push_str(&shaken(&roots));

    // Easing handles hoisted to module scope: the solver takes them as an
    // argument, and an inline literal would allocate an array every frame.
    for (i, e) in code.easings.iter().enumerate() {
        src.push_str(&format!(
            "const Z{i}=[{},{},{},{}];\n",
            crate::scene::svg::n(e[0]),
            crate::scene::svg::n(e[1]),
            crate::scene::svg::n(e[2]),
            crate::scene::svg::n(e[3])
        ));
    }

    // Constant path outlines, hoisted so a keyframed shape interpolates
    // between objects that already exist.
    for (i, p) in code.paths.iter().enumerate() {
        src.push_str(&format!("const Q{i}={p};\n"));
    }
    // Arc-length and trim tables, built once when the module loads.
    src.push_str(&code.pre);

    if let Some(e) = exprs {
        src.push_str(&e.src);
    }
    // Factored-out subtrees ship as markup strings and are expanded before any
    // element is indexed — the planner numbered the *expanded* tree.
    if !scene.data.tpl.is_empty() {
        let items: Vec<String> = scene.data.tpl.iter().map(|m| js_string(m)).collect();
        src.push_str(&format!("const TPL=[{}];\n", items.join(",")));
    }
    src.push_str(&format!("const M={markup};\nexport const markup=M;\n"));
    // Two mounts of the same module must not share `<mask>`/gradient ids. The
    // interpreter keeps this counter in core.js, which a generated module does
    // not include.
    if scene.data.uses_ids {
        src.push_str("let uid=0;\n");
    }

    src.push_str("export const init=(c,o)=>{\n");
    src.push_str("o=o||{};\n");
    src.push_str(if scene.data.uses_ids {
        "const H=M.split('--u').join('-'+(uid++));\nc.innerHTML=H;\n"
    } else {
        "const H=M;\nc.innerHTML=H;\n"
    });
    src.push_str("const svg=c.firstElementChild;\n");
    if !scene.data.tpl.is_empty() {
        src.push_str("expand(svg,TPL);\n");
    }
    // Elements are addressed by document-order index, the same numbering the
    // planner assigned; only the bound ones are looked up.
    // Not `E`: that is the expression table's name, and the collision made
    // `initExpr(E, ctx)` receive the DOM node list.
    src.push_str("const NL=svg.querySelectorAll('*');\n");
    let binds: Vec<String> = code
        .els
        .iter()
        .enumerate()
        .map(|(i, idx)| format!("e{i}=NL[{idx}]"))
        .collect();
    src.push_str(&format!("const {};\n", binds.join(",")));
    // One slot per written attribute, holding the last value written. This is
    // what `attr()` allocated a closure for.
    if !code.slots.is_empty() {
        src.push_str(&format!("let {};\n", code.slots.join(",")));
    }
    if !code.decls.is_empty() {
        src.push_str(&format!("const {};\n", code.decls.join(",")));
    }
    if code.exprs {
        src.push_str("const ctx={frame:0,fr:");
        src.push_str(&crate::scene::svg::n(scene.data.fr));
        src.push_str("};\ninitExpr(E,ctx);\n");
    }
    src.push_str(&code.init);
    src.push_str("const apply=(f)=>{\n");
    if code.exprs {
        src.push_str("ctx.frame=f;\n");
    }
    src.push_str(&code.body);
    src.push_str("};\n");
    src.push_str(&format!(
        "return player(c,svg,H,apply,{},{},{},o);\n}};\n",
        crate::scene::svg::n(scene.data.fr),
        crate::scene::svg::n(scene.data.ip),
        crate::scene::svg::n(scene.data.op)
    ));

    minify(&src).unwrap_or(src)
}

/// The runtime declarations a set of roots reaches, with module syntax removed.
fn shaken(roots: &[&str]) -> String {
    let kept = shake::shake(all_declarations(), roots, Caps::all());
    let mut out = String::new();
    for d in &kept {
        out.push_str(&d.text);
    }
    out
}

/// The symbol a module sources its markup from, so a consumer can tell which
/// sprite entry it needs without parsing the module. Empty in inline mode.
fn sprite_export(markup_mode: &MarkupMode) -> String {
    match markup_mode {
        MarkupMode::Inline => String::new(),
        MarkupMode::Extracted(id) => {
            format!("export const spriteSymbol = {};\n", js_string(id))
        }
    }
}

/// The markup the module carries: the whole document, or — when it has been
/// extracted to a sprite — just the `<svg>` shell the runtime fills.
fn carried(scene: &Scene, markup_mode: &MarkupMode) -> String {
    match markup_mode {
        MarkupMode::Inline => scene.inline.clone(),
        MarkupMode::Extracted(_) => crate::scene::shell(&scene.inline),
    }
}

/// The scene's capabilities plus whatever the delivery mode needs. Extraction
/// is a property of how the module ships, not of the animation, so the planner
/// never sees it.
fn caps_of(scene: &Scene, markup_mode: &MarkupMode) -> Caps {
    match markup_mode {
        MarkupMode::Inline => scene.caps,
        MarkupMode::Extracted(_) => scene.caps | Caps::EXTRACTED,
    }
}

/// The inert player handed back for an animation with nothing to animate.
///
/// It imports nothing, including in extracted mode: cloning a symbol's
/// children is four lines, and a module whose entire point is that it needs no
/// runtime should not acquire a dependency to fetch its own picture.
fn static_player(scene: &Scene, markup_mode: &MarkupMode) -> String {
    let fill = match markup_mode {
        MarkupMode::Inline => String::new(),
        MarkupMode::Extracted(id) => format!(
            "const s=document.getElementById({});\
             if(!s)throw new Error('ulottie: sprite symbol {} is not in the document');\
             for(const n of s.children)c.firstElementChild.appendChild(n.cloneNode(true));",
            js_string(id),
            id,
        ),
    };
    format!(
        "export const init=(c)=>{{c.innerHTML=M;{fill}const p={{svg:c.firstElementChild,markup:M,\
         totalFrames:{},frameRate:{},duration:{},currentFrame:{},isPlaying:false,\
         destroy(){{c.innerHTML=''}}}};\
         for(const k of ['play','pause','stop','seek','goToFrame','goToAndStop','goToAndPlay','on','off'])p[k]=()=>p;\
         return p}};\n",
        fmt(scene.data.op - scene.data.ip),
        fmt(scene.data.fr),
        fmt((scene.data.op - scene.data.ip) / scene.data.fr.max(1.0)),
        fmt(scene.data.ip),
    )
}

/// The same module, printed for review: one element per line, one binding per
/// line. Only whitespace differs from what `emit` produces.
fn readable(
    scene: &Scene,
    mode: RuntimeMode,
    exprs: Option<&Exprs>,
    markup_mode: &MarkupMode,
) -> anyhow::Result<String> {
    let mut src = String::with_capacity(scene.inline.len() * 2 + 4096);

    src.push_str(&format!(
        "// Generated by ulottie — unminified for review; not what ships.\n\
// Object keys are sorted here for stable diffs; emitted order differs.\n\
// mode: {}{}\n\
// caps: {}\n\n",
        match mode {
            RuntimeMode::Extern => "extern",
            RuntimeMode::Embedded => "embedded",
        },
        match (scene.caps.contains(Caps::INSTANCES), markup_mode) {
            (true, MarkupMode::Extracted(_)) => ", precomps instanced, markup extracted",
            (true, MarkupMode::Inline) => ", precomps instanced",
            (false, MarkupMode::Extracted(_)) => ", markup extracted",
            (false, MarkupMode::Inline) => "",
        },
        caps_list(caps_of(scene, markup_mode)),
    ));

    if scene.is_static() {
        src.push_str("// Fully static after compilation: no runtime, no data table, no frame loop.\n");
    }

    let helpers = helpers_of(exprs);
    if !scene.is_static() {
        match mode {
            RuntimeMode::Extern => {
                src.push_str(&extern_imports(
                    caps_of(scene, markup_mode),
                    RUNTIME_BASE,
                    &helpers,
                ));
                src.push_str(&binder_table(scene.caps));
                src.push('\n');
            }
            RuntimeMode::Embedded => {
                src.push_str(&format!(
                    "// runtime symbols: {}\n",
                    retained_symbols(caps_of(scene, markup_mode), &helpers).join(", ")
                ));
                src.push_str(&bundle(caps_of(scene, markup_mode), &helpers));
                src.push_str(&binder_table(scene.caps));
                src.push('\n');
            }
        }
    }

    if let MarkupMode::Extracted(id) = markup_mode {
        src.push_str(&format!(
            "// The document lives in an external sprite as `<symbol id=\"{id}\">`; the\n\
             // module carries only the shell and clones the symbol's children into it.\n"
        ));
    }
    src.push_str("const M =\n");
    src.push_str(pretty::markup(&carried(scene, markup_mode), js_string).trim_end());
    src.push_str(";\n\n");

    if !scene.is_static() {
        let value = serde_json::to_value(&scene.data)?;
        src.push_str("const D = ");
        src.push_str(&pretty::json(&value, 0));
        src.push_str(";\n\n");
    }

    // The module's own strings, named rather than interned — see the emitter's
    // counterpart above.
    if !scene.data.tpl.is_empty() {
        src.push_str("const TPL = [\n");
        for m in &scene.data.tpl {
            src.push_str(&format!("  {},\n", js_string(m)));
        }
        src.push_str("];\n\n");
    }
    if !scene.data.strings.is_empty() {
        src.push_str("const SP = [\n");
        for m in &scene.data.strings {
            src.push_str(&format!("  {},\n", js_string(m)));
        }
        src.push_str("];\n\n");
    }

    if let Some(e) = exprs {
        src.push_str(&e.src);
        src.push('\n');
    }
    src.push_str("export const markup = M;\n");
    if let MarkupMode::Extracted(_) = markup_mode {
        src.push_str(
            "// `markup` is only the shell in this mode. Server-rendering the\n\
             // animation means emitting the sprite's symbol body inside it —\n\
             // `compile_document()` returns that assembled document directly.\n",
        );
    }
    src.push_str(&sprite_export(markup_mode));
    if scene.is_static() {
        src.push_str(&static_player(scene, markup_mode));
    } else {
        src.push_str(&format!(
            "export const init = (c, o) => mount(M, D, B, c, o{});\n",
            extensions(scene.caps, &scene.data.tpl, !scene.data.strings.is_empty(), exprs, markup_mode)
        ));
    }
    Ok(src)
}

/// Human-readable capability list, so a review diff shows when a change made an
/// animation stop needing (or start needing) a runtime feature.
fn caps_list(caps: Caps) -> String {
    let names: Vec<&str> = caps.iter_names().map(|(n, _)| n).collect();
    if names.is_empty() { "none".into() } else { names.join(" | ") }
}

/// The optional-capability argument to `mount`, if any. Passing these rather
/// than importing them from `core.js` is what keeps an animation from pulling
/// in code it does not use — a reference inside `core.js` would survive into
/// every module graph.
fn extensions(
    caps: Caps,
    tpl: &[String],
    pool: bool,
    exprs: Option<&Exprs>,
    markup_mode: &MarkupMode,
) -> String {
    let mut parts = Vec::new();
    if let MarkupMode::Extracted(id) = markup_mode {
        parts.push(format!("s:fromSprite({})", js_string(id)));
    }
    // Gated on the templates themselves, not on the capability: `TPL` is only
    // declared when there is something to put in it, and the two conditions
    // drifting apart is a module that throws at `init`.
    if !tpl.is_empty() {
        debug_assert!(caps.contains(Caps::TEMPLATES));
        parts.push("t:s=>expand(s,TPL)".to_string());
    }
    if pool {
        parts.push("p:SP".to_string());
    }
    if caps.contains(Caps::TIME_REMAP) {
        parts.push("r:resolve".to_string());
    }
    // Every layer reference is a literal slot, so the engine needs no lookup
    // tables to place records into.
    if exprs.is_some() {
        parts.push("x:v=>makeExpr(E,v)".to_string());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(",{{{}}}", parts.join(","))
    }
}

/// Where extern-mode output imports the runtime from. Relative so a compiled
/// module can sit next to a copy of `runtime/`; a bundler resolves these to the
/// real ES modules and tree-shakes the rest away on its own.
pub const RUNTIME_BASE: &str = "./runtime";

/// Import exactly the runtime entry points this scene binds — not an aggregate
/// driver. A bundler then sees a normal module graph and drops everything the
/// animation never reaches, the same way the embedded path does.
/// Specifiers an extern build imports, in emission order. Reported by the size
/// panel so a reader can see that an animation pulls four files, not a runtime.
pub fn imported_modules(caps: Caps) -> Vec<String> {
    extern_imports(caps, ".", &[])
        .lines()
        .filter_map(|l| l.split_once("from './").map(|(_, r)| r))
        .filter_map(|r| r.split_once('\'').map(|(m, _)| m.to_string()))
        .collect()
}

fn extern_imports(caps: Caps, base: &str, helpers: &[&'static str]) -> String {
    let mut out = format!("import {{ mount }} from '{base}/core.js';\n");
    if caps.contains(Caps::EXTRACTED) {
        out.push_str(&format!("import {{ fromSprite }} from '{base}/sprite.js';\n"));
    }
    if caps.contains(Caps::EXPRESSIONS) {
        // The bodies call the layer helpers by bare name, so an extern build has
        // to import each one it uses alongside the engine itself. Not the ones
        // a body destructures out of `ctx` — those reach it through the engine
        // and are not module bindings at the call site.
        let mut names = vec!["makeExpr"];
        names.extend(
            helpers
                .iter()
                .copied()
                .filter(|h| !super::emit_expressions::CTX_NAMES.contains(h)),
        );
        out.push_str(&format!(
            "import {{ {} }} from '{base}/expr.js';\n",
            names.join(", ")
        ));
    }
    if caps.contains(Caps::TIME_REMAP) {
        out.push_str(&format!("import {{ resolve }} from '{base}/kf.js';\n"));
    }
    if caps.contains(Caps::TEMPLATES) {
        out.push_str(&format!("import {{ expand }} from '{base}/tpl.js';\n"));
    }
    for (_, cap, name, module) in BINDERS {
        if caps.contains(cap) {
            out.push_str(&format!("import {{ {name} }} from '{base}/{module}';\n"));
        }
    }
    out
}

/// Sparse binder table holding only the ops this scene uses. Array holes keep
/// the indices aligned with `scene::op` without naming the absent binders.
fn binder_table(caps: Caps) -> String {
    let last = BINDERS
        .iter()
        .filter(|(_, c, _, _)| caps.contains(*c))
        .map(|(i, _, _, _)| *i)
        .max()
        .unwrap_or(0);
    let mut parts = Vec::new();
    for (idx, cap, name, _) in BINDERS.iter() {
        if *idx > last {
            break;
        }
        parts.push(if caps.contains(*cap) { *name } else { "" });
    }
    format!("const B=[{}];\n", parts.join(","))
}

/// Shortest round-trip float, matching what the planner writes into markup.
fn fmt(v: f64) -> String {
    crate::scene::svg::n(v)
}

/// Single-quoted JS string literal. Generated markup only ever uses double
/// quotes for attributes, so the common case needs no escaping at all.
fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundling_drops_module_syntax() {
        let out = bundle(Caps::all(), EXPR_HELPERS);
        assert!(!out.contains("import "), "bundle still has an import");
        assert!(!out.contains("export "), "bundle still has an export");
    }

    #[test]
    fn a_scene_only_carries_what_it_reaches() {
        let translate_only = retained_symbols(Caps::TRANSLATE, &[]);
        assert!(translate_only.contains(&"bTranslate".to_string()));
        assert!(!translate_only.contains(&"bGradient".to_string()));
        assert!(!translate_only.contains(&"trimApply".to_string()));
        assert!(!translate_only.contains(&"spatial".to_string()));
        // `r` formats plain coordinates; a translate binder uses r2/r5 only.
        assert!(!translate_only.contains(&"r".to_string()));

        let trimmed = retained_symbols(Caps::SHAPE | Caps::TRIM, &[]);
        assert!(trimmed.contains(&"trimApply".to_string()));
        assert!(trimmed.contains(&"pathD".to_string()));
    }

    #[test]
    fn extern_imports_name_each_binder_module() {
        let imports = extern_imports(Caps::TRANSLATE | Caps::FILL, "./runtime", &[]);
        assert_eq!(
            imports,
            "import { mount } from './runtime/core.js';\n\
             import { bTranslate } from './runtime/ops/txt.js';\n\
             import { bFill } from './runtime/ops/fill.js';\n"
        );
    }

    #[test]
    fn binder_table_leaves_holes_for_absent_ops() {
        assert_eq!(binder_table(Caps::TRANSLATE), "const B=[,bTranslate];\n");
        assert_eq!(
            binder_table(Caps::TRANSFORM | Caps::OPACITY),
            "const B=[bTransform,,bOpacity];\n"
        );
    }

    #[test]
    fn markup_needs_no_escaping_in_the_common_case() {
        assert_eq!(js_string(r#"<svg a="1"/>"#), r#"'<svg a="1"/>'"#);
        assert_eq!(js_string("id--u"), "'id--u'");
    }

    #[test]
    fn the_driver_bundles_and_minifies() {
        let d = build_driver();
        assert!(!d.is_empty());
        assert!(d.contains("mount"));
    }
}
