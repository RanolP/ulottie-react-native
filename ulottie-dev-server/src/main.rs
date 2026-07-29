//! Compile server for the demo page and the visual-diff harness.
//!
//! It serves data, not a page. Vite owns the demo (`yarn --cwd
//! ulottie-dev-server dev`) and proxies these routes here; a built demo is
//! static and compiles in-browser with the wasm build of the same crate.
//!
//! URL layout:
//!
//!   /healthz                readiness probe (the test harness waits on it)
//!   /compile (POST)         body = raw Lottie JSON
//!   /.output/<id>.js        compiled JS (lazy for fixtures, eager for uploads)
//!   /.output/<id>.json      fixture source (registered) or upload source
//!   /.output/driver.js      minified shared runtime (served from memory)
//!   /.output/runtime/**     the runtime as an ES module tree, so extern-mode
//!                           output resolves its imports
//!   /_fixtures/<name>.json  registered fixture source
//!
//! On-disk locations:
//!
//!   __fixtures__/           pre-registered Lottie sources
//!                            (`<workspace>/_fixtures/animations/`)
//!   .output/                disk cache: compiled JS + upload sources
//!
//! `__fixtures__` is the conceptual name for the fixture source location, not
//! a URL prefix — both `/.output/<name>.json` and `/_fixtures/<name>.json`
//! resolve to it.
//!
//! `/.output/driver.js` is a dedicated route ahead of the chain: the
//! compiler's minified driver is computed once on first request and held
//! in process memory. No disk mirror needed.

use std::net::SocketAddr;
use std::path::{Path as StdPath, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use std::io::Write;

use anyhow::Result;

mod contract;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::{Parser, ValueEnum};
use flate2::{Compression, write::GzEncoder};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::fs;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

#[derive(Parser, Debug)]
#[command(name = "ulottie-dev-server")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Parser, Debug)]
enum Cmd {
    /// Start the dev server.
    Serve(ServeArgs),
    /// Compile every fixture under `_fixtures/animations/` and print a
    /// size report (raw + gzipped bytes) for the source JSON, the
    /// extern-mode compiled JS, the embedded (tree-shaken) JS, and the
    /// shared `driver.js`. Use this to track how code changes affect
    /// output size across the whole fixture corpus.
    Sizes(SizesArgs),
}

#[derive(Parser, Debug)]
struct ServeArgs {
    /// Port to listen on.
    #[arg(long, default_value_t = 4567)]
    port: u16,
}

#[derive(Parser, Debug)]
struct SizesArgs {
    /// Sort the fixture rows by this column. Default is fixture name.
    #[arg(long, value_enum, default_value_t = SortKey::Name)]
    sort: SortKey,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum SortKey {
    /// Fixture file name (alphabetical).
    Name,
    /// Embedded JS raw size (descending — largest first).
    Embedded,
    /// Embedded JS gzipped size (descending).
    EmbeddedGz,
    /// Extern JS raw size (descending).
    Extern,
    /// Source JSON raw size (descending).
    Json,
}

/// Pre-registered file locations, resolved once at startup.
#[derive(Debug, Clone)]
struct PathLayout {
    /// Pre-registered fixture sources — the "__fixtures__" location.
    fixtures_dir: PathBuf,
    /// Disk cache for compiled JS + ad-hoc upload sources.
    output_dir: PathBuf,
    /// The `lottie-web` bundle, read from `node_modules` — this is a dev tool,
    /// and taking the size from the installed dependency means bumping it moves
    /// the "original Lottie runtime" baseline instead of leaving a stale copy.
    lottie_web_bundle: PathBuf,
}

impl PathLayout {
    fn from_crate_dir(crate_dir: &StdPath) -> Result<Self> {
        let workspace = crate_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("crate has no parent"))?
            .to_path_buf();
        Ok(Self {
            lottie_web_bundle: workspace.join("node_modules/lottie-web/build/player/lottie.min.js"),
            fixtures_dir: workspace.join("_fixtures").join("animations"),
            output_dir: crate_dir.join(".output"),
        })
    }
}

/// Unsupported features each fixture is allowed to use, from
/// `_fixtures/allowances.json`. Keeping it in the repo rather than in code
/// means a degradation shows up in review as a diff.
fn fixture_allowances(
    name: &str,
) -> std::collections::BTreeSet<ulottie_compiler::support::Feature> {
    static CACHE: OnceLock<serde_json::Value> = OnceLock::new();
    let doc = CACHE.get_or_init(|| {
        let path = StdPath::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("_fixtures/allowances.json"));
        path.and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null)
    });
    doc.get(name)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .filter_map(ulottie_compiler::support::Feature::from_name)
                .collect()
        })
        .unwrap_or_default()
}

