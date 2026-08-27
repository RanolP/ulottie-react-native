//! C ABI surface for the platform packages (iOS / Android).
//!
//! Ownership contract:
//! - The **platform owns the pixel memory**. `set_buffer` stores a borrowed
//!   pointer; the platform must keep it valid until the next `set_buffer` or
//!   `destroy` for that instance, and must only call these functions from one
//!   thread at a time per instance (in practice: the platform main thread —
//!   the worklets UI runtime).
//! - `render_frame` after `destroy`, or with an unknown id, is a **no-op that
//!   returns false**, never undefined behaviour — the JS-side frame loop lives
//!   on a different runtime than React's unmount and will race it.
//! - `load` hands the instance its RTDL blob (the compiler backend's base64
//!   payload, already decoded to bytes by the caller); `render_frame` before a
//!   successful `load` returns false.
//!
//! Two symbol sets, one contract (`include/ulottie_rt.h` and
//! `include/ulottie_rt_tvg.h`):
//! - `ulottie_rt_*` — the tiny-skia backend (feature `tinyskia`).
//! - `ulottie_rt_tvg_*` — the ThorVG backend (feature `thorvg`).
//!
//! The prefix exists because the compare app links BOTH backend pods into one
//! binary; identical exported names would collide at app link time. Behaviour
//! is byte-for-byte the same contract — only the rasterizer behind
//! `render_frame` differs — and both sets share one instance registry, so ids
//! stay unique across backends.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::anim::Player;
use crate::rtdl;

#[cfg(feature = "tinyskia")]
use crate::raster::{self, ImagePool};
#[cfg(feature = "thorvg")]
use crate::thorvg::{self, ThorImages};
#[cfg(feature = "tinyskia")]
use tiny_skia::{PixmapMut, Transform};

/// A borrowed platform pixel buffer, premultiplied RGBA8888.
struct Buffer {
    ptr: *mut u8,
    width: u32,
    height: u32,
    stride_bytes: u32,
}

// The raw pointer makes `Buffer` !Send by default. Instances live in the
// global registry behind a `Mutex`, and the contract above pins all use of
// one instance to a single thread; the marker only tells the compiler the
// registry itself may be touched from any thread.
unsafe impl Send for Buffer {}

#[derive(Default)]
struct Instance {
    buffer: Option<Buffer>,
    scene: Option<Scene>,
}

/// Which rasterizer a loaded scene renders with — chosen by the export set
/// `load` was called through.
#[derive(Clone, Copy, PartialEq)]
enum BackendKind {
    #[cfg_attr(not(feature = "tinyskia"), allow(dead_code))]
    TinySkia,
    #[cfg_attr(not(feature = "thorvg"), allow(dead_code))]
    Thorvg,
}

struct Scene {
    // Read only when a backend feature is on; a featureless build still
    // validates load/decode so its FFI contract matches.
    #[cfg_attr(not(any(feature = "tinyskia", feature = "thorvg")), allow(dead_code))]
    player: Player,
    #[cfg_attr(not(any(feature = "tinyskia", feature = "thorvg")), allow(dead_code))]
    backend: Backend,
}

enum Backend {
    #[cfg(feature = "tinyskia")]
    TinySkia(ImagePool),
    #[cfg(feature = "thorvg")]
    Thorvg(ThorImages),
    /// `load` called through an export set whose feature is off (impossible
    /// via the exports themselves, kept for the featureless build).
    #[allow(dead_code)]
    None,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static INSTANCES: LazyLock<Mutex<HashMap<u64, Instance>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// ------------------------------------------------------- shared implementation

fn instance_create() -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    INSTANCES.lock().unwrap().insert(id, Instance::default());
    id
}

fn instance_destroy(id: u64) {
    INSTANCES.lock().unwrap().remove(&id);
}

fn instance_load(id: u64, ptr: *const u8, len: usize, kind: BackendKind) -> bool {
    if ptr.is_null() || len == 0 {
        return false;
    }
    // Safety: the caller passes a readable byte range it owns for the call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let Ok(anim) = rtdl::decode(bytes) else {
        return false;
    };
    let backend = match kind {
        #[cfg(feature = "tinyskia")]
        BackendKind::TinySkia => Backend::TinySkia(ImagePool::new(&anim)),
        #[cfg(feature = "thorvg")]
        BackendKind::Thorvg => Backend::Thorvg(ThorImages::new(&anim)),
        #[allow(unreachable_patterns)]
        _ => Backend::None,
    };
    let mut instances = INSTANCES.lock().unwrap();
    let Some(instance) = instances.get_mut(&id) else {
        return false;
    };
    instance.scene = Some(Scene {
        player: Player::new(anim),
        backend,
    });
    true
}

