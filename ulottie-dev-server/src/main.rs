//! Dev server for the visual comparison harness.
//!
//! Two modes:
//!
//!   --mode api (default)   exposes /compile + /.output/* for live Rust
//!                          iteration, and injects `<script>` into HTML
//!                          responses that redirects window.ulottieCompile
//!                          at POST /compile.
//!   --mode static          serves public/ as-is, no /compile, no HTML
//!                          rewriting. Use to verify that the standalone
//!                          static deployment (CDN-ready) still works —
//!                          the page falls back to its in-browser wasm
//!                          bootstrap.
//!
//! URL layout (api mode):
//!
//!   /                       redirect → /compare-all.html
//!   /compile (POST)         body = raw Lottie JSON
//!   /.output/<id>.js        compiled JS (lazy for fixtures, eager for uploads)
//!   /.output/<id>.json      fixture source (registered) or upload source
//!   /.output/driver.js      minified shared runtime (served from memory)
//!   /_fixtures/<name>.json  registered fixture source
//!   /<anything-else>        static UI under public/
//!
//! On-disk locations:
//!
//!   public/                 static UI source (compare-all.html, app.js,
//!                           bootstrap-default.js, lottie.min.js, wasm/)
//!   __fixtures__/           pre-registered Lottie sources
//!                            (`<workspace>/_fixtures/animations/`)
//!   .output/                disk cache: compiled JS + upload sources
//!                            (api mode only)
//!
//! `__fixtures__` is the conceptual name for the fixture source location,
//! not a URL prefix — both `/.output/<name>.json` (api mode) and
//! `/_fixtures/<name>.json` (both modes) resolve to it.
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
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use clap::{Parser, ValueEnum};
use flate2::{Compression, write::GzEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::fs;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

#[derive(Parser, Debug)]
#[command(name = "ulottie-dev-server")]
struct Args {
    /// Port to listen on.
    #[arg(long, default_value_t = 4567)]
    port: u16,

    /// Server mode. `api` exposes /compile and rewrites HTML responses to
    /// route the page's compile backend at the server. `static` serves
    /// public/ as-is so the in-browser wasm bootstrap takes over — useful
    /// for testing the CDN-ready deployment shape locally.
    #[arg(long, value_enum, default_value_t = Mode::Api)]
    mode: Mode,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Mode {
    /// Live Rust compilation via POST /compile.
    Api,
    /// Pure static serving; client uses its own wasm compiler bundle.
    Static,
}

/// Pre-registered file locations, resolved once at startup.
#[derive(Debug, Clone)]
struct PathLayout {
    public_dir: PathBuf,
    /// Pre-registered fixture sources — the "__fixtures__" location.
    fixtures_dir: PathBuf,
    /// Disk cache for compiled JS + ad-hoc upload sources.
    output_dir: PathBuf,
    /// Vendored lottie-web bundle inside `public/`. Served by the static
    /// chain at `/lottie.min.js`; measured as the "original Lottie runtime"
    /// baseline in size reports.
    lottie_web_bundle: PathBuf,
}

impl PathLayout {
    fn from_crate_dir(crate_dir: &StdPath) -> Result<Self> {
        let workspace = crate_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("crate has no parent"))?
            .to_path_buf();
        let public_dir = crate_dir.join("public");
        Ok(Self {
            lottie_web_bundle: public_dir.join("lottie.min.js"),
            public_dir,
            fixtures_dir: workspace.join("_fixtures").join("animations"),
            output_dir: crate_dir.join(".output"),
        })
    }
}

/// Minified driver.js, computed once on first access. Same string used to
/// serve `/.output/driver.js` and to measure the extern-runtime size in
/// `/compile` responses, so both stay in sync without a disk round-trip.
fn minified_driver() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(ulottie_compiler::minified_driver).as_str()
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ulottie_dev_server=info,tower_http=warn".into()),
        )
        .init();

    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let paths = Arc::new(PathLayout::from_crate_dir(&crate_dir)?);
    if !paths.public_dir.is_dir() {
        anyhow::bail!("public/ missing: {}", paths.public_dir.display());
    }

    // Both modes can load fixture JSON via /_fixtures/<name>.json. The
    // static-mode demo still needs them; a real CDN deploy would copy
    // them into public/_fixtures/ at build time.
    let fixtures_service = ServeDir::new(&paths.fixtures_dir);

    let mut router = Router::new()
        .route("/", get(|| async { Redirect::temporary("/compare-all.html") }))
        .nest_service("/_fixtures", fixtures_service);

    if args.mode == Mode::Api {
        fs::create_dir_all(&paths.output_dir).await?;

        // /.output/<file>: .output/ wins on hit; fixture source dir wins on
        // miss so fixture JSON falls through naturally without being copied.
        let output_service = ServeDir::new(&paths.output_dir)
            .fallback(ServeDir::new(&paths.fixtures_dir));

        router = router
            .route("/compile", post(compile_handler))
            .route("/.output/driver.js", get(serve_driver))
            .nest_service("/.output", output_service)
            // Shadow `public/compiler.js` (wasm + worker) with a tiny
            // fetch-based ESM. app.js's `import './compiler.js'`
            // resolves here in api mode; the wasm bundle never loads.
            .route("/compiler.js", get(serve_api_compiler))
            // ensure_compiled only needs to wrap /.output/* — but layering
            // it before fallback_service is the simplest way to scope it
            // to routes that exist before the fallback runs.
            .layer(middleware::from_fn_with_state(paths.clone(), ensure_compiled));
    }

    // fallback_service serves the static UI; in static mode this owns
    // `compiler.js` as well, dishing out the on-disk wasm/worker variant.
    let app = router
        .fallback_service(ServeDir::new(&paths.public_dir))
        .layer(middleware::from_fn(no_store))
        .layer(TraceLayer::new_for_http())
        .with_state(paths.clone());

    let addr: SocketAddr = ([127, 0, 0, 1], args.port).into();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("ulottie-dev-server listening on http://{addr}  (mode: {:?})", args.mode);
    eprintln!("  public:       {}", paths.public_dir.display());
    eprintln!("  __fixtures__: {}", paths.fixtures_dir.display());
    if args.mode == Mode::Api {
        eprintln!("  .output:      {}", paths.output_dir.display());
        eprintln!("  POST /compile               — body = raw Lottie JSON");
        eprintln!("  GET  /.output/<id>.{{json,js}} — fixture stem or upload hash");
    } else {
        eprintln!("  (static mode — page uses ./wasm/ulottie_compiler.js)");
    }
    axum::serve(listener, app).await?;
    Ok(())
}

