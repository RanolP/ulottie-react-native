pub mod backend;
pub mod data;
// The frame evaluator is no longer optional: the scene planner uses its
// geometry, transform and gradient math to bake static values at compile time.
// The `eval` feature is kept as a no-op so existing invocations keep working.
pub mod eval;
pub mod expr;
pub mod ir;
pub mod lottie;
pub mod scene;
pub mod support;
#[cfg(feature = "wasm")]
pub mod wasm;

use anyhow::Result;
use serde::Serialize;

pub use scene::assets::ExtractedAsset;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeMode {
    /// Emit `import { run } from './driver.js'` — the runtime is a separate
    /// shared module. Default mode; best when many animations ship together.
    #[default]
    Extern,
    /// Inline a tree-shaken subset of the runtime into the compiled output.
    /// Produces a self-contained JS module with no external dependencies.
    /// Best for single animations or when the shared runtime can't be cached.
    Embedded,
}

/// Whether precomps are planned once and replayed, or walked inline at every
/// use.
///
/// Instancing is a large win on heavily-instanced files — `ripple` goes from
/// 3825 to 2416 B gzipped and its frame time roughly halves — and a loss on
/// lightly-instanced ones, where gzip already deduplicates the inlined copies
/// and the expansion code is pure overhead. So it is measured, not assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Instancing {
    /// Compile both ways and keep the smaller compressed module.
    #[default]
    Auto,
    Always,
    Never,
}

/// Where a module's initial markup comes from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MarkupMode {
    /// The module carries the markup as a string literal. One request, no
    /// ordering constraint, and the module is directly server-renderable.
    #[default]
    Inline,
    /// The markup lives in an external SVG sprite as `<symbol id="…">`; the
    /// module carries only the outer `<svg>` shell and clones the symbol's
    /// children in at mount.
    ///
    /// Worth it when the markup dominates the module (it usually does), when
    /// several animations share one sprite, or when the picture should be
    /// cacheable and preloadable on its own — the sprite is a plain `.svg`
    /// with its own URL, so it revalidates independently of the JS.
    ///
    /// The sprite must already be in the document when `init()` runs: inline
    /// it into the HTML, or fetch and inject it once. `init()` stays
    /// synchronous and throws if the symbol is missing.
    Extracted(String),
    /// The module carries no markup at all — the SSR module. The page already
    /// has the document ([`compile_document`]'s output, server-rendered into
    /// the HTML, or a `<noscript>` body), and `init(el)` adopts the `<svg>`
    /// it finds in `el`: hydration is implied, since there is nothing else the
    /// module could do, and `init` throws by name when the container is empty.
    /// Each adopted instance gets its own id suffix, so several served copies
    /// of one animation on a page keep their gradients and masks apart.
    ///
    /// Strictly smaller than [`MarkupMode::Inline`] — the document is dead
    /// bytes to a module that only ever hydrates — and no second file to
    /// place, unlike [`MarkupMode::Extracted`]. Planned fully expanded, never
    /// instanced, because the served document is the expanded tree.
    None,
}

/// How embedded images (`data:` URIs) above a size threshold are delivered.
///
/// Off by default: without extraction the markup keeps every image inline as
/// a data URI, exactly as the source had it, and every existing output is
/// byte-identical. With `extract` on, images whose decoded size is strictly
/// above `threshold` bytes are written out as files and referenced by
/// `url_base + name`, so they load concurrently with (or ahead of, via the
/// manifest) the module. See `scene::assets` and EXPLAINER's
/// "Pre-loadable assets".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetOptions {
    pub extract: bool,
    /// Where the markup points at the extracted files, e.g. `"assets/"`.
    /// Always slash-terminated when used; `"assets"` and `"assets/"` agree.
    pub url_base: String,
    /// Images whose decoded byte count is *strictly above* this stay inline.
    pub threshold: usize,
}

