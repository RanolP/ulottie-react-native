//! Data-driven backend.
//!
//! Lowers an `ir::Module` to a JS module exporting `markup` and `init`.
//!
//! `RuntimeMode::Extern` imports the runtime entry points the animation binds;
//! `RuntimeMode::Embedded` inlines them, shaken to the reachable declarations.

pub mod codegen;
pub mod emit;
pub mod emit_expressions;
pub mod layers;
pub mod pretty;
pub mod rn;
pub mod rt;
pub mod runtime;
pub mod shake;
pub mod skia;

use anyhow::Result;

/// Whether the compiler should explain the decisions it makes.
///
/// `ULOTTIE_WHY=1` turns on the running commentary the backends emit when they
/// decline something — which binding defeated the generator, which candidate
/// won on size, which construct sent an expression to the fallback.
pub(crate) fn why() -> bool {
    std::env::var("ULOTTIE_WHY").is_ok()
}

use crate::data;
use crate::ir;
use crate::scene;

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
    /// The self-contained build is code rather than an interpreter plus a
    /// payload. False when the generator cannot express the animation, and also
    /// when it can but the result would be larger — `ripple` unrolls 230
    /// bindings into three times the interpreter's bytes.
    pub generated: bool,
    /// Instancing put bindings on per-instance clocks. Those are correct but
    /// currently expensive — see `Instancing::Auto`.
    pub instance_clocks: bool,
    /// Images extracted from the markup into URL references, when the options
    /// asked for it (`CompileOptions::assets`). Deterministic per payload, so
    /// the two instancing candidates extract the same set.
    pub assets: Vec<scene::assets::ExtractedAsset>,
}

pub fn compile(module: &ir::Module, options: &crate::CompileOptions) -> Result<Option<String>> {
    Ok(report(module, options)?.map(|r| r.js))
}

/// Facts about a finished scene, for the size/decision panel.
fn describe(
    scene: &scene::Scene,
    js: String,
    instanced: bool,
    exprs: Option<&layers::Exprs>,
) -> Report {
    let mut caps: Vec<String> = scene
        .caps
        .iter_names()
        .map(|(n, _)| n.to_string())
        .collect();
    let mut modules = if scene.is_static() {
        Vec::new()
    } else {
        emit::imported_modules(scene.caps)
    };

    // Note: `iter_names()` and `imported_modules()` report in bit and emission order,
    // which carries meaning for the compiler but reads as arbitrary in the panel that lists them.
    caps.sort_unstable();
    modules.sort_unstable();

    let replays: usize = scene
        .data
        .uses
        .iter()
        .map(|u| scene.data.assets[u.asset as usize].bindings.len())
        .sum();
    let instance_clocks = scene.data.uses.iter().any(|u| {
        u.parent_slot != 0
            || scene.data.assets[u.asset as usize]
                .slots
                .iter()
                .any(|s| *s != 0)
    });
    Report {
        caps,
        instance_clocks,
        runtime_slice: if scene.is_static() {
            0
        } else {
            let helpers: Vec<&'static str> = exprs
                .map(|e| e.helpers.iter().copied().collect())
                .unwrap_or_default();
            emit::runtime_size_with(scene.caps, &helpers)
        },
        modules,
        is_static: scene.is_static(),
        instanced,
        templated: !scene.data.tpl.is_empty(),
        elements: scene.markup.matches('<').count() - scene.markup.matches("</").count(),
        bindings: scene.data.b.len() + replays,
        generated: emit::is_generated(scene, exprs),
        records: scene.data.layers.len()
            + scene
                .data
                .assets
                .iter()
                .map(|a| a.records.len())
                .sum::<usize>(),
        js,
        assets: Vec::new(),
    }
}

