//! Generate the demo's TypeScript bindings from the Rust `/compile` contract.
//!
//! `src/contract.rs` is the single definition; this emits its TypeScript types
//! and their rkyv decoder, so the page cannot drift from the server. Decode
//! only — the page reads responses and never writes them.

fn main() -> Result<(), rkyv_js_codegen::Error> {
    println!("cargo:rerun-if-changed=src/contract.rs");

    // Generated, and gitignored — the directory may not exist on a fresh
    // checkout.
    std::fs::create_dir_all("demo/src/generated")?;

    let mut codegen = rkyv_js_codegen::CodeGenerator::new();
    codegen.set_direction(rkyv_js_codegen::Direction::Decode);
    codegen
        .add_source_file("src/contract.rs")?
        .write_to_file("demo/src/generated/bindings.ts")?;
    Ok(())
}
