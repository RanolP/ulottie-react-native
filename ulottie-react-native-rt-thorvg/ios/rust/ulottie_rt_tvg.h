/* C ABI of the ulottie-rt rasterizer static library — ThorVG symbol set.
 *
 * The contract is identical to `ulottie_rt.h`, function for function; only
 * the rasterizer behind render_frame differs (ThorVG SW engine, feature
 * `thorvg`). The `_tvg_` prefix exists because an app may link both backend
 * pods into one binary — the compare app does — and identical exported names
 * would collide at app link time. Kept by hand in lockstep with
 * `ulottie-rt/src/ffi.rs`; see ffi.rs for the ownership contract.
 */
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Creates a rasterizer instance; returns its id (never 0). */
uint64_t ulottie_rt_tvg_instance_create(void);

/* Destroys an instance; unknown ids are ignored. */
void ulottie_rt_tvg_instance_destroy(uint64_t id);

/* Loads an RTDL blob into an instance (once, at mount). The bytes are copied
 * during the call. Returns false on an unknown id or undecodable bytes. */
bool ulottie_rt_tvg_instance_load(uint64_t id, const uint8_t *ptr, size_t len);

/* Points an instance at a platform-owned premultiplied-RGBA8888 buffer.
 * stride_bytes must equal width * 4 (tightly packed rows). Returns false and
 * clears the buffer when the arguments are invalid or the id is unknown. */
bool ulottie_rt_tvg_instance_set_buffer(uint64_t id, uint8_t *ptr,
                                        uint32_t width, uint32_t height,
                                        uint32_t stride_bytes);

/* Renders `frame` into the instance's current buffer. Returns false when the
 * id is unknown (destroyed) or no valid buffer is set. */
bool ulottie_rt_tvg_render_frame(uint64_t id, float frame);

#ifdef __cplusplus
}
#endif
