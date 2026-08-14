//! IR lowering smoke tests. Every fixture must lower without panicking and
//! the result must be self-consistent (parents resolve to existing layers,
//! expression IDs are valid, etc.).

use std::fs;
use ulottie_compiler::ir;
use ulottie_compiler::lottie::Animation;

fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures")
        .join("animations")
}

fn lower_fixture(name: &str) -> ir::Module {
    let path = fixtures_dir().join(format!("{name}.json"));
    let json =
        fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing fixture: {}", path.display()));
    let anim: Animation = serde_json::from_str(&json).expect("parse animation");
    ir::lower(&anim).expect("lower should succeed")
}

fn assert_module_well_formed(m: &ir::Module) {
    // Every layer's parent (if set) must point to a layer that exists.
    let layer_count = m.layers.len();
    for layer in &m.layers {
        if let Some(parent) = layer.parent {
            assert!(
                (parent.0 as usize) < layer_count,
                "layer {:?} has parent {:?} that doesn't exist (n_layers={})",
                layer.id,
                parent,
                layer_count
            );
        }
    }

    // Every expression id stored in a property must be a valid index into the
    // expression table.
    let expr_count = m.expressions.len();
    fn check_prop<T: Clone>(p: &ir::Property<T>, expr_count: usize) {
        if let ir::Property::Expression { expr, .. } = p {
            assert!(
                (expr.0 as usize) < expr_count,
                "expression id {:?} out of bounds (n_exprs={})",
                expr,
                expr_count
            );
        }
    }
    for layer in &m.layers {
        check_prop(&layer.transform.anchor, expr_count);
        check_prop(&layer.transform.position, expr_count);
        check_prop(&layer.transform.scale, expr_count);
        check_prop(&layer.transform.rotation, expr_count);
        check_prop(&layer.transform.opacity, expr_count);
        if let ir::LayerKind::Shape { shapes } = &layer.kind {
            check_shapes(shapes, expr_count);
        }
    }

    fn check_shapes(shapes: &[ir::ShapeNode], expr_count: usize) {
        for s in shapes {
            match s {
                ir::ShapeNode::Group { items, .. } => check_shapes(items, expr_count),
                ir::ShapeNode::Path { ks, .. } => check_prop(ks, expr_count),
                ir::ShapeNode::Ellipse { size, position, .. } => {
                    check_prop(size, expr_count);
                    check_prop(position, expr_count);
                }
                ir::ShapeNode::Rectangle {
                    size,
                    position,
                    radius,
                    ..
                } => {
                    check_prop(size, expr_count);
                    check_prop(position, expr_count);
                    check_prop(radius, expr_count);
                }
                ir::ShapeNode::Transform { transform, .. } => {
                    check_prop(&transform.position, expr_count);
                    check_prop(&transform.rotation, expr_count);
                }
                _ => {}
            }
        }
    }
}

macro_rules! lower_test {
    ($name:ident, $fixture:literal, $min_layers:expr) => {
        #[test]
        fn $name() {
            let m = lower_fixture($fixture);
            assert_module_well_formed(&m);
            assert!(
                m.layers.len() >= $min_layers,
                "{}: expected at least {} layers, got {}",
                $fixture,
                $min_layers,
                m.layers.len()
            );
        }
    };
}

// Sanity smoke tests with a minimum layer-count expectation so we'd notice if
// lowering silently dropped layers.
lower_test!(lower_bouncing_ball, "boucing_ball", 2);
lower_test!(lower_ellipse, "ellipse", 1);
lower_test!(lower_fill, "fill", 1);
lower_test!(lower_lights, "lights", 5);
lower_test!(lower_lottie_logo_1, "lottie_logo_1", 5);
lower_test!(lower_precomp_star_circle, "precomp_star_circle", 1);
lower_test!(lower_rectangle, "rectangle", 1);
// Ripple has only 2 top-level layers; the rest live inside precomp assets.
lower_test!(lower_ripple, "ripple", 2);
lower_test!(lower_starfish, "starfish", 2);
lower_test!(lower_trim_path, "trim_path", 1);

/// Lights has expressions on bulb positions, on the wire's path, and on the
/// null layers. Asserts the table actually got populated.
#[test]
fn lights_has_expressions() {
    let m = lower_fixture("lights");
    assert!(
        !m.expressions.is_empty(),
        "lights should have at least one expression after lowering"
    );
}

/// Parent links must thread correctly: the light shape layers in `lights.json`
/// are parented to their null counterparts.
#[test]
fn lights_parents_resolve() {
    let m = lower_fixture("lights");
    let with_parent = m.layers.iter().filter(|l| l.parent.is_some()).count();
    assert!(
        with_parent > 0,
        "expected at least one parented layer in lights"
    );
}

/// Ripple's dots all live in a precomp asset; the IR must lower the asset's
/// nested layers, not just the top-level layers.
#[test]
fn ripple_precomp_assets_lowered() {
    let m = lower_fixture("ripple");
    let asset_layers: usize = m
        .assets
        .iter()
        .filter_map(|a| match &a.kind {
            ir::AssetKind::Precomp { layers } => Some(layers.len()),
            _ => None,
        })
        .sum();
    assert!(
        asset_layers > 20,
        "ripple's precomp assets should contain many layers, got {asset_layers}"
    );
}