/// Minified driver.js, computed once on first access. Same string used to
/// serve `/.output/driver.js` and to measure the extern-runtime size in
/// `/compile` responses, so both stay in sync without a disk round-trip.
fn minified_driver() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(ulottie_compiler::minified_driver)
        .as_str()
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let paths = Arc::new(PathLayout::from_crate_dir(&crate_dir)?);

    match args.cmd {
        Cmd::Serve(serve_args) => run_server(paths, serve_args).await,
        Cmd::Sizes(sizes_args) => run_sizes(paths, sizes_args).await,
    }
}

async fn run_server(paths: Arc<PathLayout>, args: ServeArgs) -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ulottie_dev_server=info,tower_http=warn".into()),
        )
        .init();

    fs::create_dir_all(&paths.output_dir).await?;

    // /.output/<file>: .output/ wins on hit; fixture source dir wins on miss so
    // fixture JSON falls through naturally without being copied.
    let output_service =
        ServeDir::new(&paths.output_dir).fallback(ServeDir::new(&paths.fixtures_dir));

    // This serves data, not a page. Vite owns the demo (`yarn --cwd
    // ulottie-dev-server dev`) and proxies these routes here; a built demo is
    // static and uses the wasm compiler instead.
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest_service("/_fixtures", ServeDir::new(&paths.fixtures_dir))
        .route("/compile", post(compile_handler))
        .route("/.output/driver.js", get(serve_driver))
        .route("/.output/runtime/{*path}", get(serve_runtime_module))
        .nest_service("/.output", output_service)
        .layer(middleware::from_fn_with_state(
            paths.clone(),
            ensure_compiled,
        ))
        .layer(middleware::from_fn(no_store))
        .layer(TraceLayer::new_for_http())
        .with_state(paths.clone());

    let addr: SocketAddr = ([127, 0, 0, 1], args.port).into();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("ulottie-dev-server listening on http://{addr}");
    eprintln!("  __fixtures__: {}", paths.fixtures_dir.display());
    eprintln!("  .output:      {}", paths.output_dir.display());
    eprintln!("  POST /compile               — body = raw Lottie JSON");
    eprintln!("  GET  /.output/<id>.{{json,js}} — fixture stem or upload hash");
    eprintln!("  the page itself: yarn workspace ulottie-dev-server dev");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Serve the whole runtime as one module. Nothing generated imports it —
/// compiled output imports `/.output/runtime/**` (or `runtime-legacy/**`) — but
/// it is handy for loading a prebuilt runtime by hand, and it is the number the
/// size panel reports as the ceiling.
async fn serve_driver() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        minified_driver(),
    )
}