pub fn report(module: &ir::Module, options: &crate::CompileOptions) -> Result<Option<Report>> {
    if options.target == crate::Target::ReanimatedAot {
        return rn::report(module, options);
    }
    if options.target == crate::Target::SkiaAot {
        return skia::report(module, options);
    }
    if options.target == crate::Target::Rt {
        return rt::report(module, options);
    }
    let runtime_mode = options.runtime_mode;
    if !data::can_encode(module) {
        return Ok(None);
    }
    let payload = data::encode(module)?;

    // Every animation goes through the scene planner. Expressions ride along
    // as `Prop::Expr` bindings plus a layer table the expression runtime reads.
    let has_exprs = !module.expressions.is_empty();
    let bodies: Vec<ir::Expression> = module.expressions.iter().cloned().collect();

    let build = |instance| -> Result<Report> {
        let mut scene =
            scene::plan_with(&payload, has_exprs, options.inline_limit, instance, &bodies)?;

        // Asset extraction runs on the planned scene before any consumer reads
        // its markup, so the module `M`, the codegen `M` and every report
        // count all see the same URL references. Image layers are pure markup
        // — nothing in the wire stream mentions them — so no re-seal.
        let assets = if options.assets.extract {
            scene::assets::extract(&mut scene, &options.assets)
        } else {
            Vec::new()
        };

        // The layer pass runs *here*, not once for the module, because it
        // resolves references to record indices and the two candidate builds
        // number their records differently — an inlined precomp's layers sit
        // twenty-three places apart, an instanced one's are local to the asset.
        let exprs = has_exprs
            .then(|| layers::table(&scene.data, &bodies))
            .transpose()?;

        // The planner cannot see inside an expression body, so what the bodies
        // reach for is folded in here.
        if let Some(e) = &exprs {
            scene.caps |= emit_expressions::vocabulary(&e.bodies);
        }
        // Every name is dead. The table existed so `thisComp.layer('…')` could
        // resolve at runtime, and that lookup no longer exists in any emitted
        // body — a reference the layer pass cannot resolve fails the compile
        // rather than falling back to one.
        if exprs.is_some() {
            scene.prune_names(&Default::default())?;
        }
        // Effect and parameter names exist only so a body can look one up. Once
        // `expr::resolve` has rewritten those lookups to indices, nothing reads
        // them — and on `lights` they are 450 of the 454 bytes of names.
        if let Some(e) = &exprs {
            let mut named = std::collections::BTreeSet::new();
            for b in &e.bodies {
                named.extend(crate::expr::resolve::literals(b));
            }
            scene.prune_effect_names(&|name| named.contains(name))?;
        }
        let js = emit::emit(
            &scene,
            runtime_mode,
            options.minify,
            exprs.as_ref(),
            &options.markup,
        )?;
        let mut report = describe(&scene, js, instance, exprs.as_ref());
        report.assets = assets;
        Ok(report)
    };

    // Extracted markup and precomp instancing do not compose. The sprite holds
    // the fully-expanded document, but an instanced module binds against a tree
    // the runtime builds by expanding placeholders — a different, larger
    // element layout (ripple: 786 elements vs the 869 the bindings address). It
    // could be made to work by putting placeholders in the sprite, but then the
    // sprite is no longer a renderable picture, which is the point of the mode.
    // The same holds for a module with no markup of its own: the document it
    // hydrates is the fully-expanded one `compile_document` wrote.
    let fixed_tree = matches!(
        options.markup,
        crate::MarkupMode::Extracted(_) | crate::MarkupMode::None
    );
    if fixed_tree && options.instance_precomps == crate::Instancing::Always {
        anyhow::bail!(
            "--instance-precomps cannot be combined with --extract or --no-markup: an \
             instanced module binds against a tree expanded at runtime, so the sprite or \
             served document would not match it. Drop one of the two."
        );
    }

    let out = match options.instance_precomps {
        _ if fixed_tree => build(false)?,
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
    if src.len() > 32 * 1024 {
        src.len()
    } else {
        usize::MAX - src.len()
    }
}