impl Default for AssetOptions {
    fn default() -> Self {
        Self {
            extract: false,
            url_base: "assets/".into(),
            threshold: 4096,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// How the runtime is delivered: a shared `./driver.js` import (extern)
    /// or inlined into the compiled module (embedded).
    pub runtime_mode: RuntimeMode,
    /// Minify the emitted module. Turning this off prints the same module
    /// line-oriented — one SVG element and one binding per line — which is what
    /// the checked-in output snapshots use so compiler changes are reviewable.
    pub minify: bool,
    /// Byte budget for inlining the document template into the module.
    ///
    /// Under it the module carries the document literally: one string, no
    /// construction code, directly server-renderable. Over it, repeated
    /// subtrees are factored into a table the runtime expands at mount —
    /// cheaper to parse and build, at the cost of a little expansion code.
    ///
    /// Set to `usize::MAX` to always inline, or `0` to always factor.
    /// [`compile_document`] is unaffected either way: it always returns the
    /// fully-expanded document.
    pub inline_limit: usize,
    /// Where the initial markup comes from: the module itself, or an external
    /// sprite the page supplies.
    pub markup: MarkupMode,
    /// Unsupported Lottie features to accept anyway.
    ///
    /// Compilation fails when the source uses something the backend does not
    /// implement, because ignoring it changes how the animation looks and a
    /// silent change is the worst outcome. Listing a feature here says the
    /// degradation is understood and accepted for this input.
    pub allow: std::collections::BTreeSet<support::Feature>,
    /// Whether to plan each precomp once and replay it per use, instead of
    /// walking it inline at every use.
    pub instance_precomps: Instancing,
    /// Whether oversized embedded images become URL references plus a
    /// manifest of files to serve alongside the module.
    pub assets: AssetOptions,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            runtime_mode: RuntimeMode::default(),
            minify: true,
            inline_limit: scene::DEFAULT_INLINE_LIMIT,
            markup: MarkupMode::default(),
            allow: Default::default(),
            instance_precomps: Instancing::default(),
            assets: AssetOptions::default(),
        }
    }
}

/// Compile a Lottie animation JSON string into JS module source code.
pub fn compile(json: &str) -> Result<String> {
    compile_with(json, &CompileOptions::default())
}

/// Compile, and report what the AOT stage decided along the way.
///
/// Same work as [`compile_with`]; the extra return value is what the size panel
/// needs to explain a number instead of just printing it.
pub fn compile_report(json: &str, options: &CompileOptions) -> Result<backend::Report> {
    check_supported(json, &options.allow)?;
    let animation: lottie::Animation = serde_json::from_str(json)?;
    let module = ir::lower(&animation)?;
    backend::report(&module, options)?
        .ok_or_else(|| anyhow::anyhow!("fixture uses features the data backend doesn't support"))
}

pub fn compile_with(json: &str, options: &CompileOptions) -> Result<String> {
    check_supported(json, &options.allow)?;
    let animation: lottie::Animation = serde_json::from_str(json)?;
    let module = ir::lower(&animation)?;
    backend::compile(&module, options)?
        .ok_or_else(|| anyhow::anyhow!("fixture uses features the data backend doesn't support"))
}

/// The module plus everything extraction pulled out of it.
///
/// When `options.assets.extract` is off, `assets` is empty and `manifest` is
/// `[]` — the module is then identical to [`compile_with`]'s output.
pub struct CompiledOutput {
    pub module: String,
    pub assets: Vec<ExtractedAsset>,
    /// JSON array of `{"url","file","mime","bytes"}` objects — what a web
    /// server turns into 103 Early Hints / `<link rel=preload>` entries.
    pub manifest: String,
}

/// Compile, returning the module and the extracted image artifacts.
///
/// The assets are the files the markup now references: each must be written
/// to `<url_base><asset.name>` and served alongside the module, or the images
/// 404. The manifest's `url` entries use the same `url_base`.
pub fn compile_with_output(json: &str, options: &CompileOptions) -> Result<CompiledOutput> {
    check_supported(json, &options.allow)?;
    let animation: lottie::Animation = serde_json::from_str(json)?;
    let module = ir::lower(&animation)?;
    let report = backend::report(&module, options)?
        .ok_or_else(|| anyhow::anyhow!("fixture uses features the data backend doesn't support"))?;
    let manifest = scene::assets::manifest(&report.assets, &options.assets.url_base);
    Ok(CompiledOutput {
        module: report.js,
        assets: report.assets,
        manifest,
    })
}