/// The api-mode shim served at `/compiler.js`. Exposes the same
/// `{ compile, ready }` interface as the on-disk wasm + worker
/// variant in `public/compiler.js`, but POSTs to /compile instead.
/// app.js's `import './compiler.js'` resolves here in api mode.
const API_COMPILER_JS: &str = r#"export const ready = Promise.resolve();
export async function compile(jsonText) {
  const r = await fetch('/compile', { method: 'POST', body: jsonText });
  if (!r.ok) throw new Error(await r.text());
  return await r.json();
}
"#;

async fn serve_api_compiler() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        API_COMPILER_JS,
    )
}

/// Serve the minified shared runtime. Compiled animation modules do
/// `import { run } from './driver.js'` which resolves to this route; the
/// bytes come from process memory (computed lazily on first request).
async fn serve_driver() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        minified_driver(),
    )
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// If the URI is `/.output/<name>.js` and `_fixtures/animations/<name>.json`
/// is a registered fixture, ensure `.output/<name>.js` is at least as fresh.
/// Then the static service serves it.
async fn ensure_compiled(
    State(paths): State<Arc<PathLayout>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let uri_path = req.uri().path().to_string();
    if let Some(name) = uri_path
        .strip_prefix("/.output/")
        .and_then(|p| p.strip_suffix(".js"))
        .filter(|p| !p.contains('/') && !p.contains(".."))
    {
        let src = paths.fixtures_dir.join(format!("{name}.json"));
        if src.is_file() {
            let cache = paths.output_dir.join(format!("{name}.js"));
            if let Err(e) = compile_if_stale(&src, &cache, name).await {
                return e.into_response();
            }
        }
    }
    next.run(req).await
}

/// Stamp `Cache-Control: no-store` on every response.
async fn no_store(req: Request<axum::body::Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    res
}

// ---------------------------------------------------------------------------
// Compile
// ---------------------------------------------------------------------------