/// Serve the runtime as its own ES module tree. Extern-mode output imports
/// exactly the entry points it binds (`./runtime/core.js`,
/// `./runtime/ops/txt.js`, …) so a bundler resolves a normal module graph and
/// shakes it; the browser resolves the same specifiers against this route.
async fn serve_runtime_module(axum::extract::Path(rest): axum::extract::Path<String>) -> Response {
    match ulottie_compiler::runtime_module(&rest) {
        Some(src) => (
            [(
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            )],
            src.to_string(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, format!("no runtime module `{rest}`")).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// If the URI names a registered fixture, ensure the cached artifact is at
/// least as fresh as the source, then let the static service serve it:
///
///   `/.output/<name>.js`           extern output
///   `/.output/<name>.embedded.js`  self-contained build
///   `/.output/<name>.extracted.js` markup extracted to a sprite
///   `/.output/<name>.sprite.svg`   that sprite
///   `/.output/<name>.instanced.js` precomps planned once and replayed
///   `/.output/<name>.slice.js`     just the runtime modules it imports
///   `/.output/<name>.pretty.*`     any of the above, unminified
async fn ensure_compiled(
    State(paths): State<Arc<PathLayout>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let uri_path = req.uri().path().to_string();
    if let Some(file) = uri_path
        .strip_prefix("/.output/")
        .filter(|p| !p.contains('/') && !p.contains(".."))
    {
        // `.pretty` sits between the stem and the extension. Strip it before
        // routing — the variant is the same either way, only the minification
        // differs — but cache under the *requested* name, so the two forms do
        // not overwrite each other.
        let (routed, pretty) = match file.find(".pretty.") {
            Some(i) => (
                format!("{}{}", &file[..i], &file[i + ".pretty".len()..]),
                true,
            ),
            None => (file.to_string(), false),
        };
        // `<name>.sprite.svg` is produced as a side effect of compiling
        // `<name>.extracted.js`, so both requests drive the same build.
        // Longest suffix first: `.embedded.js` and `.slice.js` both end in
        // `.js`, so the plain-module arm has to come last.
        let named = |suffix: &str, v: Variant| routed.strip_suffix(suffix).map(|n| (n, v));
        let (name, variant) = match named(".embedded.js", Variant::Embedded)
            .or_else(|| named(".slice.js", Variant::Slice))
            .or_else(|| named(".instanced.js", Variant::Instanced))
            .or_else(|| named(".extracted.js", Variant::Extracted))
            .or_else(|| named(".sprite.svg", Variant::Extracted))
            .or_else(|| named(".js", Variant::Extern))
        {
            Some(pair) => pair,
            None => return next.run(req).await,
        };
        let src = paths.fixtures_dir.join(format!("{name}.json"));
        if src.is_file() {
            let cache = paths.output_dir.join(file);
            if let Err(e) = compile_if_stale(&src, &cache, name, variant, pretty, &paths).await {
                return e.into_response();
            }
        }
    }
    next.run(req).await
}

/// Stamp `Cache-Control: no-store` on every response.
async fn no_store(req: Request<axum::body::Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

// ---------------------------------------------------------------------------
// Compile
// ---------------------------------------------------------------------------

/// When this binary was built. A cached module is stale if the compiler that
/// produced it has been rebuilt since — comparing against the source JSON
/// alone silently serves output from the previous compiler, which is a
/// genuinely confusing failure: the tests fail against code that is no longer
/// in the tree.
fn compiler_mtime() -> SystemTime {
    static T: std::sync::OnceLock<SystemTime> = std::sync::OnceLock::new();
    *T.get_or_init(|| {
        std::env::current_exe()
            .and_then(|p| std::fs::metadata(p))
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    })
}

/// Disk-only cache. If the cache file is at least as new as the source and the
/// compiler, no-op. Otherwise recompile and write.
/// Which artifact a request wants. Extraction is a delivery choice rather than
/// a runtime mode, so it does not fold into `RuntimeMode`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    Extern,
    Embedded,
    Extracted,
    Instanced,
    /// Not a module: the runtime modules an extern build imports, minified.
    /// The size table names it, so the viewer has to be able to show it.
    Slice,
}

async fn compile_if_stale(
    src: &StdPath,
    cache_path: &StdPath,
    name: &str,
    variant: Variant,
    pretty: bool,
    paths: &PathLayout,
) -> Result<(), ApiError> {
    let src_mtime = fs::metadata(src)
        .await
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .max(compiler_mtime());
    if let Ok(meta) = fs::metadata(cache_path).await {
        let cache_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if cache_mtime >= src_mtime {
            return Ok(());
        }
    }
    let json = fs::read_to_string(src)
        .await
        .map_err(|e| ApiError::compile(format!("read {name}: {e}")))?;
    // The dev server is a viewer: it has to render whatever it is handed,
    // including an upload using a feature the backend does not implement.
    // Refusing would show nothing at all, which is strictly less informative
    // than showing the degraded render next to the warning — and the response
    // reports every finding, so the degradation is never silent. The strict
    // gate lives in the CLI and in `_fixtures/allowances.json`, which the
    // snapshot suite still enforces.
    let allow = ulottie_compiler::unsupported(&json)
        .unwrap_or_default()
        .iter()
        .map(|f| f.feature)
        .collect();
    if variant == Variant::Slice {
        let report = ulottie_compiler::compile_report(
            &json,
            &ulottie_compiler::CompileOptions {
                allow,
                ..Default::default()
            },
        )
        .map_err(|e| ApiError::compile(format!("compile {name}: {e}")))?;
        let body = if pretty {
            ulottie_compiler::runtime_slice_pretty(&report.caps)
        } else {
            minified_slice(&report)
        };
        fs::write(cache_path, body)
            .await
            .map_err(|e| ApiError::compile(format!("write {name}.slice.js: {e}")))?;
        return Ok(());
    }
    let markup = match variant {
        Variant::Extracted => ulottie_compiler::MarkupMode::Extracted(name.to_string()),
        _ => ulottie_compiler::MarkupMode::Inline,
    };
    let js = ulottie_compiler::compile_with(
        &json,
        &ulottie_compiler::CompileOptions {
            runtime_mode: match variant {
                Variant::Embedded => ulottie_compiler::RuntimeMode::Embedded,
                _ => ulottie_compiler::RuntimeMode::Extern,
            },
            // The `.js`/`.embedded.js`/`.extracted.js` variants use the real
            // default so the panel and the pixel tests see what ships;
            // `.instanced.js` forces it on to keep that path covered.
            instance_precomps: if variant == Variant::Instanced {
                ulottie_compiler::Instancing::Always
            } else {
                ulottie_compiler::Instancing::Auto
            },
            markup,
            allow,
            // Unminified is the compiler's own review form — one element and
            // one binding per line — not a reformatting of the minified bytes.
            minify: !pretty,
            ..Default::default()
        },
    )
    .map_err(|e| ApiError::compile(format!("compile {name}: {e}")))?;
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).await.ok();
    }
    if variant == Variant::Extracted {
        // One fixture per sprite here. A real build shares one across many;
        // keeping them separate keeps a stale fixture from being served out of
        // another fixture's file.
        let symbol = ulottie_compiler::compile_symbol(&json, name)
            .map_err(|e| ApiError::compile(format!("extract {name}: {e}")))?;
        let svg = ulottie_compiler::sprite(&[symbol]);
        fs::write(paths.output_dir.join(format!("{name}.sprite.svg")), &svg)
            .await
            .map_err(|e| ApiError::compile(format!("write {name}.sprite.svg: {e}")))?;
        fs::write(
            paths.output_dir.join(format!("{name}.sprite.pretty.svg")),
            ulottie_compiler::markup_pretty(&svg),
        )
        .await
        .map_err(|e| ApiError::compile(format!("write {name}.sprite.pretty.svg: {e}")))?;
        fs::write(paths.output_dir.join(format!("{name}.extracted.js")), js)
            .await
            .map_err(|e| ApiError::compile(format!("write {name}.extracted.js: {e}")))?;
        return Ok(());
    }
    fs::write(cache_path, js)
        .await
        .map_err(|e| ApiError::compile(format!("write {name}.js: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// POST /compile
// ---------------------------------------------------------------------------

/// Bytes each optional feature costs the embedded runtime, measured against
/// the all-features-on baseline. These are intrinsic properties of `driver.js`
/// (and the minifier), so we cache the result after the first compute — the
/// oxc minifier round-trip is ~50 ms per call.
#[derive(Clone, Copy)]
struct FeatureCost {
    expressions: i32,
    trim_path: i32,
    gradient: i32,
}

fn feature_costs() -> &'static FeatureCost {
    use std::sync::OnceLock;
    static CACHE: OnceLock<FeatureCost> = OnceLock::new();
    CACHE.get_or_init(|| {
        let all_on = ulottie_compiler::EmbeddedFeatures {
            expressions: true,
            trim_path: true,
            gradient: true,
        };
        let full = ulottie_compiler::embedded_runtime_size(all_on) as i32;
        let cost = |omitted: ulottie_compiler::EmbeddedFeatures| {
            full - ulottie_compiler::embedded_runtime_size(omitted) as i32
        };
        FeatureCost {
            expressions: cost(ulottie_compiler::EmbeddedFeatures {
                expressions: false,
                ..all_on
            }),
            trim_path: cost(ulottie_compiler::EmbeddedFeatures {
                trim_path: false,
                ..all_on
            }),
            gradient: cost(ulottie_compiler::EmbeddedFeatures {
                gradient: false,
                ..all_on
            }),
        }
    })
}

#[derive(Deserialize)]
struct LottieHeader {
    nm: Option<String>,
    #[serde(default)]
    ip: f64,
    #[serde(default)]
    op: f64,
}

fn gzip_size(data: &[u8]) -> usize {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).ok();
    enc.finish().map(|v| v.len()).unwrap_or(0)
}

fn size_entry(data: &[u8]) -> contract::SizeEntry {
    contract::SizeEntry {
        raw: data.len() as u32,
        gzipped: gzip_size(data) as u32,
    }
}

async fn compile_handler(
    State(paths): State<Arc<PathLayout>>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let json_text = std::str::from_utf8(&body)
        .map_err(|e| ApiError::bad_request(format!("body is not UTF-8: {e}")))?;
    let header: LottieHeader = serde_json::from_str(json_text)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))?;

    // Persist a compacted copy of the JSON: the `<id>.json` URL serves
    // these bytes, lottie-web parses them happily, and the size shown in
    // the matrix matches what production would actually ship. Hashing the
    // compacted bytes keeps fixture-vs-upload cache hits stable regardless
    // of whether the source was indented.
    let json_compact = serde_json::from_str::<serde_json::Value>(json_text)
        .and_then(|v| serde_json::to_vec(&v))
        .map_err(|e| ApiError::bad_request(format!("re-serialize JSON: {e}")))?;
    let id = content_hash(&json_compact);
    let json_path = paths.output_dir.join(format!("{id}.json"));
    let js_path = paths.output_dir.join(format!("{id}.js"));
    fs::create_dir_all(&paths.output_dir).await.ok();
    fs::write(&json_path, &json_compact)
        .await
        .map_err(|e| ApiError::compile(format!("write upload json: {e}")))?;
    compile_if_stale(&json_path, &js_path, &id, Variant::Extern, false, &paths).await?;

    // Compiled JS reads from disk (one round-trip per `/compile`, cheap).
    // Minified driver is in process memory — same source the `/driver.js`
    // route serves, so the byte counts agree without a disk hop.
    let js_bytes = fs::read(&js_path)
        .await
        .map_err(|e| ApiError::compile(format!("read {id}.js: {e}")))?;
    let ulottie_runtime_bytes: &[u8] = minified_driver().as_bytes();
    let lottie_runtime_bytes = fs::read(&paths.lottie_web_bundle)
        .await
        .map_err(|e| ApiError::compile(format!("read lottie.min.js: {e}")))?;

    // Compile the embedded variant for size comparison. This produces a
    // self-contained module with a tree-shaken, minified runtime inlined.
    // Written to disk so the UI can load it for visual verification.
    let found = ulottie_compiler::unsupported(json_text).unwrap_or_default();
    let allow_all: std::collections::BTreeSet<_> = found.iter().map(|f| f.feature).collect();
    let embedded_options = ulottie_compiler::CompileOptions {
        runtime_mode: ulottie_compiler::RuntimeMode::Embedded,
        allow: allow_all.clone(),
        ..Default::default()
    };
    let embedded_js =
        ulottie_compiler::compile_with(json_text, &embedded_options).unwrap_or_default();
    let embedded_path = paths.output_dir.join(format!("{id}.embedded.js"));
    fs::write(&embedded_path, &embedded_js).await.ok();
    let embedded_bytes = embedded_js.as_bytes();

    let included = ulottie_compiler::analyze_features(json_text)
        .ok()
        .flatten()
        .unwrap_or_default();

    // The extern build again, this time keeping the compiler's own account of
    // what it decided. Cheap next to the two compiles already done above.
    let extern_options = ulottie_compiler::CompileOptions {
        allow: allow_all.clone(),
        ..Default::default()
    };
    let report = ulottie_compiler::compile_report(json_text, &extern_options)
        .map_err(|e| ApiError::compile(format!("report {id}: {e}")))?;

    // Extracted variant: module plus the sprite it sources its markup from.
    let extracted_options = ulottie_compiler::CompileOptions {
        markup: ulottie_compiler::MarkupMode::Extracted(id.clone()),
        allow: allow_all.clone(),
        ..Default::default()
    };
    let extracted_js =
        ulottie_compiler::compile_with(json_text, &extracted_options).unwrap_or_default();
    let sprite = ulottie_compiler::compile_symbol(json_text, &id)
        .map(|sym| ulottie_compiler::sprite(&[sym]))
        .unwrap_or_default();

    // Every artifact the size table names is written, not just measured: the
    // panel lets a row show its own bytes, and an upload is addressed by
    // content hash rather than a fixture name, so the on-demand compile route
    // cannot rebuild these from a source file that is not there.
    let slice = minified_slice(&report);

    // Every artifact twice: as it ships, and as the compiler writes it before
    // minification. The second is what the viewer shows — the same form the
    // snapshots are reviewed in, rather than a formatter's reconstruction.
    let pretty = |mode, markup| {
        ulottie_compiler::compile_with(
            json_text,
            &ulottie_compiler::CompileOptions {
                runtime_mode: mode,
                markup,
                allow: allow_all.clone(),
                minify: false,
                ..Default::default()
            },
        )
        .unwrap_or_default()
    };
    let pretty_extern = pretty(
        ulottie_compiler::RuntimeMode::Extern,
        ulottie_compiler::MarkupMode::Inline,
    );
    let pretty_embedded = pretty(
        ulottie_compiler::RuntimeMode::Embedded,
        ulottie_compiler::MarkupMode::Inline,
    );
    let pretty_extracted = pretty(
        ulottie_compiler::RuntimeMode::Extern,
        ulottie_compiler::MarkupMode::Extracted(id.clone()),
    );
    let pretty_json = serde_json::from_str::<serde_json::Value>(json_text)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| json_text.to_string());
    let pretty_slice = ulottie_compiler::runtime_slice_pretty(&report.caps);
    let pretty_sprite = ulottie_compiler::markup_pretty(&sprite);

    for (name, body) in [
        (format!("{id}.extracted.js"), extracted_js.as_bytes()),
        (format!("{id}.sprite.svg"), sprite.as_bytes()),
        (format!("{id}.slice.js"), slice.as_bytes()),
        (format!("{id}.pretty.json"), pretty_json.as_bytes()),
        (format!("{id}.pretty.js"), pretty_extern.as_bytes()),
        (
            format!("{id}.embedded.pretty.js"),
            pretty_embedded.as_bytes(),
        ),
        (
            format!("{id}.extracted.pretty.js"),
            pretty_extracted.as_bytes(),
        ),
        (format!("{id}.slice.pretty.js"), pretty_slice.as_bytes()),
        (format!("{id}.sprite.pretty.svg"), pretty_sprite.as_bytes()),
    ] {
        fs::write(paths.output_dir.join(name), body).await.ok();
    }

    let sizes = contract::Sizes {
        json: size_entry(&json_compact),
        js: size_entry(&js_bytes),
        runtime_slice: {
            // A static animation imports nothing; gzipping the empty string
            // would report a 20-byte header as if it were payload.
            if slice.is_empty() {
                contract::SizeEntry { raw: 0, gzipped: 0 }
            } else {
                size_entry(slice.as_bytes())
            }
        },
        ulottie_runtime: size_entry(ulottie_runtime_bytes),
        js_embedded: size_entry(embedded_bytes),
        js_extracted: size_entry(extracted_js.as_bytes()),
        sprite: size_entry(sprite.as_bytes()),
        features: contract::FeatureReport {
            expressions: included.expressions,
            trim_path: included.trim_path,
            gradient: included.gradient,
            expressions_cost: feature_costs().expressions,
            trim_path_cost: feature_costs().trim_path,
            gradient_cost: feature_costs().gradient,
        },
        lottie_runtime: size_entry(&lottie_runtime_bytes),
    };

    let total_frames = (header.op - header.ip).max(0.0);
    let response = contract::CompileResponse {
        id: id.clone(),
        json_url: format!("/.output/{id}.json"),
        js_url: format!("/.output/{id}.js"),
        js_embedded_url: format!("/.output/{id}.embedded.js"),
        js_extracted_url: format!("/.output/{id}.extracted.js"),
        sprite_url: format!("/.output/{id}.sprite.svg"),
        slice_url: format!("/.output/{id}.slice.js"),
        json_pretty_url: format!("/.output/{id}.pretty.json"),
        js_pretty_url: format!("/.output/{id}.pretty.js"),
        js_embedded_pretty_url: format!("/.output/{id}.embedded.pretty.js"),
        js_extracted_pretty_url: format!("/.output/{id}.extracted.pretty.js"),
        sprite_pretty_url: format!("/.output/{id}.sprite.pretty.svg"),
        slice_pretty_url: format!("/.output/{id}.slice.pretty.js"),
        name: header.nm,
        total_frames,
        sizes,
        plan: contract::Plan {
            caps: report.caps.clone(),
            modules: report.modules.clone(),
            is_static: report.is_static,
            instanced: report.instanced,
            templated: report.templated,
            generated: report.generated,
            elements: report.elements as u32,
            bindings: report.bindings as u32,
            records: report.records as u32,
        },
        unsupported: found
            .iter()
            .map(|f| contract::Unsupported {
                feature: f.feature.name().to_string(),
                effect: f.feature.effect().to_string(),
                // The viewer allows everything, so a finding here is a
                // degradation you are looking at right now.
                allowed: true,
            })
            .collect(),
    };

    // rkyv, not JSON: `demo/src/generated/bindings.ts` decodes this, and both
    // the types and the decoder are generated from `contract.rs` — so the page
    // cannot drift from the server.
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&response)
        .map_err(|e| ApiError::compile(format!("archive response: {e}")))?;
    Ok((
        [(header::CONTENT_TYPE, "application/octet-stream")],
        bytes.into_vec(),
    ))
}

