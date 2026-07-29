//! Folding tests, written against bodies taken verbatim from the corpus.
//!
//! The point of the pass is what it decides on real Bodymovin output, so the
//! inputs here are copied from `_fixtures/animations/` rather than invented.
//! Every one of them ships today.

use super::*;

fn effect(name: &str, params: &[(&str, Option<f64>)]) -> ir::Effect {
    ir::Effect {
        name: Some(name.to_string()),
        match_name: Some(name.to_string()),
        ty: 5,
        index: None,
        enabled: true,
        parameters: params
            .iter()
            .map(|(n, v)| ir::EffectParam {
                name: Some(n.to_string()),
                match_name: Some(n.to_string()),
                ty: 0,
                index: None,
                value: ir::EffectValue::Scalar(match v {
                    Some(x) => ir::Property::Static(*x),
                    // An animated parameter: no single value to fold to.
                    None => ir::Property::Animated(ir::Keyframes { frames: Vec::new() }),
                }),
            })
            .collect(),
    }
}

fn facts<'a>(effects: &'a [ir::Effect], num_keys: usize) -> Facts<'a> {
    Facts { effects, num_keys, value_range: None }
}

/// `lights`, used on five layers. The guard is an effect checkbox and a
/// keyframe count — both known — so the whole thing folds to one branch.
const LOOP_TOGGLE: &str = r#"
var $bm_rt;
if (thisProperty.propertyGroup(1)('Pseudo/ADBE Trace Path-0002') == true && thisProperty.numKeys > 1) {
    $bm_rt = thisProperty.loopOut('cycle');
} else {
    $bm_rt = value;
}
"#;

#[test]
fn a_loop_toggle_that_is_off_deletes_the_expression() {
    let fx = [effect("Trace", &[("Pseudo/ADBE Trace Path-0002", Some(0.0))])];
    assert_eq!(fold(LOOP_TOGGLE, &facts(&fx, 4)), Outcome::Identity);
}

#[test]
fn a_property_with_one_keyframe_deletes_it_too() {
    // The toggle is on, but `numKeys > 1` is false, so the loop cannot apply.
    let fx = [effect("Trace", &[("Pseudo/ADBE Trace Path-0002", Some(1.0))])];
    assert_eq!(fold(LOOP_TOGGLE, &facts(&fx, 1)), Outcome::Identity);
}

#[test]
fn a_loop_toggle_that_is_on_stays() {
    // `loopOut` genuinely varies with the frame — folding it away would be a
    // rendering change, so the expression ships.
    let fx = [effect("Trace", &[("Pseudo/ADBE Trace Path-0002", Some(1.0))])];
    assert_eq!(fold(LOOP_TOGGLE, &facts(&fx, 4)), Outcome::Open);
}

#[test]
fn an_unknown_toggle_is_not_guessed() {
    // No such effect on this layer: the guard is undecidable, so is the body.
    let fx = [effect("Something else", &[("other", Some(1.0))])];
    assert_eq!(fold(LOOP_TOGGLE, &facts(&fx, 4)), Outcome::Open);
}

#[test]
fn an_ambiguous_name_is_refused_rather_than_picked() {
    // Two instances of the same pseudo effect answer to the same match name.
    // Picking either would be a coin flip on which one drives the property.
    let fx = [
        effect("Trace A", &[("Pseudo/ADBE Trace Path-0002", Some(0.0))]),
        effect("Trace B", &[("Pseudo/ADBE Trace Path-0002", Some(1.0))]),
    ];
    assert_eq!(fold(LOOP_TOGGLE, &facts(&fx, 4)), Outcome::Open);
}

/// `lights` and `starfish`, three uses. Bodymovin emits this on opacity
/// whether or not the keyframes ever leave the range.
const CLAMP: &str = "var $bm_rt;\n$bm_rt = clamp(value, 0, 100);";

#[test]
fn clamping_a_property_to_a_range_it_never_leaves_is_nothing() {
    let f = Facts { effects: &[], num_keys: 3, value_range: Some((0.0, 100.0)) };
    assert_eq!(fold(CLAMP, &f), Outcome::Identity);
}