/// Fail unless every feature the source uses is either implemented or
/// explicitly allowed.
pub fn check_supported(
    json: &str,
    allow: &std::collections::BTreeSet<support::Feature>,
) -> Result<()> {
    let doc: serde_json::Value = serde_json::from_str(json)?;
    if let Some(msg) = support::reject(&support::scan(&doc), allow) {
        anyhow::bail!(msg);
    }
    Ok(())
}

/// Everything the source uses that the backend does not implement, whether or
/// not it is allowed. Reported by the dev server so degradations stay visible.
pub fn unsupported(json: &str) -> Result<Vec<support::Finding>> {
    let doc: serde_json::Value = serde_json::from_str(json)?;
    Ok(support::scan(&doc))
}

/// Lower a Lottie JSON straight to the `Payload`. The payload is the analysis
/// IR: `eval::render` renders it directly as the reference implementation, and
/// `scene::plan` consumes it to produce the emitted wire format.
pub fn compile_to_payload(json: &str) -> Result<data::Payload> {
    let animation: lottie::Animation = serde_json::from_str(json)?;
    let module = ir::lower(&animation)?;
    if !data::can_encode(&module) {
        anyhow::bail!("fixture uses features the data backend doesn't support yet");
    }
    data::encode(&module)
}

/// The animation's **document**: one standalone SVG showing the composition's
/// first frame.
///
/// It needs no script to render, which is what makes it usable for server-side
/// rendering, `<noscript>` fallbacks, static thumbnails, `<img src>`, or as the
/// DOM a client hydrates onto (`init(el, { hydrate: true })`).
///
/// Every binding is evaluated at the first frame and written as an ordinary
/// attribute — see [`scene::bake`]. Without that the document would hold only
/// what cannot change, and a layer with an animated transform would have no
/// `transform` at all and draw at the origin. What the module inlines is the
/// *un*baked form, since the runtime writes those attributes on mount anyway —
/// and a module compiled with [`MarkupMode::None`] inlines nothing: it is the
/// script that hydrates *this* document.
///
/// Expressions make exactness conditional rather than impossible: the bake
/// runs a compile-time interpreter over the raw body
/// ([`expr::interp`], a frame-aware twin of the runtime engine), and a body
/// that interpreter cannot decide bakes to its fallback — the keyframes the
/// body reads as `value` — exactly as the runtime falls back with no engine.
/// A missed evaluation costs picture accuracy on that one attribute; a wrong
/// one never ships, because the interpreter refuses everything it does not
/// understand.
///
/// **Generated ids keep their per-instance marker** (`id="g0--u"`,
/// `url(#g0--u)`): the document is valid and renders as-is, and a module
/// compiled with [`MarkupMode::None`] rewrites the marker per adopted mount so
/// several served copies of one animation keep their gradients and masks
/// apart. A page that inlines more than one copy *without* hydrating them
/// should replace `--u` with a suffix of its own per copy — a plain string
/// replacement, no parsing. Resolving the marker here to a fixed suffix (what
/// this used to do) made every served copy identical, and nothing downstream
/// could tell them apart again.
pub fn compile_document(json: &str) -> Result<String> {
    // Inlined markup is parsed as HTML, where the SVG namespace is implied.
    // A standalone document has to declare it or a browser renders the source
    // tree instead of the picture.
    Ok(document_template(json)?.replacen("<svg ", "<svg xmlns=\"http://www.w3.org/2000/svg\" ", 1))
}