fn instance_set_buffer(id: u64, ptr: *mut u8, width: u32, height: u32, stride_bytes: u32) -> bool {
    let mut instances = INSTANCES.lock().unwrap();
    let Some(instance) = instances.get_mut(&id) else {
        return false;
    };
    if ptr.is_null() || width == 0 || height == 0 || stride_bytes != width * 4 {
        instance.buffer = None;
        return false;
    }
    instance.buffer = Some(Buffer {
        ptr,
        width,
        height,
        stride_bytes,
    });
    true
}

// `allow(unused_variables)`: in the featureless build (the compiler links
// this crate with `default-features = false`) the fit math and buffer slice
// have no consumer arm.
#[allow(unused_variables)]
fn render_frame(id: u64, frame: f32) -> bool {
    let mut instances = INSTANCES.lock().unwrap();
    let Some(instance) = instances.get_mut(&id) else {
        return false;
    };
    let (Some(buffer), Some(scene)) = (instance.buffer.as_ref(), instance.scene.as_mut()) else {
        return false;
    };
    let len = (buffer.height * buffer.stride_bytes) as usize;
    // Safety: `set_buffer` validated the geometry and the platform guarantees
    // the pointer stays valid between `set_buffer` and the next call.
    let data = unsafe { std::slice::from_raw_parts_mut(buffer.ptr, len) };
    let (width, height) = (buffer.width, buffer.height);

    scene.player.apply(frame);
    // `xMidYMid meet`: uniform scale, centred — same fit as the web player.
    let anim = &scene.player.anim;
    let (dw, dh) = (anim.width.max(1.0), anim.height.max(1.0));
    let s = (width as f32 / dw).min(height as f32 / dh);
    let (tx, ty) = (
        (width as f32 - dw * s) / 2.0,
        (height as f32 - dh * s) / 2.0,
    );

    match &scene.backend {
        #[cfg(feature = "tinyskia")]
        Backend::TinySkia(images) => {
            let Some(mut pixmap) = PixmapMut::from_bytes(data, width, height) else {
                return false;
            };
            let fit = Transform::from_translate(tx, ty).pre_scale(s, s);
            pixmap.data_mut().fill(0);
            raster::render(&scene.player.anim, images, &mut pixmap, fit);
            true
        }
        #[cfg(feature = "thorvg")]
        Backend::Thorvg(images) => {
            // ThorVG clears + copies internally (aligned scratch surface).
            thorvg::render(
                &scene.player.anim,
                images,
                data,
                width,
                height,
                [s, 0.0, 0.0, s, tx, ty],
            );
            true
        }
        Backend::None => false,
    }
}

// ------------------------------------------------- tiny-skia exports (ulottie_rt_*)

/// Creates a rasterizer instance and returns its id (never 0).
#[cfg(feature = "tinyskia")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_instance_create() -> u64 {
    instance_create()
}

/// Destroys an instance. Unknown ids are ignored, so a double-destroy is safe.
#[cfg(feature = "tinyskia")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_instance_destroy(id: u64) {
    instance_destroy(id)
}

/// Loads an RTDL blob into an instance (called once at mount). Returns false
/// on an unknown id or bytes that do not decode as RTDL.
#[cfg(feature = "tinyskia")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_instance_load(id: u64, ptr: *const u8, len: usize) -> bool {
    instance_load(id, ptr, len, BackendKind::TinySkia)
}

/// Points an instance at the platform-owned buffer it renders into.
///
/// Returns false (and clears any previous buffer) when the arguments cannot
/// describe a pixmap: null pointer, zero size, or a stride other than
/// `width * 4` — the rasterizers require tightly packed rows, so the platform
/// must allocate them that way.
#[cfg(feature = "tinyskia")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_instance_set_buffer(
    id: u64,
    ptr: *mut u8,
    width: u32,
    height: u32,
    stride_bytes: u32,
) -> bool {
    instance_set_buffer(id, ptr, width, height, stride_bytes)
}

/// Renders `frame` into the instance's buffer. Returns false when the id is
/// unknown (destroyed), no valid buffer is set, or no scene is loaded.
#[cfg(feature = "tinyskia")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_render_frame(id: u64, frame: f32) -> bool {
    render_frame(id, frame)
}

// --------------------------------------------------- ThorVG exports (ulottie_rt_tvg_*)

/// [`ulottie_rt_instance_create`], ThorVG symbol set.
#[cfg(feature = "thorvg")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_tvg_instance_create() -> u64 {
    instance_create()
}

/// [`ulottie_rt_instance_destroy`], ThorVG symbol set.
#[cfg(feature = "thorvg")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_tvg_instance_destroy(id: u64) {
    instance_destroy(id)
}

