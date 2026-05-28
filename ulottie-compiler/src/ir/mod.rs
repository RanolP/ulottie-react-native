//! μlottie intermediate representation.
//!
//! See `types.rs` for the data types and `lower.rs` for the Lottie → IR
//! lowering pass. Optimization passes that consume the IR live in
//! `crate::opt`; the IR → JS backend lives in `crate::backend`.

pub mod lower;
pub mod types;

pub use lower::lower;
pub use types::*;