/// The document with per-mount id markers in place — which is also how it
/// ships; see [`compile_document`].
///
/// Planned fully expanded — never instanced — which is also what lets the bake
/// cover every element: an instanced precomp body is stored once and replayed
/// at a different point on each instance's clock, so it has no single frame.
fn document_template(json: &str) -> Result<String> {
    Ok(document_template_with(json, &CompileOptions::default())?.0)
}

/// Same plan, with the asset options applied — extraction runs on the scene
/// before anything reads `markup`, so a document or symbol compiled this way
/// references the same files the module does.
fn document_template_with(
    json: &str,
    options: &CompileOptions,
) -> Result<(String, Vec<ExtractedAsset>)> {
    let animation: lottie::Animation = serde_json::from_str(json)?;
    let module = ir::lower(&animation)?;
    if !data::can_encode(&module) {
        anyhow::bail!("fixture uses features the data backend doesn't support");
    }
    let payload = data::encode(&module)?;
    let bodies: Vec<ir::Expression> = module.expressions.iter().cloned().collect();
    let mut scene = scene::plan(&payload, !module.expressions.is_empty(), &bodies)?;
    let assets = if options.assets.extract {
        scene::assets::extract(&mut scene, &options.assets)
    } else {
        Vec::new()
    };
    Ok((scene.markup, assets))
}

/// The document plus its extracted image artifacts — the `--document`
/// counterpart of [`compile_with_output`].
pub struct DocumentOutput {
    pub svg: String,
    pub assets: Vec<ExtractedAsset>,
    pub manifest: String,
}

/// [`compile_document`], with the asset options applied. See
/// [`compile_with_output`] for what to do with `assets` and `manifest`.
pub fn compile_document_with(json: &str, options: &CompileOptions) -> Result<DocumentOutput> {
    let (doc, assets) = document_template_with(json, options)?;
    // Inlined markup is parsed as HTML, where the SVG namespace is implied.
    // A standalone document has to declare it or a browser renders the source
    // tree instead of the picture.
    let doc = doc.replacen("<svg ", "<svg xmlns=\"http://www.w3.org/2000/svg\" ", 1);
    let manifest = scene::assets::manifest(&assets, &options.assets.url_base);
    Ok(DocumentOutput {
        svg: doc,
        assets,
        manifest,
    })
}

/// The document as a sprite `<symbol>`, ready to be concatenated with others
/// into one file a page inlines or preloads.
///
/// The symbol's content is exactly the document's children, in order — the
/// runtime binds by document-order index, so nothing may be added or removed.
/// The first-frame bake respects that: it only ever adds attributes, which is
/// why a sprite can be both the pre-script picture and the tree that hydrates.
///
/// Generated ids keep their marker here, unlike [`compile_document`]: a sprite
/// can be mounted any number of times and each mount resolves its own.
pub fn compile_symbol(json: &str, id: &str) -> Result<String> {
    Ok(scene::symbol(&document_template(json)?, id))
}

/// [`compile_symbol`], with the asset options applied — the sprite's `<image>`
/// hrefs reference the extracted files, returned alongside for the caller to
/// write and serve.
pub fn compile_symbol_with(
    json: &str,
    id: &str,
    options: &CompileOptions,
) -> Result<DocumentOutput> {
    let (doc, assets) = document_template_with(json, options)?;
    let manifest = scene::assets::manifest(&assets, &options.assets.url_base);
    Ok(DocumentOutput {
        svg: scene::symbol(&doc, id),
        assets,
        manifest,
    })
}

/// Combine symbols into one sprite file. Symbol ids have to be unique within
/// it; each is the `id` passed to [`compile_symbol`].
pub fn sprite(symbols: &[String]) -> String {
    scene::sprite(symbols)
}

/// Same, indented one element per line for review and diffing.
/// Markup, one element per line — the same formatter the document and the
/// snapshots use. The sprite ships on one line like everything else.
pub fn markup_pretty(markup: &str) -> String {
    backend::pretty::markup_plain(markup)
}

pub fn compile_document_pretty(json: &str) -> Result<String> {
    Ok(backend::pretty::markup_plain(&compile_document(json)?))
}

