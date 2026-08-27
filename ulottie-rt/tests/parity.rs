//! Pixel parity against lottie-web, at 512², over the full fixture corpus.
//!
//! `tests/refs/<name>-<frame>-ref.png` are lottie-web screenshots captured by
//! `ulottie-dev-server/tools/compare.mjs` (playwright, element screenshot of
//! the reference SVG at 512×512; frames pinned at 0/25/50/75/100% of the
//! span, clamped to lottie-web's last frame). Each fixture compiles here with
//! the *current* compiler (`--target rt`), renders with the *current*
//! tiny-skia rasterizer at the same frames, and diffs pixel-by-pixel over a
//! white backdrop.
//!
//! A pixel counts as different past 25/255 on any channel (odiff's 0.1
//! colour-distance threshold, per channel); a fixture passes when every frame
//! stays within its budget. The default budget is 1%. A larger budget is
//! never a loosened gate — it is a *documented divergence*, listed in
//! `BUDGETS` with the reason, and it still pins the fixture: a regression
//! past the measured divergence fails.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use tiny_skia::{PixmapMut, Transform};
use ulottie_rt::anim::Player;
use ulottie_rt::raster::{self, ImagePool};
use ulottie_rt::rtdl;
#[cfg(feature = "thorvg")]
use ulottie_rt::thorvg;

/// (fixture, budget in percent, reason for any budget above the 1% default).
///
/// The divergences behind the wider budgets, all structural rather than bugs:
/// - **3-box blur**: the rasterizer approximates the Gaussian with three box
///   passes (W3C `d = floor(σ·3·√(2π)/4 + 0.5)`); edge pixels of every
///   blurred region differ from lottie-web's SVG-filter Gaussian.
/// - **integer-frame tracks**: eases are baked at integer frames and lerped
///   between, so a screenshot at a fractional-speed pin can sit a sub-frame
///   off the browser's continuous sampling.
/// - **luma constants**: tiny-skia's luminance mask uses Rec.709 constants
///   (0.2126/0.7152/0.0722); browsers' SVG luminanceToAlpha differs slightly.
/// - **stroke_under_fill:50**: an existing, documented web-target divergence
///   at the 50% pin (see the skia target's parity notes) carried over here.
const BUDGETS: &[(&str, f64, &str)] = &[
    ("boucing_ball", 1.0, ""),
    ("rectangle", 1.0, ""),
    ("ellipse", 1.0, ""),
    ("fill", 1.0, ""),
    ("trim_path", 1.0, ""),
    ("android_wave", 1.0, ""),
    ("precomp_star_circle", 1.0, ""),
    ("gradient_radial", 1.0, ""),
    ("lottie_logo_1", 1.0, ""),
    ("mask_subtract", 1.0, ""),
    ("matte_alpha", 1.0, ""),
    ("stroke_under_fill", 1.0, ""),
    ("blend_multiply", 1.0, ""),
    ("gradient_animated", 1.0, ""),
    ("matte_luma_inv", 1.0, ""),
    (
        "fx_effects",
        3.0,
        "the drop shadow: lottie-web keeps its default 0%/100% filter region, \
         which clips the shadow to the element's own box — behind the opaque \
         square, so the reference never shows it. This rasterizer drops \
         percentage filter regions (same stance as the skia-aot target) and \
         draws the real After Effects shadow; every differing pixel is in \
         that shadow's area (measured worst 2.657% at the last pin, where \
         opacity and softness peak).",
    ),
    ("image_embedded", 1.0, ""),
];

const SIZE: u32 = 512;
const CHANNEL_THRESHOLD: u8 = 25;

fn refs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("refs")
}

fn fixture_json(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("_fixtures")
        .join("animations")
        .join(format!("{name}.json"));
    std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("missing fixture {}", p.display()))
}

