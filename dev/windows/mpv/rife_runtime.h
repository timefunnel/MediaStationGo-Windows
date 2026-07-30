#pragma once

#include <stddef.h>
#include <stdint.h>

#include <d3d11.h>

#ifdef __cplusplus
extern "C" {
#endif

#define RIFE_RUNTIME_ABI_VERSION 1u

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

struct rife_runtime_config {
    uint32_t abi_version;
    ID3D11Device *device;
    ID3D11DeviceContext *context;
    const wchar_t *engine_path;
    const wchar_t *cuda_runtime_path;
    uint32_t source_width;
    uint32_t source_height;
    uint32_t color_matrix;
    uint32_t limited_range;
    uint32_t scene_sample_stride;
    uint32_t scene_pixel_threshold;
    float scene_average_threshold;
    float scene_changed_ratio;
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
};

struct rife_runtime;

__declspec(dllexport) uint32_t __cdecl rife_runtime_abi_version(void);

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
    int *scene_cut,
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