#[test]
fn clamping_one_that_does_leave_it_stays() {
    let f = Facts { effects: &[], num_keys: 3, value_range: Some((-10.0, 120.0)) };
    assert_eq!(fold(CLAMP, &f), Outcome::Open);
}

#[test]
fn clamping_an_unknown_range_stays() {
    let f = Facts { effects: &[], num_keys: 3, value_range: None };
    assert_eq!(fold(CLAMP, &f), Outcome::Open);
}

/// `starfish` and `lights`. The sliders are static, so the guard `0 < numKeys`
/// decides the whole body — including the `try`/`catch` around it.
const OVERSHOOT: &str = r#"
var $bm_rt;
var amp, freq, decay, n, t, v;
try {
    amp = div(effect('Position - Overshoot')('ADBE Slider Control-0001'), 2.5), freq = div(effect('Position - Bounce')('ADBE Slider Control-0001'), 20), decay = div(effect('Position - Friction')('ADBE Slider Control-0001'), 20), n = 0, 0 < numKeys && (n = nearestKey(time).index, key(n).time > time && n--), t = 0 === n ? 0 : time - key(n).time, $bm_rt = 0 < n ? (v = velocityAtTime(sub(key(n).time, div(thisComp.frameDuration, 10))), sum(value, div(mul(mul(div(v, 100), amp), Math.sin(mul(mul(mul(freq, t), 2), Math.PI))), Math.exp(mul(decay, t))))) : value;
} catch (e$$4) {
    $bm_rt = value = value;
}
"#;

#[test]
fn an_overshoot_on_a_property_with_no_keyframes_is_nothing() {
    // `0 < numKeys` is false, so `n` stays 0, so the ternary takes `value`.
    let fx = [
        effect("Position - Overshoot", &[("ADBE Slider Control-0001", Some(35.0))]),
        effect("Position - Bounce", &[("ADBE Slider Control-0001", Some(60.0))]),
        effect("Position - Friction", &[("ADBE Slider Control-0001", Some(20.0))]),
    ];
    assert_eq!(fold(OVERSHOOT, &facts(&fx, 0)), Outcome::Identity);
}

#[test]
fn an_overshoot_with_keyframes_stays() {
    // `nearestKey(time)` depends on the frame; nothing here can decide it.
    let fx = [
        effect("Position - Overshoot", &[("ADBE Slider Control-0001", Some(35.0))]),
        effect("Position - Bounce", &[("ADBE Slider Control-0001", Some(60.0))]),
        effect("Position - Friction", &[("ADBE Slider Control-0001", Some(20.0))]),
    ];
    assert_eq!(fold(OVERSHOOT, &facts(&fx, 5)), Outcome::Open);
}

#[test]
fn an_animated_slider_is_not_a_constant() {
    let fx = [effect("Fade", &[("ADBE Slider Control-0001", None)])];
    let body = "var $bm_rt;\n$bm_rt = effect('Fade')('ADBE Slider Control-0001');";
    assert_eq!(fold(body, &facts(&fx, 0)), Outcome::Open);
}

#[test]
fn a_static_slider_folds_to_its_value() {
    let fx = [effect("Fade", &[("ADBE Slider Control-0001", Some(42.0))])];
    let body = "var $bm_rt;\n$bm_rt = div(effect('Fade')('ADBE Slider Control-0001'), 2);";
    assert_eq!(fold(body, &facts(&fx, 0)), Outcome::Constant(21.0));
}

#[test]
fn a_body_that_reaches_another_layer_stays() {
    // `thisComp.layer(…)` is resolvable in principle but not folded here: the
    // result is a path sampled per frame, not a constant.
    let body = r#"
var $bm_rt;
var pathLayer = thisComp.layer('wire');
$bm_rt = pathLayer.toComp(pathLayer('ADBE Root Vectors Group')(1).pointOnPath(0.5));
"#;
    assert_eq!(fold(body, &facts(&[], 0)), Outcome::Open);
}

#[test]
fn syntax_it_cannot_parse_is_left_alone() {
    assert_eq!(fold("this is not javascript {{{", &facts(&[], 0)), Outcome::Open);
}

#[test]
fn an_empty_body_decides_nothing() {
    assert_eq!(fold("", &facts(&[], 0)), Outcome::Open);
}
