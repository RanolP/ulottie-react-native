//! Data-driven backend.
//!
//! Lowers an `ir::Module` to a JS module exporting `markup` and `init`.
//!
//! `RuntimeMode::Extern` imports the runtime entry points the animation binds;
//! `RuntimeMode::Embedded` inlines them, shaken to the reachable declarations.

pub mod emit;
pub mod emit_expressions;
pub mod pretty;
pub mod runtime;
pub mod shake;

use anyhow::Result;

use crate::data;
use crate::ir;
use crate::scene;

/// Layer names the emitted expressions can still reach by name, or `None` when
/// that cannot be determined and every name has to be kept.
///
/// The planner interns a name for every layer, but the table exists solely so
/// `thisComp.layer('…')` can find one — and most animations never call it. On
/// `starfish` none of the 16 interned names is reachable.
///
/// This is deliberately a text scan with a wide bail-out rather than a real
/// analysis: a name that is dropped but turns out to be reachable fails by
/// returning `undefined` from `thisComp.layer()`, which surfaces as a wrong
/// picture rather than an error. Anything the scan does not fully understand
/// keeps the whole table.
fn reachable_names(exprs: &[ir::Expression]) -> Option<std::collections::BTreeSet<String>> {
    let mut found = std::collections::BTreeSet::new();
    for e in exprs {
        let b = e.body.as_str();
        // `.name` can hand a name to any comparison the scan cannot follow.
        if b.contains(".name") {
            return None;
        }
        let bytes = b.as_bytes();
        for (i, _) in b.match_indices("layer(") {
            // `pathLayer(`/`nullLayerNames` are different identifiers; require
            // a word boundary so only a real `layer(` call is considered.
            if i > 0 && (bytes[i - 1] as char).is_alphanumeric() {
                continue;
            }
            let rest = &b[i + "layer(".len()..];
            let arg = rest.trim_start();
            let quote = match arg.chars().next() {
                Some(q @ ('\'' | '"')) => q,
                // A numeric index does not consult the name table.
                Some(c) if c.is_ascii_digit() => continue,
                // Anything computed: give up on the whole table.
                _ => return None,
            };
            match arg[1..].split_once(quote) {
                Some((name, _)) => {
                    found.insert(name.to_string());
                }
                None => return None,
            }
        }
    }
    Some(found)
}

#[cfg(test)]
mod name_scan_tests {
    use super::reachable_names;
    use crate::ir;

    fn expr(body: &str) -> ir::Expression {
        ir::Expression {
            id: ir::ExprId(0),
            body: body.to_string(),
            canonical_hash: 0,
            used_apis: Default::default(),
            uses_value: false,
            uses_this_property: false,
            uses_loop_out: false,
            references_layers: Vec::new(),
            references_effects: Vec::new(),
        }
    }

    fn names(bodies: &[&str]) -> Option<Vec<String>> {
        let e: Vec<_> = bodies.iter().map(|b| expr(b)).collect();
        reachable_names(&e).map(|s| s.into_iter().collect())
    }

