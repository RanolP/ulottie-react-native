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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let json = std::fs::read_to_string(&cli.input)?;

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
    let options = ulottie_compiler::CompileOptions { runtime_mode };
    let js = ulottie_compiler::compile_with(&json, &options)?;

    let output = cli.output.unwrap_or_else(|| cli.input.with_extension("js"));
    std::fs::write(&output, &js)?;

    eprintln!(
        "Compiled {} -> {}",
        cli.input.display(),
        output.display()
    );

    Ok(())
}