/// Minified source of just the runtime modules this animation imports.
///
/// `Report::runtime_slice` already has the byte count; the panel also wants a
/// gzipped one, so rebuild the same slice here and let `size_entry` compress it.
fn minified_slice(report: &ulottie_compiler::backend::Report) -> String {
    if report.is_static {
        return String::new();
    }
    ulottie_compiler::runtime_slice(&report.caps)
}

fn content_hash(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(14);
    s.push('u');
    s.push('_');
    for b in digest.iter().take(6) {
        use std::fmt::Write;
        write!(s, "{b:02x}").unwrap();
    }
    s
}

// ---------------------------------------------------------------------------
// `sizes` subcommand — compile every fixture and report output sizes.
// ---------------------------------------------------------------------------

struct FixtureSizes {
    name: String,
    json_raw: usize,
    json_gz: usize,
    extern_raw: usize,
    extern_gz: usize,
    embedded_raw: usize,
    embedded_gz: usize,
    /// The runtime slice an extern build imports, gzipped — what a page
    /// downloads *in addition to* the module for a single animation.
    slice_gz: usize,
    /// Whether the self-contained build is generated code.
    generated: bool,
    features: ulottie_compiler::EmbeddedFeatures,
}

async fn run_sizes(paths: Arc<PathLayout>, args: SizesArgs) -> Result<()> {
    let mut entries = fs::read_dir(&paths.fixtures_dir).await?;
    let mut fixtures = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        // Skip temp/upload files dumped into the fixture dir.
        if name.contains('/') || name.starts_with("tmp/") {
            continue;
        }

        let sizes = measure_fixture(&path).await?;
        fixtures.push(sizes);
        eprintln!("measured {}", name);
    }

    if fixtures.is_empty() {
        eprintln!("no fixtures found in {}", paths.fixtures_dir.display());
        return Ok(());
    }

    // Sort.
    match args.sort {
        SortKey::Name => fixtures.sort_by(|a, b| a.name.cmp(&b.name)),
        SortKey::Embedded => fixtures.sort_by(|a, b| b.embedded_raw.cmp(&a.embedded_raw)),
        SortKey::EmbeddedGz => fixtures.sort_by(|a, b| b.embedded_gz.cmp(&a.embedded_gz)),
        SortKey::Extern => fixtures.sort_by(|a, b| b.extern_raw.cmp(&a.extern_raw)),
        SortKey::Json => fixtures.sort_by(|a, b| b.json_raw.cmp(&a.json_raw)),
    }

    let driver_bytes = minified_driver().as_bytes();
    let driver_raw = driver_bytes.len();
    let driver_gz = gzip_size(driver_bytes);
    let lottie_bytes = fs::read(&paths.lottie_web_bundle)
        .await
        .map(|v| v)
        .unwrap_or_default();
    let lottie_raw = lottie_bytes.len();
    let lottie_gz = gzip_size(&lottie_bytes);

    print_table(&fixtures, driver_raw, driver_gz, lottie_raw, lottie_gz);

    Ok(())
}

