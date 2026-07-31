#pragma once

#include <stddef.h>
#include <stdint.h>

#include <d3d11.h>

#ifdef __cplusplus
extern "C" {
#endif

#define RIFE_RUNTIME_ABI_VERSION 6u
#define RIFE_SCENE_CLASS_COUNT 5u
#define RIFE_PROFILE_STAGE_COUNT 9u

enum rife_color_matrix {
    RIFE_COLOR_MATRIX_BT601 = 0,
    RIFE_COLOR_MATRIX_BT709 = 1,
    RIFE_COLOR_MATRIX_BT2020_NCL = 2,
};

enum rife_runtime_status {
    RIFE_RUNTIME_OK = 0,
    RIFE_RUNTIME_INVALID_ARGUMENT = 1,
    RIFE_RUNTIME_CUDA_LOAD_FAILED = 2,
    RIFE_RUNTIME_TENSORRT_FAILED = 3,
    RIFE_RUNTIME_ENGINE_INCOMPATIBLE = 4,
    RIFE_RUNTIME_D3D11_FAILED = 5,
    RIFE_RUNTIME_INFERENCE_FAILED = 6,
};

enum rife_scene_classification {
    RIFE_SCENE_NORMAL = 0,
    RIFE_SCENE_FLASH = 1,
    RIFE_SCENE_FADE_DISSOLVE = 2,
    RIFE_SCENE_HARD_CUT = 3,
    RIFE_SCENE_UNCERTAIN = 4,
};

enum rife_profile_stage {
    RIFE_PROFILE_PROCESS_TOTAL = 0,
    RIFE_PROFILE_FRAME_SETUP = 1,
    RIFE_PROFILE_SCENE_DETECTION = 2,
    RIFE_PROFILE_INPUT_CONVERSION = 3,
    RIFE_PROFILE_CUDA_MAP = 4,
    RIFE_PROFILE_TENSOR_BIND = 5,
    RIFE_PROFILE_TENSORRT = 6,
    RIFE_PROFILE_CUDA_UNMAP = 7,
    RIFE_PROFILE_OUTPUT_CONVERSION = 8,
};

struct rife_timing_stats {
    double total_ms;
    double p95_ms;
    double max_ms;
};

struct rife_runtime_config {
    uint32_t abi_version;
    ID3D11Device *device;
    ID3D11DeviceContext *context;
    const wchar_t *engine_path;
    const wchar_t *cuda_runtime_path;
    uint32_t source_width;
    uint32_t source_height;
    uint32_t shape_alignment;
    uint32_t color_matrix;
    uint32_t limited_range;
    uint32_t scene_sample_stride;
    uint32_t scene_pixel_threshold;
    float scene_average_threshold;
    float scene_changed_ratio;
    uint32_t profiling_enabled;
};

struct rife_runtime_stats {
    uint64_t pairs;
    uint64_t inferred_pairs;
    uint64_t scene_cuts;
    uint64_t failures;
    double inference_total_ms;
    double inference_max_ms;
    double inference_p95_ms;
    double scene_total_ms;
    double scene_max_ms;
    uint32_t runtime_cache_hit;
    uint32_t runtime_prewarm_hit;
    uint32_t optimization_profile;
    uint64_t runtime_reuses;
    double runtime_initialization_ms;
    double runtime_cuda_load_ms;
    double runtime_cuda_bind_ms;
    double runtime_engine_read_ms;
    double runtime_trt_runtime_ms;
    double runtime_engine_deserialize_ms;
    double runtime_execution_context_ms;
    double runtime_engine_validate_ms;
    double runtime_d3d_resources_ms;
    uint64_t scene_classes[RIFE_SCENE_CLASS_COUNT];
    uint64_t profiled_pairs;
    struct rife_timing_stats profile_stages[RIFE_PROFILE_STAGE_COUNT];
};

struct rife_frame_diagnostics {
    double source0_pts;
    double source1_pts;
    double midpoint_pts;
    double inference_ms;
    double scene_ms;
    double average_delta;
    double changed_ratio;
    double average_kl;
    double regional_kl_max;
    double chroma_delta;
    double edge_delta;
    double exposure_delta;
    double exposure_spread;
    uint32_t classification;
    uint32_t scene_cut;
    double profile_stage_ms[RIFE_PROFILE_STAGE_COUNT];
};

struct rife_runtime;

__declspec(dllexport) uint32_t __cdecl rife_runtime_abi_version(void);

__declspec(dllexport) int __cdecl rife_runtime_prewarm_with_device(
    const wchar_t *engine_path,
    const wchar_t *cuda_runtime_path,
    uint32_t source_width,
    uint32_t source_height,
    uint32_t shape_alignment,
    ID3D11Device *device,
    ID3D11DeviceContext *context,
    char *error,
    size_t error_capacity);

__declspec(dllexport) int __cdecl rife_runtime_queue_prewarm_with_device(
    const wchar_t *engine_path,
    const wchar_t *cuda_runtime_path,
    uint32_t source_width,
    uint32_t source_height,
    uint32_t shape_alignment,
    ID3D11Device *device,
    ID3D11DeviceContext *context,
    char *error,
    size_t error_capacity);

__declspec(dllexport) struct rife_runtime *__cdecl rife_runtime_create(
    const struct rife_runtime_config *config,
    char *error,
    size_t error_capacity);

__declspec(dllexport) int __cdecl rife_runtime_process(
    struct rife_runtime *runtime,
    ID3D11Texture2D *frame0,
    uint32_t frame0_slice,
    ID3D11Texture2D *frame1,
    uint32_t frame1_slice,
    ID3D11Texture2D *output,
    uint32_t output_slice,
    double source0_pts,
    double source1_pts,
    struct rife_frame_diagnostics *diagnostics,
    char *error,
    size_t error_capacity);

__declspec(dllexport) void __cdecl rife_runtime_reset(
    struct rife_runtime *runtime);

__declspec(dllexport) int __cdecl rife_runtime_get_stats(
    const struct rife_runtime *runtime,
    struct rife_runtime_stats *stats);

__declspec(dllexport) void __cdecl rife_runtime_destroy(
    struct rife_runtime *runtime);

#ifdef __cplusplus
}
#endif
