use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "ulottie-compiler",
    about = "AOT compiler for Lottie animations"
)]
struct Cli {
    /// Input Lottie JSON file
    input: PathBuf,

    /// Output JavaScript file (defaults to input with .js extension)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Dump the IR to stderr instead of compiling. Useful for debugging the
    /// frontend / optimization passes.
    #[arg(long)]
    emit_ir: bool,

    /// Inline a tree-shaken subset of the runtime into the compiled output
    /// instead of importing the shared `driver.js`. Produces a self-contained
    /// JS module with no external dependencies.
    #[arg(long)]
    embedded: bool,

    /// Emit the module unminified and line-oriented — one SVG element and one
    /// binding per line. Useful for reviewing what a compiler change did to the
    /// output; this is how `_fixtures/__snapshots__/` is generated.
    #[arg(long)]
    pretty: bool,

    /// Write the animation's document template — one standalone SVG with every
    /// compile-time-resolvable value in it — instead of a JS module. Renders
    /// with no script, so it works for SSR, `<noscript>`, or hydration (pair
    /// it with `--no-markup` for the module that hydrates it). Generated ids
    /// keep their `--u` marker: valid as-is, rewritten per mount by the
    /// hydrating module; a page inlining several copies *without* hydrating
    /// them should replace the marker per copy.
    #[arg(long)]
    document: bool,

    /// Byte budget for inlining the document template into the module. Above
    /// it, repeated subtrees are factored into a table the runtime expands at
    /// mount instead of being written out at every occurrence.
    #[arg(long, default_value_t = ulottie_compiler::scene::DEFAULT_INLINE_LIMIT)]
    inline_limit: usize,

    /// Comma-separated unsupported features to accept anyway, e.g.
    /// `--allow track-matte,time-remap`. Compilation otherwise fails when the
    /// source uses something the backend does not implement.
    #[arg(long, value_delimiter = ',')]
    allow: Vec<String>,

    /// Extract the initial markup into an external SVG sprite at this path,
    /// instead of carrying it in the module. The module then holds only the
    /// `<svg>` shell and clones the symbol's children in at mount, so the
    /// picture is cached and preloadable separately from the JS.
    ///
    /// Compiling several animations into the same path accumulates them into
    /// one sprite; recompiling one replaces its symbol. The sprite has to be
    /// in the document before `init()` runs.
    #[arg(long, value_name = "FILE")]
    extract: Option<PathBuf>,

    /// Symbol id to use with `--extract`. Defaults to the input file stem, and
    /// has to be unique within the sprite.
    #[arg(long, value_name = "ID")]
    symbol_id: Option<String>,

    /// Compile the hydration module for a server-rendered document: the module
    /// carries no markup at all, and `init(el)` adopts the `<svg>` already in
    /// `el` — the one `--document` wrote, served in the HTML — rewriting its
    /// id marker per mount. Strictly smaller than the default module, which
    /// carries the same document as a string it never reads on that path.
    #[arg(long, conflicts_with = "extract")]
    no_markup: bool,

    /// Force precomp instancing on. By default the compiler decides per
    /// animation, by compiling both ways and keeping the smaller compressed
    /// module — a large win on heavily-instanced files, a loss on light ones.
    #[arg(long, conflicts_with = "no_instance_precomps")]
    instance_precomps: bool,

    /// Force precomp instancing off.
    #[arg(long)]
    no_instance_precomps: bool,

    /// Extract embedded images above a size threshold (default 4096 decoded
    /// bytes) into files under this directory, written next to the output
    /// file, and reference them by URL instead of a data URI. Larger images
    /// then load concurrently with the module instead of inside it.
    ///
    /// `<DIR>/manifest.json` lists them (`url`, `file`, `mime`, `bytes`) so a
    /// web server can emit 103 Early Hints / `<link rel=preload>` for exactly
    /// what the markup will request. The files must be served alongside the
    /// module at `<DIR>/`, and the markup references them as `<DIR>/<file>`.
    #[arg(long, value_name = "DIR")]
    assets: Option<String>,