/// [`ulottie_rt_instance_load`], ThorVG symbol set: the loaded scene renders
/// with ThorVG.
#[cfg(feature = "thorvg")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_tvg_instance_load(id: u64, ptr: *const u8, len: usize) -> bool {
    instance_load(id, ptr, len, BackendKind::Thorvg)
}

/// [`ulottie_rt_instance_set_buffer`], ThorVG symbol set.
#[cfg(feature = "thorvg")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_tvg_instance_set_buffer(
    id: u64,
    ptr: *mut u8,
    width: u32,
    height: u32,
    stride_bytes: u32,
) -> bool {
    instance_set_buffer(id, ptr, width, height, stride_bytes)
}

/// [`ulottie_rt_render_frame`], ThorVG symbol set.
#[cfg(feature = "thorvg")]
#[unsafe(no_mangle)]
pub extern "C" fn ulottie_rt_tvg_render_frame(id: u64, frame: f32) -> bool {
    render_frame(id, frame)
}

#[cfg(all(test, feature = "tinyskia"))]
mod tests {
    use crate::rtdl::{
        Animation, Channel, Geom, Group, Keys, Node, Paint, PaintSource, Shape, Track,
    };

    fn tiny_scene() -> Vec<u8> {
        // A red dot sliding right over 60 frames.
        let anim = Animation {
            width: 32.0,
            height: 32.0,
            fr: 30.0,
            ip: 0.0,
            op: 60.0,
            nodes: vec![
                Node::Group(Group {
                    opacity: 1.0,
                    children: vec![1],
                    ..Group::default()
                }),
                Node::Shape(Shape {
                    slot: Some(1),
                    matrix: None,
                    opacity: 1.0,
                    hidden: false,
                    geom: Geom::Ellipse {
                        cx: 8.0,
                        cy: 16.0,
                        rx: 6.0,
                        ry: 6.0,
                    },
                    even_odd: false,
                    paint: Paint {
                        fill: Some(PaintSource::Color([1.0, 0.0, 0.0, 1.0])),
                        ..Paint::default()
                    },
                }),
            ],
            gradients: Vec::new(),
            images: Vec::new(),
            tracks: vec![Track {
                slot: 1,
                channels: vec![Channel::Ellipse(Keys {
                    frames: vec![0.0, 60.0],
                    values: vec![[8.0, 16.0, 6.0, 6.0], [24.0, 16.0, 6.0, 6.0]],
                })],
            }],
        };
        crate::rtdl::encode(&anim).unwrap()
    }

    /// The one runnable check: create → load → set_buffer → two frames of a
    /// real RTDL scene differ; render before load and after destroy refuse.
    #[test]
    fn render_lifecycle() {
        let id = super::ulottie_rt_instance_create();
        let (w, h) = (32u32, 32u32);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        assert!(super::ulottie_rt_instance_set_buffer(
            id,
            buf.as_mut_ptr(),
            w,
            h,
            w * 4
        ));
        assert!(
            !super::ulottie_rt_render_frame(id, 0.0),
            "no scene loaded yet"
        );
        let blob = tiny_scene();
        assert!(super::ulottie_rt_instance_load(id, blob.as_ptr(), blob.len()));
        assert!(super::ulottie_rt_render_frame(id, 0.0));
        let frame0 = buf.clone();
        assert!(super::ulottie_rt_render_frame(id, 60.0));
        assert_ne!(frame0, buf, "two frames must rasterize differently");
        super::ulottie_rt_instance_destroy(id);
        assert!(!super::ulottie_rt_render_frame(id, 1.0));
        assert!(!super::ulottie_rt_instance_set_buffer(
            id,
            buf.as_mut_ptr(),
            w,
            h,
            w * 4
        ));
        assert!(!super::ulottie_rt_instance_load(id, blob.as_ptr(), blob.len()));
    }

    /// Same lifecycle through the ThorVG symbol set — the two backends share
    /// one registry, so a tvg-loaded scene renders via ThorVG.
    #[cfg(feature = "thorvg")]
    #[test]
    fn render_lifecycle_tvg() {
        let id = super::ulottie_rt_tvg_instance_create();
        let (w, h) = (32u32, 32u32);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        assert!(super::ulottie_rt_tvg_instance_set_buffer(
            id,
            buf.as_mut_ptr(),
            w,
            h,
            w * 4
        ));
        let blob = tiny_scene();
        assert!(super::ulottie_rt_tvg_instance_load(id, blob.as_ptr(), blob.len()));
        assert!(super::ulottie_rt_tvg_render_frame(id, 0.0));
        let frame0 = buf.clone();
        assert!(super::ulottie_rt_tvg_render_frame(id, 60.0));
        assert_ne!(frame0, buf, "two frames must rasterize differently");
        super::ulottie_rt_tvg_instance_destroy(id);
        assert!(!super::ulottie_rt_tvg_render_frame(id, 1.0));
    }
}