/// Disk-only cache. If the cache file is at least as new as the source, no-op.
/// Otherwise recompile and write.
async fn compile_if_stale(
    src: &StdPath,
    cache_path: &StdPath,
    name: &str,
) -> Result<(), ApiError> {
    let src_mtime = fs::metadata(src)
        .await
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if let Ok(meta) = fs::metadata(cache_path).await {
        let cache_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if cache_mtime >= src_mtime {
            return Ok(());
        }
    }
    let json = fs::read_to_string(src)
        .await
        .map_err(|e| ApiError::compile(format!("read {name}: {e}")))?;
    let js = ulottie_compiler::compile(&json)
        .map_err(|e| ApiError::compile(format!("compile {name}: {e}")))?;
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent).await.ok();
    }
    fs::write(cache_path, js)
        .await
        .map_err(|e| ApiError::compile(format!("write {name}.js: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// POST /compile
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompileResponse {
    id: String,
    json_url: String,
    js_url: String,
    /// URL for the embedded (tree-shaken, self-contained) variant.
    js_embedded_url: String,
    name: Option<String>,
    total_frames: f64,
    /// Byte-level sizes for the UI's payload-budget panel. Raw + gzipped.
    /// Computed here so the client never needs a follow-up round-trip.
    sizes: Sizes,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Sizes {
    /// Lottie source JSON.
    json: SizeEntry,
    /// ulottie compiled JS payload (extern mode — imports driver.js).
    js: SizeEntry,
    /// ulottie shared runtime (`driver.js`).
    ulottie_runtime: SizeEntry,
    /// ulottie compiled JS with embedded, tree-shaken & minified runtime.
    /// Self-contained — no external driver.js dependency.
    js_embedded: SizeEntry,
    /// What the embedded build actually inlines, vs what it tree-shook.
    /// The UI uses this to label the embedded row with its included /
    /// stripped feature set so the tree-shaking advantage is legible
    /// without doing arithmetic on the size numbers.
    embedded_features: EmbeddedFeaturesEntry,
    /// lottie-web runtime — the baseline a "regular Lottie" pipeline ships
    /// for the same fixture. Lets the UI present an apples-to-apples
    /// first-load delta (`json + lottie_runtime` vs `js + ulottie_runtime`).
    lottie_runtime: SizeEntry,
}

/// Per-feature inclusion + cost report for the embedded build. `included` is
/// what the data backend determined the animation needs; `byteCostRaw` is the
/// number of bytes the feature contributes to the embedded payload (computed
/// by diffing the embedded build with vs without each feature).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EmbeddedFeaturesEntry {
    included: ulottie_compiler::EmbeddedFeatures,
    /// Raw byte cost of each feature in the embedded build. Computed by
    /// subtracting the size of the embedded build without the feature from
    /// the size with it. The UI shows these so the user can see "expressions
    /// is the biggest single saving here, gradient is small".
    cost_raw: FeatureCost,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FeatureCost {
    expressions: i64,
    trim_path: i64,
    gradient: i64,
}

/// Bytes each optional feature costs the embedded runtime, measured against
/// the all-features-on baseline. These are intrinsic properties of `driver.js`
/// (and the minifier), so we cache the result after the first compute — the
/// oxc minifier round-trip is ~50 ms per call.
fn feature_costs() -> &'static FeatureCost {
    use std::sync::OnceLock;
    static CACHE: OnceLock<FeatureCost> = OnceLock::new();
    CACHE.get_or_init(|| {
        let all_on = ulottie_compiler::EmbeddedFeatures {
            expressions: true,
            trim_path: true,
            gradient: true,
        };
        let full = ulottie_compiler::embedded_runtime_size(all_on) as i64;
        let cost = |omitted: ulottie_compiler::EmbeddedFeatures| {
            full - ulottie_compiler::embedded_runtime_size(omitted) as i64
        };
        FeatureCost {
            expressions: cost(ulottie_compiler::EmbeddedFeatures { expressions: false, ..all_on }),
            trim_path: cost(ulottie_compiler::EmbeddedFeatures { trim_path: false, ..all_on }),
            gradient: cost(ulottie_compiler::EmbeddedFeatures { gradient: false, ..all_on }),
        }
    })
}

#[derive(Serialize)]
struct SizeEntry {
    raw: usize,
    gzipped: usize,
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

fn size_entry(data: &[u8]) -> SizeEntry {
    SizeEntry { raw: data.len(), gzipped: gzip_size(data) }
}

async fn compile_handler(
    State(paths): State<Arc<PathLayout>>,
    body: Bytes,
) -> Result<Json<CompileResponse>, ApiError> {
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
    compile_if_stale(&json_path, &js_path, &id).await?;

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
    let embedded_options = ulottie_compiler::CompileOptions {
        runtime_mode: ulottie_compiler::RuntimeMode::Embedded,
    };
    let embedded_js = ulottie_compiler::compile_with(json_text, &embedded_options)
        .unwrap_or_default();
    let embedded_path = paths.output_dir.join(format!("{id}.embedded.js"));
    fs::write(&embedded_path, &embedded_js).await.ok();
    let embedded_bytes = embedded_js.as_bytes();

    let included = ulottie_compiler::analyze_features(json_text)
        .ok()
        .flatten()
        .unwrap_or_default();

    let sizes = Sizes {
        json: size_entry(&json_compact),
        js: size_entry(&js_bytes),
        ulottie_runtime: size_entry(ulottie_runtime_bytes),
        js_embedded: size_entry(embedded_bytes),
        embedded_features: EmbeddedFeaturesEntry {
            included,
            cost_raw: feature_costs().clone(),
        },
        lottie_runtime: size_entry(&lottie_runtime_bytes),
    };

    let total_frames = (header.op - header.ip).max(0.0);
    Ok(Json(CompileResponse {
        id: id.clone(),
        json_url: format!("/.output/{id}.json"),
        js_url: format!("/.output/{id}.js"),
        js_embedded_url: format!("/.output/{id}.embedded.js"),
        name: header.nm,
        total_frames,
        sizes,
    }))
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
// Errors
// ---------------------------------------------------------------------------

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(msg: String) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: msg }
    }
    fn compile(msg: String) -> Self {
        Self { status: StatusCode::UNPROCESSABLE_ENTITY, message: msg }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}