/// The reference frames on disk for one fixture, sorted.
fn ref_frames(name: &str) -> Vec<(u32, PathBuf)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(refs_dir()).unwrap() {
        let path = entry.unwrap().path();
        let file = path.file_name().unwrap().to_str().unwrap().to_string();
        let Some(rest) = file.strip_prefix(&format!("{name}-")) else {
            continue;
        };
        let Some(frame) = rest.strip_suffix("-ref.png") else {
            continue;
        };
        if let Ok(f) = frame.parse::<u32>() {
            out.push((f, path));
        }
    }
    out.sort();
    assert!(!out.is_empty(), "{name}: no reference screenshots in tests/refs/");
    out
}

/// Straight RGBA8 at 512², composited over white.
fn read_ref(path: &Path) -> Vec<[u8; 3]> {
    let mut decoder = png::Decoder::new(std::fs::File::open(path).unwrap());
    decoder.set_transformations(
        png::Transformations::ALPHA | png::Transformations::EXPAND | png::Transformations::STRIP_16,
    );
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!((info.width, info.height), (SIZE, SIZE), "{}: not 512²", path.display());
    assert_eq!(info.color_type, png::ColorType::Rgba, "{}", path.display());
    buf.truncate(info.buffer_size());
    buf.chunks_exact(4)
        .map(|px| {
            let a = px[3] as u32;
            let over = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
            [over(px[0]), over(px[1]), over(px[2])]
        })
        .collect()
}

/// Per-fixture budgets for the ThorVG backend (feature `thorvg`), same rules
/// as `BUDGETS`: 1% default, anything wider is a documented divergence that
/// still pins the fixture.
#[cfg(feature = "thorvg")]
const BUDGETS_THORVG: &[(&str, f64, &str)] = &[
    ("boucing_ball", 1.0, ""),
    ("rectangle", 1.0, ""),
    ("ellipse", 1.0, ""),
    ("fill", 1.0, ""),
    ("trim_path", 1.0, ""),
    ("android_wave", 1.0, ""),
    ("precomp_star_circle", 1.0, ""),
    ("gradient_radial", 1.0, ""),
    ("lottie_logo_1", 1.0, ""),
    ("mask_subtract", 1.0, ""),
    ("matte_alpha", 1.0, ""),
    ("stroke_under_fill", 1.0, ""),
    ("blend_multiply", 1.0, ""),
    ("gradient_animated", 1.0, ""),
    ("matte_luma_inv", 1.0, ""),
    ("fx_effects", 3.0, "same drop-shadow filter-region divergence as tiny-skia"),
    (
        "image_embedded",
        3.5,
        "rotated-bitmap sampling: ThorVG's texture mapper bilinear-filters \
         across the picture's outer edge (a ~1-texel feather band, wide at \
         this fixture's 32x upscale) and interpolates the interior seam over \
         a wider span than canvas drawImage's clamped bilinear; the axis-\
         aligned frame 0 passes at 0.002%",
    ),
];

