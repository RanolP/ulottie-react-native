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

/// Roots of the reachability walk: the mount entry point plus every binder the
/// scene actually uses.
fn roots(caps: Caps) -> Vec<&'static str> {
    let mut r = vec!["mount"];
    if caps.contains(Caps::EXPRESSIONS) {
        r.push("makeExpr");
    }
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
fn bundle(caps: Caps) -> String {
    let kept = shake::shake(all_declarations(), &roots(caps), caps);
    let mut out = String::with_capacity(16384);
    for d in &kept {
        out.push_str(&d.text);
    }
    out
}

/// Names of the runtime declarations a scene retains. Reported in unminified
/// output so a review diff shows when a change starts (or stops) pulling
/// something in.
pub fn retained_symbols(caps: Caps) -> Vec<String> {
    shake::shake(all_declarations(), &roots(caps), caps)
        .into_iter()
        .map(|d| d.name)
        .collect()
}

/// Minified source of the runtime a capability set pulls in.
pub fn runtime_source(caps: Caps) -> String {
    let mut src = bundle(caps);
    src.push_str(&format!("export {{ {} }};\n", roots(caps).join(", ")));
    minify(&src).unwrap_or(src)
}

/// Minified size of the runtime a capability set pulls in. Used to report what
/// each optional feature costs.
pub fn runtime_size(caps: Caps) -> usize {
    let mut src = bundle(caps);
    // Anchor the entry points before minifying. The shaker strips `export`
    // keywords, so without this the module has no exports and no side effects
    // and the minifier correctly deletes all of it — which reported every
    // runtime as 0 bytes, and so every feature as costing nothing.
    src.push_str(&format!("export {{ {} }};\n", roots(caps).join(", ")));
    minify(&src).unwrap_or(src).len()
}

/// The whole runtime, every capability on, unminified.
///
/// No compiled module imports this — output imports the entry points it binds
/// and a bundler assembles the rest. It exists so size reporting can show the
/// upper bound on what a page could ever load.
pub fn driver_source() -> String {
    let mut src = bundle(Caps::all());
    src.push_str(&binder_table(Caps::all()));
    src
}

/// Minified counterpart of [`driver_source`], for size reporting.
pub fn build_driver() -> String {
    let mut src = driver_source();
    // Every entry point, not just `mount`/`B`. The optional capabilities reach
    // the runtime through the `ext` argument rather than an import, so
    // exporting only `mount` let the minifier drop the expression engine,
    // template expansion and sprite sourcing — and the "all capabilities"
    // figure then understated the runtime by roughly 40%.
    src.push_str(&format!("export {{ {}, B }};\n", roots(Caps::all()).join(", ")));
    minify(&src).unwrap_or(src)
}

// ---------------------------------------------------------------------------
// Module emission
// ---------------------------------------------------------------------------

pub fn emit(
    scene: &Scene,
    mode: RuntimeMode,
    compress: bool,
    exprs: Option<&str>,
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

    let data = serde_json::to_string(&scene.data)?;

    match mode {
        RuntimeMode::Extern => {
            src.push_str(&extern_imports(caps_of(scene, markup_mode), RUNTIME_BASE));
            src.push_str(&binder_table(scene.caps));
        }
        RuntimeMode::Embedded => {
            src.push_str(&bundle(caps_of(scene, markup_mode)));
            src.push_str(&binder_table(scene.caps));
        }
    }
    src.push_str(&format!("const M={markup};\nconst D={data};\n"));
    if let Some(e) = exprs {
        src.push_str(e);
    }
    src.push_str("export const markup=M;\n");
    src.push_str(&sprite_export(markup_mode));
    src.push_str(&format!(
        "export const init=(c,o)=>mount(M,D,B,c,o{});\n",
        extensions(scene.caps, exprs.is_some(), markup_mode)
    ));

    Ok(minify(&src).unwrap_or(src))
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
    exprs: Option<&str>,
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

    if !scene.is_static() {
        match mode {
            RuntimeMode::Extern => {
                src.push_str(&extern_imports(caps_of(scene, markup_mode), RUNTIME_BASE));
                src.push_str(&binder_table(scene.caps));
                src.push('\n');
            }
            RuntimeMode::Embedded => {
                src.push_str(&format!(
                    "// runtime symbols: {}\n",
                    retained_symbols(caps_of(scene, markup_mode)).join(", ")
                ));
                src.push_str(&bundle(caps_of(scene, markup_mode)));
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

    if let Some(e) = exprs {
        src.push_str(e);
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
            extensions(scene.caps, exprs.is_some(), markup_mode)
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
fn extensions(caps: Caps, has_exprs: bool, markup_mode: &MarkupMode) -> String {
    let mut parts = Vec::new();
    if let MarkupMode::Extracted(id) = markup_mode {
        parts.push(format!("s:fromSprite({})", js_string(id)));
    }
    if caps.contains(Caps::TEMPLATES) {
        parts.push("t:expand".to_string());
    }
    if caps.contains(Caps::TIME_REMAP) {
        parts.push("r:resolve".to_string());
    }
    if has_exprs {
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
    extern_imports(caps, ".")
        .lines()
        .filter_map(|l| l.split_once("from './").map(|(_, r)| r))
        .filter_map(|r| r.split_once('\'').map(|(m, _)| m.to_string()))
        .collect()
}

fn extern_imports(caps: Caps, base: &str) -> String {
    let mut out = format!("import {{ mount }} from '{base}/core.js';\n");
    if caps.contains(Caps::EXTRACTED) {
        out.push_str(&format!("import {{ fromSprite }} from '{base}/sprite.js';\n"));
    }
    if caps.contains(Caps::EXPRESSIONS) {
        out.push_str(&format!("import {{ makeExpr }} from '{base}/expr.js';\n"));
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
        let out = bundle(Caps::all());
        assert!(!out.contains("import "), "bundle still has an import");
        assert!(!out.contains("export "), "bundle still has an export");
    }

    #[test]
    fn a_scene_only_carries_what_it_reaches() {
        let translate_only = retained_symbols(Caps::TRANSLATE);
        assert!(translate_only.contains(&"bTranslate".to_string()));
        assert!(!translate_only.contains(&"bGradient".to_string()));
        assert!(!translate_only.contains(&"trimApply".to_string()));
        assert!(!translate_only.contains(&"spatial".to_string()));
        // `r` formats plain coordinates; a translate binder uses r2/r5 only.
        assert!(!translate_only.contains(&"r".to_string()));

        let trimmed = retained_symbols(Caps::SHAPE | Caps::TRIM);
        assert!(trimmed.contains(&"trimApply".to_string()));
        assert!(trimmed.contains(&"pathD".to_string()));
    }

    #[test]
    fn extern_imports_name_each_binder_module() {
        let imports = extern_imports(Caps::TRANSLATE | Caps::FILL, "./runtime");
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