/// Which optional runtime features a Lottie animation requires. The embedded
/// build inlines only the regions whose flags are `true`; the rest are
/// tree-shaken out. Returned by [`analyze_features`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedFeatures {
    /// Lottie expressions (`makeThisProperty`, path API, expression runtime).
    pub expressions: bool,
    /// `TrimPath` shape modifier (largest single feature region).
    pub trim_path: bool,
    /// Linear / radial gradient fills and strokes.
    pub gradient: bool,
}

/// Inspect a Lottie JSON to determine which optional runtime features the
/// embedded build would include. `Ok(None)` means the backend can't encode
/// this fixture yet.
pub fn analyze_features(json: &str) -> Result<Option<EmbeddedFeatures>> {
    let animation: lottie::Animation = serde_json::from_str(json)?;
    let module = ir::lower(&animation)?;
    if !data::can_encode(&module) {
        return Ok(None);
    }
    let payload = data::encode(&module)?;
    let bodies: Vec<ir::Expression> = module.expressions.iter().cloned().collect();
    let scene = scene::plan(&payload, !module.expressions.is_empty(), &bodies)?;
    Ok(Some(EmbeddedFeatures {
        expressions: scene.caps.contains(scene::Caps::EXPRESSIONS),
        trim_path: scene.caps.contains(scene::Caps::TRIM),
        gradient: scene.caps.contains(scene::Caps::GRADIENT),
    }))
}

/// The full runtime, every capability on, minified. Used only to report the
/// ceiling against `lottie.min.js` — compiled output imports the individual
/// modules it binds, so nothing actually loads this.
pub fn minified_driver() -> String {
    backend::emit::build_driver()
}

/// Source of one runtime module by its specifier relative to the runtime root
/// (`core.js`, `ops/txt.js`, …). Extern-mode output imports these directly, so
/// a host has to be able to serve them.
pub fn runtime_module(path: &str) -> Option<&'static str> {
    backend::emit::modules()
        .iter()
        .find(|m| m.name == path)
        .map(|m| m.src)
}

/// Minified source of the runtime a capability set pulls in, by capability
/// name. The names are the ones [`backend::Report::caps`] reports.
///
/// This is the slice a bundler actually ships for one animation, as opposed to
/// [`minified_driver`], which is the whole runtime and which nothing loads.
pub fn runtime_slice(caps: &[String]) -> String {
    let mut c = scene::Caps::empty();
    for (name, bit) in scene::Caps::all().iter_names() {
        if caps.iter().any(|n| n == name) {
            c |= bit;
        }
    }
    backend::emit::runtime_source(c)
}

/// The same slice, unminified — the modules as they are written.
///
/// The demo shows this rather than re-deriving structure from the minified
/// form: a formatter guessing at a compiled module has to be right about
/// template literals and regexes, and this has nothing to be wrong about.
pub fn runtime_slice_pretty(caps: &[String]) -> String {
    let mut c = scene::Caps::empty();
    for (name, bit) in scene::Caps::all().iter_names() {
        if caps.iter().any(|n| n == name) {
            c |= bit;
        }
    }
    backend::emit::runtime_pretty(c)
}

/// Every runtime module as `(specifier, source)` — for publishing the tree.
pub fn runtime_modules() -> impl Iterator<Item = (&'static str, &'static str)> {
    backend::emit::modules().iter().map(|m| (m.name, m.src))
}

/// Build the embedded runtime source for an arbitrary feature subset. Used by
/// the dev server to compute per-feature size deltas: by calling this with
/// each feature individually stripped, the UI can show "if you tree-shake
/// this feature you save N bytes".
pub fn embedded_runtime_size(features: EmbeddedFeatures) -> usize {
    let mut caps = scene::Caps::all();
    if !features.expressions {
        caps.remove(scene::Caps::EXPRESSIONS);
    }
    if !features.trim_path {
        caps.remove(scene::Caps::TRIM);
    }
    if !features.gradient {
        caps.remove(scene::Caps::GRADIENT);
    }
    backend::emit::runtime_size(caps)
}