async fn measure_fixture(path: &StdPath) -> Result<FixtureSizes> {
    let json_text = std::fs::read_to_string(path)?;

    // Source JSON (compact form, matching what the server measures).
    let json_compact = serde_json::from_str::<serde_json::Value>(&json_text)
        .and_then(|v| serde_json::to_vec(&v))?;
    let json_raw = json_compact.len();
    let json_gz = gzip_size(&json_compact);

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let allow = fixture_allowances(stem);
    // Extern mode.
    let extern_js = ulottie_compiler::compile_with(
        &json_text,
        &ulottie_compiler::CompileOptions {
            allow: allow.clone(),
            ..Default::default()
        },
    )
    .unwrap_or_default();
    let extern_raw = extern_js.len();
    let extern_gz = gzip_size(extern_js.as_bytes());

    // Embedded mode.
    let allow2 = allow.clone();
    let embedded_js = ulottie_compiler::compile_with(
        &json_text,
        &ulottie_compiler::CompileOptions {
            runtime_mode: ulottie_compiler::RuntimeMode::Embedded,
            allow,
            ..Default::default()
        },
    )
    .unwrap_or_default();
    let embedded_raw = embedded_js.len();
    let embedded_gz = gzip_size(embedded_js.as_bytes());

    // The report carries both the slice and which backend won.
    let report = ulottie_compiler::compile_report(
        &json_text,
        &ulottie_compiler::CompileOptions {
            runtime_mode: ulottie_compiler::RuntimeMode::Embedded,
            allow: allow2,
            ..Default::default()
        },
    )
    .ok();
    let slice_gz = report
        .as_ref()
        .map(|r| gzip_size(ulottie_compiler::runtime_slice(&r.caps).as_bytes()))
        .unwrap_or(0);
    let generated = report.as_ref().is_some_and(|r| r.generated);

    let features = ulottie_compiler::analyze_features(&json_text)
        .ok()
        .flatten()
        .unwrap_or_default();

    Ok(FixtureSizes {
        name: path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string(),
        json_raw,
        json_gz,
        extern_raw,
        extern_gz,
        embedded_raw,
        embedded_gz,
        slice_gz,
        generated,
        features,
    })
}

