// Static imports only — Metro cannot bundle dynamic requires.
// `*.lottie.json` compiles through the ulottie Metro transformer into a module
// exporting { tree, meta, init }; the `*.skia.lottie.json` twin (same bytes,
// the name picks the target) compiles to the skia-aot { dl, meta, init }
// module; the plain `*.json` twin stays raw JSON for lottie-react-native.
import { createUlottie } from 'ulottie-react-native';
import { createUlottieSkia } from 'ulottie-react-native/skia';

import * as boucing_ball_u from '../assets/boucing_ball.lottie.json';
import * as boucing_ball_s from '../assets/boucing_ball.skia.lottie.json';
import boucing_ball_l from '../assets/boucing_ball.json';
import * as rectangle_u from '../assets/rectangle.lottie.json';
import * as rectangle_s from '../assets/rectangle.skia.lottie.json';
import rectangle_l from '../assets/rectangle.json';
import * as ellipse_u from '../assets/ellipse.lottie.json';
import * as ellipse_s from '../assets/ellipse.skia.lottie.json';
import ellipse_l from '../assets/ellipse.json';
import * as fill_u from '../assets/fill.lottie.json';
import * as fill_s from '../assets/fill.skia.lottie.json';
import fill_l from '../assets/fill.json';
import * as trim_path_u from '../assets/trim_path.lottie.json';
import * as trim_path_s from '../assets/trim_path.skia.lottie.json';
import trim_path_l from '../assets/trim_path.json';
import * as android_wave_u from '../assets/android_wave.lottie.json';
import * as android_wave_s from '../assets/android_wave.skia.lottie.json';
import android_wave_l from '../assets/android_wave.json';
import * as precomp_star_circle_u from '../assets/precomp_star_circle.lottie.json';
import * as precomp_star_circle_s from '../assets/precomp_star_circle.skia.lottie.json';
import precomp_star_circle_l from '../assets/precomp_star_circle.json';
import * as gradient_radial_u from '../assets/gradient_radial.lottie.json';
import * as gradient_radial_s from '../assets/gradient_radial.skia.lottie.json';
import gradient_radial_l from '../assets/gradient_radial.json';
import * as lottie_logo_1_u from '../assets/lottie_logo_1.lottie.json';
import * as lottie_logo_1_s from '../assets/lottie_logo_1.skia.lottie.json';
import lottie_logo_1_l from '../assets/lottie_logo_1.json';
import * as mask_subtract_u from '../assets/mask_subtract.lottie.json';
import * as mask_subtract_s from '../assets/mask_subtract.skia.lottie.json';
import mask_subtract_l from '../assets/mask_subtract.json';
import * as matte_alpha_u from '../assets/matte_alpha.lottie.json';
import * as matte_alpha_s from '../assets/matte_alpha.skia.lottie.json';
import matte_alpha_l from '../assets/matte_alpha.json';
import * as stroke_under_fill_u from '../assets/stroke_under_fill.lottie.json';
import * as stroke_under_fill_s from '../assets/stroke_under_fill.skia.lottie.json';
import stroke_under_fill_l from '../assets/stroke_under_fill.json';
import * as bodymoovin_u from '../assets/bodymoovin.lottie.json';
import * as bodymoovin_s from '../assets/bodymoovin.skia.lottie.json';
import bodymoovin_l from '../assets/bodymoovin.json';
import * as fireworks_u from '../assets/fireworks.lottie.json';
import * as fireworks_s from '../assets/fireworks.skia.lottie.json';
import fireworks_l from '../assets/fireworks.json';
import * as lottie_logo_2_u from '../assets/lottie_logo_2.lottie.json';
import * as lottie_logo_2_s from '../assets/lottie_logo_2.skia.lottie.json';
import lottie_logo_2_l from '../assets/lottie_logo_2.json';
import * as lottie_logo_3_u from '../assets/lottie_logo_3.lottie.json';
import * as lottie_logo_3_s from '../assets/lottie_logo_3.skia.lottie.json';
import lottie_logo_3_l from '../assets/lottie_logo_3.json';
import * as matte_luma_u from '../assets/matte_luma.lottie.json';
import * as matte_luma_s from '../assets/matte_luma.skia.lottie.json';
import matte_luma_l from '../assets/matte_luma.json';
// Skia-only fixtures: the reanimated-aot (svg) target refuses these, so they
// carry no `.lottie.json` twin — only the skia module and the raw JSON for
// lottie-react-native.
import * as blend_multiply_s from '../assets/blend_multiply.skia.lottie.json';
import blend_multiply_l from '../assets/blend_multiply.json';
import * as gradient_animated_s from '../assets/gradient_animated.skia.lottie.json';
import gradient_animated_l from '../assets/gradient_animated.json';
import * as matte_luma_inv_s from '../assets/matte_luma_inv.skia.lottie.json';
import matte_luma_inv_l from '../assets/matte_luma_inv.json';
import * as fx_effects_s from '../assets/fx_effects.skia.lottie.json';
import fx_effects_l from '../assets/fx_effects.json';
import * as image_embedded_s from '../assets/image_embedded.skia.lottie.json';
import image_embedded_l from '../assets/image_embedded.json';

