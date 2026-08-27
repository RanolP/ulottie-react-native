#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * One rasterizer backend as a table of the five C ABI functions from
 * `ulottie-rt/src/ffi.rs`. The two symbol sets (`ulottie_rt_*`, tiny-skia;
 * `ulottie_rt_tvg_*`, ThorVG) are shape-identical, so a backend package
 * initializes this struct straight from its header and hands it to the
 * shared view/adapter layer — which holds all the buffer/blit/registry
 * logic exactly once per platform.
 */
typedef struct UlottieRtBackendFns {
  uint64_t (*instance_create)(void);
  void (*instance_destroy)(uint64_t id);
  bool (*instance_load)(uint64_t id, const uint8_t *ptr, size_t len);
  bool (*instance_set_buffer)(uint64_t id, uint8_t *ptr, uint32_t width,
                              uint32_t height, uint32_t stride_bytes);
  bool (*render_frame)(uint64_t id, float frame);
} UlottieRtBackendFns;

#ifdef __cplusplus
}
#endif