    /// Only images whose decoded size is *strictly above* this many bytes are
    /// extracted; smaller ones stay inline as data URIs (a data URI smaller
    /// than the request that would replace it is the cheaper delivery).
    /// Only meaningful with `--assets`.
    #[arg(long, default_value_t = 4096)]
    asset_threshold: usize,

    /// Compilation target: `web` (the default) emits a browser module;
    /// `reanimated-aot` emits a React Native module for react-native-svg +
    /// react-native-reanimated — always self-contained and unminified (Metro
    /// workletizes, then minifies), with the markup replaced by a static
    /// element-tree descriptor. `skia-aot` is its @shopify/react-native-skia
    /// twin: the markup becomes a display-list descriptor drawn imperatively
    /// into an SkPicture by a worklet. `rt` compiles the same display list to
    /// a binary RTDL blob rasterized natively by tiny-skia (`ulottie-rt`).
    #[arg(long, value_enum, default_value_t = TargetArg::Web)]
    target: TargetArg,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum TargetArg {
    Web,
    ReanimatedAot,
    SkiaAot,
    Rt,
}

/// The asset options implied by `--assets <DIR>`: on, with `url_base` taken
/// from the directory (slash-terminated) so the markup and the manifest agree.
fn asset_options(cli: &Cli) -> ulottie_compiler::AssetOptions {
    match &cli.assets {
        Some(dir) => ulottie_compiler::AssetOptions {
            extract: true,
            url_base: if dir.ends_with('/') {
                dir.clone()
            } else {
                format!("{dir}/")
            },
            threshold: cli.asset_threshold,
        },
        None => ulottie_compiler::AssetOptions::default(),
    }
}

/// Write the extracted assets and their manifest under `dir`, relative to the
/// output file the markup ships next to — the same convention `--extract`
/// uses for the sprite.
fn write_assets(
    output: &std::path::Path,
    dir: &str,
    assets: &[ulottie_compiler::ExtractedAsset],
    manifest: &str,
) -> Result<()> {
    if assets.is_empty() && manifest == "[]" {
        return Ok(());
    }
    let root = output.parent().unwrap_or(std::path::Path::new(".")).join(dir);
    std::fs::create_dir_all(&root)?;
    for a in assets {
        std::fs::write(root.join(&a.name), &a.bytes)?;
    }
    std::fs::write(root.join("manifest.json"), manifest)?;
    eprintln!(
        "Extracted {} asset(s) -> {}",
        assets.len(),
        root.display()
    );
    Ok(())
}

/// The symbol id for `--extract`: explicit, or the input's file stem.
fn symbol_id(cli: &Cli) -> Result<String> {
    if let Some(id) = &cli.symbol_id {
        return Ok(id.clone());
    }
    cli.input
        .file_stem()
        .and_then(|s| s.to_str())
        .map(String::from)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot derive a symbol id from {:?}; pass --symbol-id",
                cli.input
            )
        })
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let json = std::fs::read_to_string(&cli.input)?;

    let mut allow = std::collections::BTreeSet::new();
    for name in &cli.allow {
        let f = ulottie_compiler::support::Feature::from_name(name)
            .ok_or_else(|| anyhow::anyhow!("unknown feature `{name}`"))?;
        allow.insert(f);
    }

    if cli.emit_ir {
        let anim: ulottie_compiler::lottie::Animation = serde_json::from_str(&json)?;
        let module = ulottie_compiler::ir::lower(&anim)?;
        eprintln!("{module:#?}");
        return Ok(());
    }

    let target = match cli.target {
        TargetArg::Web => ulottie_compiler::Target::Web,
        TargetArg::ReanimatedAot => ulottie_compiler::Target::ReanimatedAot,
        TargetArg::SkiaAot => ulottie_compiler::Target::SkiaAot,
        TargetArg::Rt => ulottie_compiler::Target::Rt,
    };
    if target != ulottie_compiler::Target::Web {
        // These flags shape a browser module; the RN target is always
        // self-contained, fully inlined, never instanced, tree-descriptored.
        for (set, flag) in [
            (cli.embedded, "--embedded"),
            (cli.document, "--document"),
            (cli.extract.is_some(), "--extract"),
            (cli.no_markup, "--no-markup"),
            (cli.assets.is_some(), "--assets"),
            (cli.instance_precomps, "--instance-precomps"),
        ] {
            anyhow::ensure!(
                !set,
                "{flag} does not apply to a React Native target (the RN module is always \
                 self-contained, fully inlined and never instanced)"
            );
        }
    }

    let runtime_mode = if cli.embedded {
        ulottie_compiler::RuntimeMode::Embedded
    } else {
        ulottie_compiler::RuntimeMode::Extern
    };
    ulottie_compiler::check_supported_for(&json, &allow, target)?;

    if cli.document {
        let output = cli
            .output
            .clone()
            .unwrap_or_else(|| cli.input.with_extension("svg"));
        let assets = asset_options(&cli);
        if assets.extract {
            let out = ulottie_compiler::compile_document_with(&json, &ulottie_compiler::CompileOptions {
                assets,
                ..Default::default()
            })?;
            let svg = if cli.pretty {
                ulottie_compiler::markup_pretty(&out.svg)
            } else {
                out.svg
            };
            std::fs::write(&output, &svg)?;
            write_assets(&output, cli.assets.as_deref().unwrap(), &out.assets, &out.manifest)?;
        } else {
            let svg = if cli.pretty {
                ulottie_compiler::compile_document_pretty(&json)?
            } else {
                ulottie_compiler::compile_document(&json)?
            };
            std::fs::write(&output, &svg)?;
        }
        eprintln!("Wrote {} -> {}", cli.input.display(), output.display());
        return Ok(());
    }

    let markup = match (&cli.extract, cli.no_markup) {
        (Some(_), _) => ulottie_compiler::MarkupMode::Extracted(symbol_id(&cli)?),
        (None, true) => ulottie_compiler::MarkupMode::None,
        (None, false) => ulottie_compiler::MarkupMode::Inline,
    };

    let options = ulottie_compiler::CompileOptions {
        runtime_mode,
        minify: !cli.pretty,
        inline_limit: cli.inline_limit,
        markup: markup.clone(),
        allow,
        instance_precomps: match (cli.instance_precomps, cli.no_instance_precomps) {
            (true, _) => ulottie_compiler::Instancing::Always,
            (_, true) => ulottie_compiler::Instancing::Never,
            _ => ulottie_compiler::Instancing::Auto,
        },
        assets: asset_options(&cli),
        target,
    };
    let output = cli.output.clone().unwrap_or_else(|| cli.input.with_extension("js"));

    let (js, extracted) = if options.assets.extract {
        let out = ulottie_compiler::compile_with_output(&json, &options)?;
        (out.module.clone(), Some(out))
    } else {
        (ulottie_compiler::compile_with(&json, &options)?, None)
    };
    std::fs::write(&output, &js)?;
    if let Some(out) = &extracted {
        write_assets(
            &output,
            cli.assets.as_deref().unwrap(),
            &out.assets,
            &out.manifest,
        )?;
    }

    eprintln!("Compiled {} -> {}", cli.input.display(), output.display());

    if let (Some(path), ulottie_compiler::MarkupMode::Extracted(id)) = (&cli.extract, &markup) {
        let symbol_out = ulottie_compiler::compile_symbol_with(&json, id, &options)?;
        let symbol = symbol_out.svg;
        let sprite = match std::fs::read_to_string(path) {
            Ok(existing) => ulottie_compiler::scene::merge_sprite(&existing, &symbol, id),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ulottie_compiler::sprite(&[symbol])
            }
            Err(e) => return Err(e.into()),
        };
        let sprite = if cli.pretty {
            ulottie_compiler::backend::pretty::markup_plain(&sprite)
        } else {
            sprite
        };
        std::fs::write(path, &sprite)?;
        if options.assets.extract {
            write_assets(
                path,
                cli.assets.as_deref().unwrap(),
                &symbol_out.assets,
                &symbol_out.manifest,
            )?;
        }
        eprintln!("Extracted markup -> {} (#{id})", path.display());
    }

    Ok(())
}
