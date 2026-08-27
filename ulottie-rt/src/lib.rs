//! ulottie-rt — the native rasterizer runtime behind the React Native target.
//!
//! The crate is consumed through the C ABI in [`ffi`]: the platform view owns
//! the pixel buffers, hands a borrowed pointer over per frame, and this crate
//! rasterizes into it with tiny-skia. Pixel format is premultiplied RGBA8888
//! (tiny-skia's native format), which is also what a `CGImage` with
//! `kCGImageAlphaPremultipliedLast` and an Android `ARGB_8888` bitmap read.

pub mod anim;
pub mod bounds;
pub mod ffi;
pub mod pixels;
#[cfg(feature = "tinyskia")]
pub mod raster;
pub mod rtdl;
#[cfg(feature = "thorvg")]
pub mod thorvg;