const raw = [
  ['boucing_ball', boucing_ball_u, boucing_ball_s, boucing_ball_l],
  ['rectangle', rectangle_u, rectangle_s, rectangle_l],
  ['ellipse', ellipse_u, ellipse_s, ellipse_l],
  ['fill', fill_u, fill_s, fill_l],
  ['trim_path', trim_path_u, trim_path_s, trim_path_l],
  ['android_wave', android_wave_u, android_wave_s, android_wave_l],
  ['precomp_star_circle', precomp_star_circle_u, precomp_star_circle_s, precomp_star_circle_l],
  ['gradient_radial', gradient_radial_u, gradient_radial_s, gradient_radial_l],
  ['lottie_logo_1', lottie_logo_1_u, lottie_logo_1_s, lottie_logo_1_l],
  ['mask_subtract', mask_subtract_u, mask_subtract_s, mask_subtract_l],
  ['matte_alpha', matte_alpha_u, matte_alpha_s, matte_alpha_l],
  ['stroke_under_fill', stroke_under_fill_u, stroke_under_fill_s, stroke_under_fill_l],
  ['bodymoovin', bodymoovin_u, bodymoovin_s, bodymoovin_l],
  ['fireworks', fireworks_u, fireworks_s, fireworks_l],
  ['lottie_logo_2', lottie_logo_2_u, lottie_logo_2_s, lottie_logo_2_l],
  ['lottie_logo_3', lottie_logo_3_u, lottie_logo_3_s, lottie_logo_3_l],
  ['matte_luma', matte_luma_u, matte_luma_s, matte_luma_l],
  // Skia-only (ulottieModule null → no reanimated-aot player).
  ['blend_multiply', null, blend_multiply_s, blend_multiply_l],
  ['gradient_animated', null, gradient_animated_s, gradient_animated_l],
  ['matte_luma_inv', null, matte_luma_inv_s, matte_luma_inv_l],
  ['fx_effects', null, fx_effects_s, fx_effects_l],
  ['image_embedded', null, image_embedded_s, image_embedded_l],
];

/** name → { name, Ulottie (null on skia-only), UlottieSkia, lottieSource (raw JSON), meta } */
export const FIXTURES = raw.map(([name, ulottieModule, skiaModule, lottieSource]) => ({
  name,
  Ulottie: ulottieModule && createUlottie(ulottieModule),
  UlottieSkia: createUlottieSkia(skiaModule),
  lottieSource,
  meta: (ulottieModule || skiaModule).meta,
}));

export const FIXTURE_BY_NAME = Object.fromEntries(FIXTURES.map((f) => [f.name, f]));
