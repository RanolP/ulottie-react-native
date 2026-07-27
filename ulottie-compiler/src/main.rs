use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[derive(Parser)]
#[command(name = "ulottie-compiler", about = "AOT compiler for Lottie animations")]
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
    /// with no script, so it works for SSR, `<noscript>`, or hydration.
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

    /// Force precomp instancing on. By default the compiler decides per
    /// animation, by compiling both ways and keeping the smaller compressed
    /// module — a large win on heavily-instanced files, a loss on light ones.
    #[arg(long, conflicts_with = "no_instance_precomps")]
    instance_precomps: bool,

    /// Force precomp instancing off.
    #[arg(long)]
    no_instance_precomps: bool,
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
        .ok_or_else(|| anyhow::anyhow!("cannot derive a symbol id from {:?}; pass --symbol-id", cli.input))
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

    let runtime_mode = if cli.embedded {
        ulottie_compiler::RuntimeMode::Embedded
    } else {
        ulottie_compiler::RuntimeMode::Extern
    };
    ulottie_compiler::check_supported(&json, &allow)?;

    if cli.document {
        let svg = if cli.pretty {
            ulottie_compiler::compile_document_pretty(&json)?
        } else {
            ulottie_compiler::compile_document(&json)?
        };
        let output = cli.output.unwrap_or_else(|| cli.input.with_extension("svg"));
        std::fs::write(&output, &svg)?;
        eprintln!("Wrote {} -> {}", cli.input.display(), output.display());
        return Ok(());
    }

    let markup = match &cli.extract {
        None => ulottie_compiler::MarkupMode::Inline,
        Some(_) => ulottie_compiler::MarkupMode::Extracted(symbol_id(&cli)?),
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
    };
    let js = ulottie_compiler::compile_with(&json, &options)?;

    let output = cli.output.unwrap_or_else(|| cli.input.with_extension("js"));
    std::fs::write(&output, &js)?;

    eprintln!(
        "Compiled {} -> {}",
        cli.input.display(),
        output.display()
    );

    if let (Some(path), ulottie_compiler::MarkupMode::Extracted(id)) = (&cli.extract, &markup) {
        let symbol = ulottie_compiler::compile_symbol(&json, id)?;
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
        eprintln!("Extracted markup -> {} (#{id})", path.display());
    }

    Ok(())
}