fn fmt_bytes(n: usize) -> String {
    if n >= 1024 {
        format!("{:.1}K", n as f64 / 1024.0)
    } else {
        format!("{}B", n)
    }
}

fn print_table(
    fixtures: &[FixtureSizes],
    driver_raw: usize,
    driver_gz: usize,
    lottie_raw: usize,
    lottie_gz: usize,
) {
    // Column widths.
    let nw = fixtures
        .iter()
        .map(|f| f.name.len())
        .max()
        .unwrap_or(4)
        .max(4)
        + 1;

    fn feat_str(f: &ulottie_compiler::EmbeddedFeatures) -> String {
        let mut s = String::new();
        if f.expressions {
            s.push('E');
        } else {
            s.push('-');
        }
        if f.trim_path {
            s.push('T');
        } else {
            s.push('-');
        }
        if f.gradient {
            s.push('G');
        } else {
            s.push('-');
        }
        s
    }

    let hdr = |label: &str| format!("{:>8}", label);
    let hdr_gz = |label: &str| format!("{:>8}", label);

    println!(
        "\n {:<nw$}  {}  {}  {}  {}  {}  {}  {}  Feat  How",
        "Fixt",
        hdr("JSON"),
        hdr_gz("gz"),
        hdr("Ext"),
        hdr_gz("gz"),
        hdr("+slice"),
        hdr("Emb"),
        hdr_gz("gz"),
        nw = nw,
    );
    println!("{}", "-".repeat(nw + 80));

    for f in fixtures {
        println!(
            " {:<nw$}  {:>8}  {:>8}  {:>8}  {:>8}  {:>8}  {:>8}  {:>8}  {}  {}",
            f.name,
            fmt_bytes(f.json_raw),
            fmt_bytes(f.json_gz),
            fmt_bytes(f.extern_raw),
            fmt_bytes(f.extern_gz),
            // What a page actually downloads for *one* animation in shared
            // mode: the module plus the runtime slice it imports.
            fmt_bytes(f.extern_gz + f.slice_gz),
            fmt_bytes(f.embedded_raw),
            fmt_bytes(f.embedded_gz),
            feat_str(&f.features),
            if f.generated { "code" } else { "interp" },
            nw = nw,
        );
    }

    println!("{}", "-".repeat(nw + 80));
    // Nothing imports this — compiled output imports the entry points it
    // binds. It is the ceiling: what a page would load if one animation used
    // every capability at once.
    println!(
        " {:<nw$}  {:>8}  {:>8}  {:>8}  {:>8}",
        "runtime (all capabilities)",
        "",
        "",
        fmt_bytes(driver_raw),
        fmt_bytes(driver_gz),
        nw = nw,
    );
    println!(
        " {:<nw$}  {:>8}  {:>8}  {:>8}  {:>8}",
        "lottie.min.js",
        "",
        "",
        fmt_bytes(lottie_raw),
        fmt_bytes(lottie_gz),
        nw = nw,
    );
    println!();

    // Totals row: extern total = driver + avg fixture extern JS
    let n = fixtures.len();
    if n > 0 {
        let avg_extern_gz = fixtures.iter().map(|f| f.extern_gz).sum::<usize>() / n;
        let avg_embedded_gz = fixtures.iter().map(|f| f.embedded_gz).sum::<usize>() / n;
        let avg_json_gz = fixtures.iter().map(|f| f.json_gz).sum::<usize>() / n;
        println!("Averages ({} fixtures, gzipped):", n);
        println!(
            "  lottie-web first load : {:>8}  (lottie.min.js + {} avg JSON)",
            fmt_bytes(lottie_gz + avg_json_gz),
            fmt_bytes(avg_json_gz),
        );
        println!(
            "  ulottie extern module : {:>8}  (avg; imports the runtime as ES modules,",
            fmt_bytes(avg_extern_gz),
        );
        println!("                                    so a bundler ships only what is reached)");
        println!(
            "  ulottie embedded      : {:>8}  (self-contained, avg — this is the",
            fmt_bytes(avg_embedded_gz),
        );
        println!("                                    single-animation first load)");
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(msg: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg,
        }
    }
    fn compile(msg: String) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: msg,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}
