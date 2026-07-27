//! Shared test helpers.

use std::collections::BTreeSet;

use ulottie_compiler::support::Feature;

/// Unsupported features a fixture is allowed to use, from
/// `_fixtures/allowances.json`. The corpus is compiled with these and nothing
/// else, so a newly-unsupported feature fails the build rather than changing a
/// render quietly.
pub fn allowances(name: &str) -> BTreeSet<Feature> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures/allowances.json");
    let doc: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    doc.get(name)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .filter_map(Feature::from_name)
                .collect()
        })
        .unwrap_or_default()
}