/// Compile with the current compiler, decode with the current runtime.
fn load(name: &str) -> (Player, ImagePool) {
    let js = ulottie_compiler::compile_with(
        &fixture_json(name),
        &ulottie_compiler::CompileOptions {
            target: ulottie_compiler::Target::Rt,
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("{name}: {e:#}"));
    let b64 = js
        .split("export const rtdl = '")
        .nth(1)
        .and_then(|r| r.split('\'').next())
        .unwrap_or_else(|| panic!("{name}: no rtdl export"));
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
    let anim = rtdl::decode(&blob).unwrap_or_else(|e| panic!("{name}: decode: {e}"));
    let images = ImagePool::new(&anim);
    (Player::new(anim), images)
}

/// Premultiplied canvas pixels → straight RGB over white.
fn over_white(data: &[u8]) -> Vec<[u8; 3]> {
    data.chunks_exact(4)
        .map(|px| {
            // Premultiplied: white shows through by (255 - a).
            let a = px[3] as u32;
            let over = |c: u8| (c as u32 + (255 - a)).min(255) as u8;
            [over(px[0]), over(px[1]), over(px[2])]
        })
        .collect()
}

fn render(player: &mut Player, images: &ImagePool, frame: f32) -> Vec<[u8; 3]> {
    player.apply(frame);
    let anim = &player.anim;
    let mut data = vec![0u8; (SIZE * SIZE * 4) as usize];
    let mut pixmap = PixmapMut::from_bytes(&mut data, SIZE, SIZE).unwrap();
    let (dw, dh) = (anim.width.max(1.0), anim.height.max(1.0));
    let s = (SIZE as f32 / dw).min(SIZE as f32 / dh);
    let fit = Transform::from_translate(
        (SIZE as f32 - dw * s) / 2.0,
        (SIZE as f32 - dh * s) / 2.0,
    )
    .pre_scale(s, s);
    raster::render(anim, images, &mut pixmap, fit);
    over_white(&data)
}

#[cfg(feature = "thorvg")]
fn render_tvg(player: &mut Player, images: &thorvg::ThorImages, frame: f32) -> Vec<[u8; 3]> {
    player.apply(frame);
    let anim = &player.anim;
    let mut data = vec![0u8; (SIZE * SIZE * 4) as usize];
    let (dw, dh) = (anim.width.max(1.0), anim.height.max(1.0));
    let s = (SIZE as f32 / dw).min(SIZE as f32 / dh);
    let fit = [
        s,
        0.0,
        0.0,
        s,
        (SIZE as f32 - dw * s) / 2.0,
        (SIZE as f32 - dh * s) / 2.0,
    ];
    thorvg::render(anim, images, &mut data, SIZE, SIZE, fit);
    over_white(&data)
}

/// Debug aid for `ULOTTIE_PARITY_DUMP`.
fn dump_png(path: &str, px: &[[u8; 3]]) {
    let f = std::fs::File::create(path).unwrap();
    let mut enc = png::Encoder::new(f, SIZE, SIZE);
    enc.set_color(png::ColorType::Rgb);
    let mut w = enc.write_header().unwrap();
    w.write_image_data(px.as_flattened()).unwrap();
}

fn diff_pct(a: &[[u8; 3]], b: &[[u8; 3]]) -> f64 {
    let differing = a
        .iter()
        .zip(b)
        .filter(|(x, y)| {
            x.iter()
                .zip(y.iter())
                .any(|(&p, &q)| p.abs_diff(q) > CHANNEL_THRESHOLD)
        })
        .count();
    differing as f64 * 100.0 / a.len() as f64
}

fn check_frames<F: FnMut(f32) -> Vec<[u8; 3]>>(
    name: &str,
    label: &str,
    budgets: &[(&str, f64, &str)],
    mut render_at: F,
) {
    let (_, budget, note) = budgets
        .iter()
        .find(|(n, _, _)| *n == name)
        .unwrap_or_else(|| panic!("{name}: not in the {label} budget table"));
    let mut results: BTreeMap<u32, f64> = BTreeMap::new();
    let mut worst = 0.0f64;
    for (frame, path) in ref_frames(name) {
        let reference = read_ref(&path);
        let ours = render_at(frame as f32);
        let pct = diff_pct(&reference, &ours);
        // ULOTTIE_PARITY_DUMP=<dir>: write ours/ref/diff PNGs for inspection.
        if let Ok(dir) = std::env::var("ULOTTIE_PARITY_DUMP") {
            dump_png(&format!("{dir}/{name}-{label}-{frame}-ours.png"), &ours);
            dump_png(&format!("{dir}/{name}-{label}-{frame}-ref.png"), &reference);
            let d: Vec<[u8; 3]> = reference
                .iter()
                .zip(&ours)
                .map(|(x, y)| {
                    if x.iter().zip(y.iter()).any(|(&p, &q)| p.abs_diff(q) > CHANNEL_THRESHOLD) {
                        [255, 0, 0]
                    } else {
                        [255, 255, 255]
                    }
                })
                .collect();
            dump_png(&format!("{dir}/{name}-{label}-{frame}-diff.png"), &d);
        }
        results.insert(frame, pct);
        worst = worst.max(pct);
    }
    // Always print the measurements: the budget table is maintained from
    // these numbers, and a passing run should still show its margins.
    let detail: Vec<String> = results.iter().map(|(f, p)| format!("f{f}={p:.3}%")).collect();
    println!(
        "{name} [{label}]: worst={worst:.3}% budget={budget}% [{}]",
        detail.join(" ")
    );
    assert!(
        worst <= *budget,
        "{name} [{label}]: worst frame diff {worst:.3}% exceeds the {budget}% budget{}{note}",
        if note.is_empty() { "" } else { " — documented divergence: " },
    );
}

fn check(name: &str) {
    let (mut player, images) = load(name);
    check_frames(name, "tinyskia", BUDGETS, |f| render(&mut player, &images, f));
}

#[cfg(feature = "thorvg")]
fn check_tvg(name: &str) {
    let (mut player, _) = load(name);
    let images = thorvg::ThorImages::new(&player.anim);
    check_frames(name, "thorvg", BUDGETS_THORVG, |f| {
        render_tvg(&mut player, &images, f)
    });
}

macro_rules! parity {
    ($test:ident, $fixture:literal) => {
        #[test]
        fn $test() {
            check($fixture);
        }
    };
}

/// The same fixture through the ThorVG backend, its own budget table.
#[cfg(feature = "thorvg")]
macro_rules! parity_tvg {
    ($test:ident, $fixture:literal) => {
        #[test]
        fn $test() {
            check_tvg($fixture);
        }
    };
}
#[cfg(not(feature = "thorvg"))]
macro_rules! parity_tvg {
    ($test:ident, $fixture:literal) => {};
}

parity!(parity_boucing_ball, "boucing_ball");
parity!(parity_rectangle, "rectangle");
parity!(parity_ellipse, "ellipse");
parity!(parity_fill, "fill");
parity!(parity_trim_path, "trim_path");
parity!(parity_android_wave, "android_wave");
parity!(parity_precomp_star_circle, "precomp_star_circle");
parity!(parity_gradient_radial, "gradient_radial");
parity!(parity_lottie_logo_1, "lottie_logo_1");
parity!(parity_mask_subtract, "mask_subtract");
parity!(parity_matte_alpha, "matte_alpha");
parity!(parity_stroke_under_fill, "stroke_under_fill");
parity!(parity_blend_multiply, "blend_multiply");
parity!(parity_gradient_animated, "gradient_animated");
parity!(parity_matte_luma_inv, "matte_luma_inv");
parity!(parity_fx_effects, "fx_effects");
parity!(parity_image_embedded, "image_embedded");

parity_tvg!(parity_tvg_boucing_ball, "boucing_ball");
parity_tvg!(parity_tvg_rectangle, "rectangle");
parity_tvg!(parity_tvg_ellipse, "ellipse");
parity_tvg!(parity_tvg_fill, "fill");
parity_tvg!(parity_tvg_trim_path, "trim_path");
parity_tvg!(parity_tvg_android_wave, "android_wave");
parity_tvg!(parity_tvg_precomp_star_circle, "precomp_star_circle");
parity_tvg!(parity_tvg_gradient_radial, "gradient_radial");
parity_tvg!(parity_tvg_lottie_logo_1, "lottie_logo_1");
parity_tvg!(parity_tvg_mask_subtract, "mask_subtract");
parity_tvg!(parity_tvg_matte_alpha, "matte_alpha");
parity_tvg!(parity_tvg_stroke_under_fill, "stroke_under_fill");
parity_tvg!(parity_tvg_blend_multiply, "blend_multiply");
parity_tvg!(parity_tvg_gradient_animated, "gradient_animated");
parity_tvg!(parity_tvg_matte_luma_inv, "matte_luma_inv");
parity_tvg!(parity_tvg_fx_effects, "fx_effects");
parity_tvg!(parity_tvg_image_embedded, "image_embedded");