    #[test]
    fn collects_literal_lookups() {
        assert_eq!(
            names(&["thisComp.layer('Shape Layer 1').position"]),
            Some(vec!["Shape Layer 1".to_string()])
        );
        // Either quote style, and several per body.
        assert_eq!(
            names(&[r#"comp("a").layer("x"); thisComp.layer('y')"#]),
            Some(vec!["x".to_string(), "y".to_string()])
        );
    }

    #[test]
    fn a_computed_argument_keeps_every_name() {
        // The whole point of the bail-out: the scan cannot tell which name
        // `names[i]` produces, so nothing may be dropped.
        assert_eq!(names(&["thisComp.layer(names[i])"]), None);
        assert_eq!(names(&["thisComp.layer(n)"]), None);
    }

    #[test]
    fn reading_a_layers_name_keeps_every_name() {
        // `.name` can feed a comparison the scan cannot follow.
        assert_eq!(names(&["if (thisLayer.name === 'x') return 1"]), None);
    }

    #[test]
    fn a_numeric_index_does_not_consult_the_table() {
        // `layer(3)` resolves by index, so it justifies keeping no names.
        assert_eq!(names(&["thisComp.layer(3)"]), Some(Vec::new()));
    }

    #[test]
    fn other_identifiers_ending_in_layer_are_not_lookups() {
        // `pathLayer('ADBE Root Vectors Group')` is an AE property drill, not a
        // layer lookup, and its argument is not a layer name.
        assert_eq!(names(&["pathLayer('ADBE Root Vectors Group')(1)"]), Some(Vec::new()));
    }
}

/// What the compiler decided for one animation, alongside the module it built.
///
/// Every field is a decision the AOT stage made, not a property of the source —
/// which is exactly what is worth reporting: two animations of the same size
/// can compile to a static string and to a 230-binding instanced scene.
pub struct Report {
    pub js: String,
    /// Capability names, in bit order — the runtime features this animation
    /// actually reaches.
    pub caps: Vec<String>,
    /// Runtime modules an extern build imports. Empty for a static animation.
    pub modules: Vec<String>,
    /// Minified bytes of just those modules — the slice a bundler ships, as
    /// opposed to the whole-runtime ceiling.
    pub runtime_slice: usize,
    /// Nothing varies over time: no runtime, no data table, no frame loop.
    pub is_static: bool,
    /// Precomps were planned once and replayed per use.
    pub instanced: bool,
    /// Repeated subtrees were factored out and are expanded at mount.
    pub templated: bool,
    /// Elements in the fully-expanded document.
    pub elements: usize,
    /// Per-frame updaters the runtime will build, counting instance replays.
    pub bindings: usize,
    /// Layer records the expression engine can observe.
    pub records: usize,
    /// Instancing put bindings on per-instance clocks. Those are correct but
    /// currently expensive — see `Instancing::Auto`.
    pub instance_clocks: bool,
}

pub fn compile(module: &ir::Module, options: &crate::CompileOptions) -> Result<Option<String>> {
    Ok(report(module, options)?.map(|r| r.js))
}

/// Facts about a finished scene, for the size/decision panel.
fn describe(scene: &scene::Scene, js: String, instanced: bool) -> Report {
    let caps: Vec<String> = scene.caps.iter_names().map(|(n, _)| n.to_string()).collect();
    let modules = if scene.is_static() {
        Vec::new()
    } else {
        emit::imported_modules(scene.caps)
    };
    let replays: usize = scene
        .data
        .uses
        .iter()
        .map(|u| scene.data.assets[u.asset as usize].bindings.len())
        .sum();
    let instance_clocks = scene
        .data
        .uses
        .iter()
        .any(|u| u.parent_slot != 0 || scene.data.assets[u.asset as usize].slots.iter().any(|s| *s != 0));
    Report {
        caps,
        instance_clocks,
        runtime_slice: if scene.is_static() { 0 } else { emit::runtime_size(scene.caps) },
        modules,
        is_static: scene.is_static(),
        instanced,
        templated: !scene.data.tpl.is_empty(),
        elements: scene.markup.matches('<').count() - scene.markup.matches("</").count(),
        bindings: scene.data.b.len() + replays,
        records: scene.data.layers.len()
            + scene.data.assets.iter().map(|a| a.records.len()).sum::<usize>(),
        js,
    }
}

pub fn report(module: &ir::Module, options: &crate::CompileOptions) -> Result<Option<Report>> {
    let runtime_mode = options.runtime_mode;
    if !data::can_encode(module) {
        return Ok(None);
    }
    let payload = data::encode(module)?;

    // Every animation goes through the scene planner. Expressions ride along
    // as `Prop::Expr` bindings plus a layer table the expression runtime reads.
    let has_exprs = !module.expressions.is_empty();
    let exprs = has_exprs.then(|| {
        let mut src = String::from("const E = [\n");
        for expr in module.expressions.iter() {
            emit_expressions::emit_one(&mut src, expr);
        }
        src.push_str("];\n");
        src
    });

    let reach = has_exprs
        .then(|| reachable_names(&module.expressions.iter().cloned().collect::<Vec<_>>()))
        .flatten();

    let build = |instance| -> Result<Report> {
        let mut scene =
            scene::plan_with(&payload, has_exprs, options.inline_limit, instance)?;
        // Drop interned layer names no expression can reach, and renumber.
        if let Some(reach) = &reach {
            scene.prune_names(reach);
        }
        let js = emit::emit(
            &scene,
            runtime_mode,
            options.minify,
            exprs.as_deref(),
            &options.markup,
        )?;
        Ok(describe(&scene, js, instance))
    };

    // Extracted markup and precomp instancing do not compose. The sprite holds
    // the fully-expanded document, but an instanced module binds against a tree
    // the runtime builds by expanding placeholders — a different, larger
    // element layout (ripple: 786 elements vs the 869 the bindings address). It
    // could be made to work by putting placeholders in the sprite, but then the
    // sprite is no longer a renderable picture, which is the point of the mode.
    let extracted = matches!(options.markup, crate::MarkupMode::Extracted(_));
    if extracted && options.instance_precomps == crate::Instancing::Always {
        anyhow::bail!(
            "--instance-precomps cannot be combined with --extract: an instanced module \
             binds against a tree expanded at runtime, so the extracted sprite would not \
             match it. Drop one of the two."
        );
    }

    let out = match options.instance_precomps {
        _ if extracted => build(false)?,
        crate::Instancing::Always => build(true)?,
        crate::Instancing::Never => build(false)?,
        // Only animations with reusable precomps can differ, so the second
        // build is skipped entirely for everything else.
        crate::Instancing::Auto if !scene::has_reusable_precomps(&payload) => build(false)?,
        crate::Instancing::Auto => {
            let (inlined, instanced) = (build(false)?, build(true)?);
            // Smaller is not enough: replaying an asset whose bindings run on
            // per-instance clocks is measurably slower per frame than the
            // inlined form (`ripple`: 0.73 ms vs 0.14 ms, with lottie-web at
            // 0.31 ms). Until that is understood, take the size win only when
            // it does not buy a frame-time regression.
            let smaller = compressed_len(&instanced.js) < compressed_len(&inlined.js);
            if smaller && !instanced.instance_clocks {
                instanced
            } else {
                inlined
            }
        }
    };
    Ok(Some(out))
}

/// Compressed length, for choosing between two encodings of the same animation.
///
/// The choice has to be made on compressed bytes: instancing replaces inlined
/// copies with a template plus expansion code, and gzip already deduplicates
/// copies that fit in its 32 KiB window. `precomp_star_circle` is 34% smaller
/// raw when instanced and 19% *larger* gzipped — raw bytes pick the wrong build.
#[cfg(feature = "auto-instancing")]
fn compressed_len(src: &str) -> usize {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    if e.write_all(src.as_bytes()).is_err() {
        return src.len();
    }
    e.finish().map(|v| v.len()).unwrap_or(src.len())
}

/// Fallback when the crate is built without a compressor (the wasm bundle
/// deliberately omits one). Deflate cannot match a repeat further back than
/// 32 KiB, so a module below that is already deduplicated by the compressor
/// and has nothing to gain — which is the same crossover the measured path
/// finds on the fixture corpus.
#[cfg(not(feature = "auto-instancing"))]
fn compressed_len(src: &str) -> usize {
    if src.len() > 32 * 1024 { src.len() } else { usize::MAX - src.len() }
}