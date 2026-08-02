/*
 * MediaStationGo NVIDIA Optical Flow MEMC filter.
 *
 * Stage 1 is intentionally limited to a D3D11/P010 GPU passthrough. It
 * validates the private UAV-capable P010 frame pool and HDR metadata path
 * before any motion analysis or synthesized frames are introduced.
 */

#include <windows.h>
#include <d3d11.h>
#include <d3d11_3.h>
#include <d3dcompiler.h>
#include <dxgi1_2.h>
#include <math.h>
#include <stdio.h>

#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>

#include "common/tags.h"
#include "filters/filter.h"
#include "filters/filter_internal.h"
#include "filters/user_filters.h"
#include "video/hwdec.h"
#include "video/mp_image.h"

#include "nvOpticalFlowD3D11.h"
#include "rife_runtime.h"

struct opts {
    bool rife;
    char *rife_runtime_dll;
    char *rife_engine;
    char *rife_model;
    char *rife_cudart;
    int rife_source_width;
    int rife_source_height;
    int rife_shape_alignment;
    int rife_scene_sample_stride;
    int rife_scene_pixel_threshold;
    double rife_scene_average_threshold;
    double rife_scene_changed_ratio;
    bool stage1_passthrough;
    bool stage2_timing_test;
    bool stage3_nvof_test;
    bool stage4_synthesis_test;
    bool stage5_flow_infill_test;
    bool stage6_robust_test;
    bool flow_diagnostics;
    double flow_fb_abs;
    double flow_fb_rel;
    double flow_cost_max;
    double flow_confidence_min;
    double infill_luma_threshold;
    int scene_cut_sample_stride;
    int scene_cut_pixel_threshold;
    double scene_cut_average_threshold;
    double scene_cut_changed_ratio;
    bool gpu_timing;
    bool nvof_completion_diagnostics;
};

enum output_phase {
    OUTPUT_NONE,
    OUTPUT_ORIGINAL,
    OUTPUT_INTERMEDIATE_TEST,
    OUTPUT_FINAL,
};

#define OUTPUT_POOL_CAPACITY 16
#define FLOW_INFILL_PASS_COUNT 2
#define FLOW_INFILL_FINAL_INDEX (FLOW_INFILL_PASS_COUNT % 2)
#define SCENE_CUT_COUNTER_COUNT 4
#define SCENE_CUT_SUMMARY_COUNTER_COUNT 2
#define ROBUST_SCENE_REGION_COUNT 9
#define ROBUST_SCENE_HISTOGRAM_BINS 16
#define ROBUST_SCENE_HISTOGRAM_COUNT \
    (ROBUST_SCENE_REGION_COUNT * ROBUST_SCENE_HISTOGRAM_BINS)
#define ROBUST_SCENE_DESCRIPTOR_COUNT \
    (ROBUST_SCENE_HISTOGRAM_COUNT * 2 + 6)
#define ROBUST_SCENE_STATE_COUNT 11
#define ROBUST_SCENE_METRIC_COUNT 7
#define ROBUST_SCENE_SUMMARY_COUNT 41
#define ROBUST_SYNTHESIS_COUNTER_COUNT 19
#define ROBUST_TEMPORAL_CACHE_COUNT 2
#define GPU_PROFILE_RING_SIZE 16
#define GPU_PROFILE_BUCKET_COUNT 2001
#define GPU_PROFILE_BUCKET_MS 0.05

enum gpu_profile_stage {
    GPU_PROFILE_P010_COPY,
    GPU_PROFILE_LUMA_EXTRACT,
    GPU_PROFILE_SCENE_CUT,
    GPU_PROFILE_OCCUPANCY,
    GPU_PROFILE_FLOW_INFILL,
    GPU_PROFILE_SYNTH,
    GPU_PROFILE_STAGE_COUNT,
};

enum robust_scene_class {
    ROBUST_SCENE_NORMAL,
    ROBUST_SCENE_FLASH,
    ROBUST_SCENE_FADE_DISSOLVE,
    ROBUST_SCENE_HARD_CUT,
    ROBUST_SCENE_UNCERTAIN,
    ROBUST_SCENE_CLASS_COUNT,
};

enum robust_scene_state_index {
    ROBUST_SCENE_STATE_CLASS,
    ROBUST_SCENE_STATE_PREVIOUS_CLASS,
    ROBUST_SCENE_STATE_STREAK,
    ROBUST_SCENE_STATE_KL_MILLI,
    ROBUST_SCENE_STATE_AVERAGE_DELTA_MILLI,
    ROBUST_SCENE_STATE_CHANGED_RATIO_MILLI,
    ROBUST_SCENE_STATE_CHROMA_DELTA_MILLI,
    ROBUST_SCENE_STATE_EDGE_DELTA_MILLI,
    ROBUST_SCENE_STATE_EXPOSURE_DELTA_MILLI,
    ROBUST_SCENE_STATE_REGIONAL_KL_MAX_MILLI,
    ROBUST_SCENE_STATE_EXPOSURE_SPREAD_MILLI,
};

enum robust_scene_metric {
    ROBUST_SCENE_METRIC_KL_MILLI,
    ROBUST_SCENE_METRIC_AVERAGE_DELTA,
    ROBUST_SCENE_METRIC_CHANGED_RATIO_MILLI,
    ROBUST_SCENE_METRIC_CHROMA_DELTA,
    ROBUST_SCENE_METRIC_EDGE_DELTA,
    ROBUST_SCENE_METRIC_EXPOSURE_DELTA,
    ROBUST_SCENE_METRIC_REGIONAL_KL_MAX_MILLI,
};

enum robust_synthesis_counter {
    ROBUST_SYNTHESIS_PIXELS,
    ROBUST_SYNTHESIS_SOURCE0,
    ROBUST_SYNTHESIS_SOURCE1,
    ROBUST_SYNTHESIS_BLENDED,
    ROBUST_SYNTHESIS_FALLBACK,
    ROBUST_SYNTHESIS_RAW_CANDIDATE,
    ROBUST_SYNTHESIS_MEDIAN_CANDIDATE,
    ROBUST_SYNTHESIS_BLOCK_CANDIDATE,
    ROBUST_SYNTHESIS_OCCUPANCY0_EMPTY,
    ROBUST_SYNTHESIS_OCCUPANCY1_EMPTY,
    ROBUST_SYNTHESIS_OCCUPANCY0_COLLISION,
    ROBUST_SYNTHESIS_OCCUPANCY1_COLLISION,
    ROBUST_SYNTHESIS_TEMPORAL_CANDIDATE,
    ROBUST_SYNTHESIS_EDGE_VECTOR_CANDIDATE,
    ROBUST_SYNTHESIS_OWNER0_MATCH,
    ROBUST_SYNTHESIS_OWNER1_MATCH,
    ROBUST_SYNTHESIS_OWNER0_REJECT,
    ROBUST_SYNTHESIS_OWNER1_REJECT,
    ROBUST_SYNTHESIS_PROJECTED_OWNER_CANDIDATE,
};

_Static_assert(ROBUST_SCENE_STATE_COUNT ==
               ROBUST_SCENE_STATE_EXPOSURE_SPREAD_MILLI + 1,
               "robust scene state layout changed");
_Static_assert(ROBUST_SCENE_METRIC_COUNT ==
               ROBUST_SCENE_METRIC_REGIONAL_KL_MAX_MILLI + 1,
               "robust scene metric layout changed");
_Static_assert(ROBUST_SCENE_SUMMARY_COUNT ==
               1 + ROBUST_SCENE_CLASS_COUNT +
               ROBUST_SCENE_CLASS_COUNT * ROBUST_SCENE_METRIC_COUNT,
               "robust scene summary layout changed");
_Static_assert(ROBUST_SYNTHESIS_COUNTER_COUNT ==
               ROBUST_SYNTHESIS_PROJECTED_OWNER_CANDIDATE + 1,
               "robust synthesis summary layout changed");

struct gpu_profile_query {
    ID3D11Query *disjoint;
    ID3D11Query *start;
    ID3D11Query *end;
    bool pending;
};

struct gpu_profile_stats {
    struct gpu_profile_query queries[GPU_PROFILE_RING_SIZE];
    int cursor;
    uint64_t samples;
    uint64_t skipped;
    uint64_t invalid;
    double total_ms;
    double max_ms;
    uint64_t buckets[GPU_PROFILE_BUCKET_COUNT];
};

struct gpu_profile_token {
    struct gpu_profile_query *query;
};

enum flow_diagnostic_counter {
    FLOW_DIAG_PIXELS,
    FLOW_DIAG_BOTH_VALID,
    FLOW_DIAG_FORWARD_ONLY,
    FLOW_DIAG_BACKWARD_ONLY,
    FLOW_DIAG_HOLES,
    FLOW_DIAG_FORWARD_OOB,
    FLOW_DIAG_BACKWARD_OOB,
    FLOW_DIAG_FORWARD_INCONSISTENT,
    FLOW_DIAG_BACKWARD_INCONSISTENT,
    FLOW_DIAG_FORWARD_HIGH_COST,
    FLOW_DIAG_BACKWARD_HIGH_COST,
    FLOW_DIAG_FORWARD_COST_SUM,
    FLOW_DIAG_BACKWARD_COST_SUM,
    FLOW_DIAG_LUMA_ABS_SUM,
    FLOW_DIAG_LUMA_LARGE_CHANGE,
    FLOW_DIAG_LARGE_FLOW,
    FLOW_DIAG_FORWARD_RESIDUAL_LE_3,
    FLOW_DIAG_FORWARD_RESIDUAL_LE_6,
    FLOW_DIAG_FORWARD_RESIDUAL_LE_12,
    FLOW_DIAG_BACKWARD_RESIDUAL_LE_3,
    FLOW_DIAG_BACKWARD_RESIDUAL_LE_6,
    FLOW_DIAG_BACKWARD_RESIDUAL_LE_12,
    FLOW_DIAG_FORWARD_RESIDUAL_SUM,
    FLOW_DIAG_BACKWARD_RESIDUAL_SUM,
    FLOW_DIAG_FINAL_FORWARD_SEED,
    FLOW_DIAG_FINAL_FORWARD_PROPAGATED,
    FLOW_DIAG_FINAL_FORWARD_HOLE,
    FLOW_DIAG_FINAL_BACKWARD_SEED,
    FLOW_DIAG_FINAL_BACKWARD_PROPAGATED,
    FLOW_DIAG_FINAL_BACKWARD_HOLE,
    FLOW_DIAG_INVERSE_RESIDUAL_REJECT,
    FLOW_DIAG_PHOTOMETRIC_REJECT,
    FLOW_DIAG_INVERSE_OOB_REJECT,
    FLOW_DIAG_COST_REJECT,
    FLOW_DIAG_COUNTER_COUNT,
};

_Static_assert(FLOW_DIAG_COUNTER_COUNT == 34,
               "flow diagnostics shader counter layout changed");

struct texture_slot {
    ID3D11Texture2D *texture;
    bool in_use;
};

struct texture_pool {
    volatile LONG refs;
    SRWLOCK lock;
    ID3D11Device *device;
    int width;
    int height;
    int created;
    bool views_verified;
    struct texture_slot slots[OUTPUT_POOL_CAPACITY];
};

struct output_ref {
    struct texture_pool *pool;
    int slot;
};

struct nvof_resource {
    ID3D11Texture2D *texture;
    ID3D11UnorderedAccessView *uav;
    ID3D11ShaderResourceView *srv;
    NvOFGPUBufferHandle handle;
};

struct shader_texture_resource {
    ID3D11Texture2D *texture;
    ID3D11UnorderedAccessView *uav;
    ID3D11ShaderResourceView *srv;
};

struct robust_pair_cache {
    struct shader_texture_resource flow_forward;
    struct shader_texture_resource flow_backward;
    struct shader_texture_resource cost_forward;
    struct shader_texture_resource cost_backward;
    ID3D11Buffer *scene_state_buffer;
    ID3D11ShaderResourceView *scene_state_srv;
    bool valid;
};

struct robust_pair_views {
    ID3D11ShaderResourceView *flow_forward;
    ID3D11ShaderResourceView *flow_backward;
    ID3D11ShaderResourceView *cost_forward;
    ID3D11ShaderResourceView *cost_backward;
    ID3D11ShaderResourceView *scene_state;
};

struct robust_synthesis_context {
    struct robust_pair_views previous;
    struct robust_pair_views central;
    struct robust_pair_views next;
    bool previous_available;
    bool next_available;
};

struct robust_temporal_constants {
    uint32_t previous_available;
    uint32_t next_available;
    uint32_t reserved[2];
};

struct nvof_state {
    HMODULE module;
    HMODULE d3dcompiler_module;
    pD3DCompile d3d_compile;
    NV_OF_D3D11_API_FUNCTION_LIST api;
    NvOFHandle handle;
    ID3D11ComputeShader *promote_nv12_shader;
    ID3D11ComputeShader *extract_luma_shader;
    ID3D11ComputeShader *scene_cut_shader;
    ID3D11ComputeShader *robust_scene_descriptor_shader;
    ID3D11ComputeShader *robust_scene_classify_shader;
    ID3D11ComputeShader *robust_occupancy_shader;
    ID3D11ComputeShader *flow_diagnostics_shader;
    ID3D11ComputeShader *prepare_flow_shader;
    ID3D11ComputeShader *infill_flow_shader;
    ID3D11ComputeShader *synthesize_p010_shader;
    ID3D11Buffer *flow_diagnostics_buffer;
    ID3D11Buffer *flow_diagnostics_readback;
    ID3D11UnorderedAccessView *flow_diagnostics_uav;
    ID3D11Buffer *scene_cut_buffer;
    ID3D11UnorderedAccessView *scene_cut_uav;
    ID3D11ShaderResourceView *scene_cut_srv;
    ID3D11Buffer *scene_cut_summary_buffer;
    ID3D11Buffer *scene_cut_summary_readback;
    ID3D11UnorderedAccessView *scene_cut_summary_uav;
    ID3D11Buffer *robust_scene_descriptor_buffer;
    ID3D11UnorderedAccessView *robust_scene_descriptor_uav;
    ID3D11ShaderResourceView *robust_scene_descriptor_srv;
    ID3D11Buffer *robust_scene_state_buffer;
    ID3D11UnorderedAccessView *robust_scene_state_uav;
    ID3D11ShaderResourceView *robust_scene_state_srv;
    ID3D11Buffer *robust_scene_summary_buffer;
    ID3D11UnorderedAccessView *robust_scene_summary_uav;
    ID3D11Buffer *robust_scene_summary_readback;
    ID3D11Buffer *robust_synthesis_summary_buffer;
    ID3D11UnorderedAccessView *robust_synthesis_summary_uav;
    ID3D11Buffer *robust_synthesis_summary_readback;
    ID3D11Buffer *robust_temporal_constants_buffer;
    ID3D11Texture2D *completion_readback;
    struct nvof_resource gray[2];
    struct nvof_resource flow_forward;
    struct nvof_resource flow_backward;
    struct nvof_resource cost_forward;
    struct nvof_resource cost_backward;
    struct nvof_resource global_flow;
    struct nvof_resource flow_state_forward[2];
    struct nvof_resource flow_state_backward[2];
    struct shader_texture_resource occupancy[2];
    struct shader_texture_resource projected_owner[2];
    struct robust_pair_cache pair_cache[ROBUST_TEMPORAL_CACHE_COUNT];
    int width;
    int height;
    uint32_t driver_api_version;
    bool disable_temporal_hints_next;
    uint64_t executes;
    double execute_total_ms;
    double execute_max_ms;
    uint64_t completion_samples;
    double completion_total_ms;
    double completion_max_ms;
    uint64_t diagnostic_pairs;
    uint64_t diagnostic_totals[FLOW_DIAG_COUNTER_COUNT];
    double diagnostic_total_ms;
    double diagnostic_max_ms;
    uint64_t flow_infill_pairs;
    uint64_t scene_cut_pairs;
    uint64_t scene_cuts;
    uint64_t robust_scene_classes[ROBUST_SCENE_CLASS_COUNT];
    uint64_t robust_scene_metric_totals[ROBUST_SCENE_CLASS_COUNT]
                                       [ROBUST_SCENE_METRIC_COUNT];
    uint64_t robust_synthesis_totals[ROBUST_SYNTHESIS_COUNTER_COUNT];
    struct gpu_profile_stats gpu_profile[GPU_PROFILE_STAGE_COUNT];
};

typedef uint32_t (__cdecl *rife_abi_version_fn)(void);
typedef struct rife_runtime *(__cdecl *rife_create_fn)(
    const struct rife_runtime_config *, char *, size_t);
typedef int (__cdecl *rife_process_fn)(
    struct rife_runtime *, ID3D11Texture2D *, uint32_t,
    ID3D11Texture2D *, uint32_t, ID3D11Texture2D *, uint32_t,
    double, double, struct rife_frame_diagnostics *, char *, size_t);
typedef void (__cdecl *rife_reset_fn)(struct rife_runtime *);
typedef int (__cdecl *rife_get_stats_fn)(
    const struct rife_runtime *, struct rife_runtime_stats *);
typedef void (__cdecl *rife_destroy_fn)(struct rife_runtime *);
typedef int (__cdecl *rife_queue_prewarm_fn)(
    const wchar_t *, const wchar_t *, uint32_t, uint32_t, uint32_t,
    ID3D11Device *, ID3D11DeviceContext *, char *, size_t);

struct rife_state {
    HMODULE module;
    rife_abi_version_fn abi_version;
    rife_create_fn create;
    rife_process_fn process;
    rife_reset_fn reset;
    rife_get_stats_fn get_stats;
    rife_destroy_fn destroy;
    rife_queue_prewarm_fn queue_prewarm;
    struct rife_runtime *runtime;
    int width;
    int height;
    int color_matrix;
    int limited_range;
};

struct priv {
    struct opts *opts;
    AVBufferRef *av_device_ref;
    ID3D11Device *device;
    ID3D11DeviceContext *context;
    ID3D11DeviceContext1 *context1;
    ID3D10Multithread *multithread;
    ID3DDeviceContextState *rife_context_state;
    ID3DDeviceContextState *caller_context_state;
    struct texture_pool *pool;
    struct nvof_state nvof;
    struct rife_state rife;
    struct mp_image_params input_params;
    struct mp_image *frame0;
    struct mp_image *frame1;
    struct mp_image *frame2;
    struct mp_image *pending_frame;
    enum output_phase output_phase;
    double pair_duration;
    double next_pair_duration;
    int previous_cache_slot;
    int central_cache_slot;
    bool flush_pair_after_midpoint;
    bool input_eof;
    bool output_eof_sent;
    bool robust_scene_history_reset_pending;
    uint64_t input_frames;
    uint64_t copied_frames;
    uint64_t promoted_frames;
    uint64_t original_frames;
    uint64_t intermediate_test_frames;
    uint64_t synthesized_frames;
    uint64_t scene_cut_midpoints;
    uint64_t discontinuities;
    uint64_t resets;
    uint32_t rife_last_scene_class;
    bool rife_scene_class_initialized;
    uint64_t rife_last_timing_warning_frame;
    bool pool_logged;
};

static bool nvof_synthesis_enabled(const struct priv *p)
{
    return p->opts->stage4_synthesis_test ||
           p->opts->stage5_flow_infill_test ||
           p->opts->stage6_robust_test;
}

static bool synthesis_enabled(const struct priv *p)
{
    return p->opts->rife || nvof_synthesis_enabled(p);
}

static bool nvof_analysis_enabled(const struct priv *p)
{
    return p->opts->stage3_nvof_test || nvof_synthesis_enabled(p);
}

static void lock_d3d11_context(struct priv *p)
{
    mp_assert(p->multithread && p->context1 && p->rife_context_state);
    ID3D10Multithread_Enter(p->multithread);
    mp_assert(!p->caller_context_state);
    ID3D11DeviceContext1_SwapDeviceContextState(
        p->context1, p->rife_context_state, &p->caller_context_state);
    mp_assert(p->caller_context_state);
}

static void unlock_d3d11_context(struct priv *p)
{
    mp_assert(p->multithread && p->context1 && p->caller_context_state);
    ID3D11DeviceContext1_SwapDeviceContextState(
        p->context1, p->caller_context_state, NULL);
    ID3DDeviceContextState_Release(p->caller_context_state);
    p->caller_context_state = NULL;
    ID3D10Multithread_Leave(p->multithread);
}

static bool create_d3d11_context_state_isolation(struct mp_filter *f)
{
    struct priv *p = f->priv;
    ID3D11Device1 *device1 = NULL;
    HRESULT hr = ID3D11Device_QueryInterface(
        p->device, &IID_ID3D11Device1, (void **)&device1);
    if (FAILED(hr) || !device1) {
        MP_ERR(f, "Frame interpolation requires ID3D11Device1 for "
                  "D3D11 context state isolation hr=0x%08lx\n",
               (unsigned long)hr);
        return false;
    }

    hr = ID3D11DeviceContext_QueryInterface(
        p->context, &IID_ID3D11DeviceContext1, (void **)&p->context1);
    if (FAILED(hr) || !p->context1) {
        MP_ERR(f, "Frame interpolation requires ID3D11DeviceContext1 for "
                  "D3D11 context state isolation hr=0x%08lx\n",
               (unsigned long)hr);
        ID3D11Device1_Release(device1);
        return false;
    }

    D3D_FEATURE_LEVEL feature_level = ID3D11Device_GetFeatureLevel(p->device);
    D3D_FEATURE_LEVEL chosen_level = 0;
    hr = ID3D11Device1_CreateDeviceContextState(
        device1, 0, &feature_level, 1, D3D11_SDK_VERSION,
        &IID_ID3D11Device, &chosen_level, &p->rife_context_state);
    ID3D11Device1_Release(device1);
    if (FAILED(hr) || !p->rife_context_state || chosen_level != feature_level) {
        MP_ERR(f, "Frame interpolation could not create an isolated D3D11 "
                  "context state hr=0x%08lx requested=0x%04x chosen=0x%04x\n",
               (unsigned long)hr, (unsigned int)feature_level,
               (unsigned int)chosen_level);
        return false;
    }

    MP_INFO(f, "Frame interpolation D3D11 context state isolation ready "
               "feature-level=0x%04x\n", (unsigned int)chosen_level);
    return true;
}

static wchar_t *utf8_to_wide(void *parent, const char *value)
{
    if (!value || !value[0])
        return NULL;
    int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                                    value, -1, NULL, 0);
    if (count <= 0)
        return NULL;
    wchar_t *wide = talloc_array(parent, wchar_t, count);
    if (!wide)
        return NULL;
    if (!MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                             value, -1, wide, count)) {
        talloc_free(wide);
        return NULL;
    }
    return wide;
}

static bool absolute_windows_path(const wchar_t *path)
{
    return path && ((path[0] && path[1] == L':' &&
                     (path[2] == L'\\' || path[2] == L'/')) ||
                    (path[0] == L'\\' && path[1] == L'\\'));
}

static void destroy_rife_session(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!p->rife.runtime)
        return;
    struct rife_runtime_stats stats = {0};
    if (p->rife.get_stats &&
        p->rife.get_stats(p->rife.runtime, &stats) == RIFE_RUNTIME_OK) {
        MP_INFO(f, "RIFE runtime summary profile=%u pairs=%llu inferred=%llu "
                   "scene-cuts=%llu failures=%llu inference-average-ms=%.3f "
                   "inference-p95-ms=%.3f inference-max-ms=%.3f "
                   "scene-average-ms=%.3f scene-max-ms=%.3f "
                   "cache=%s init-ms=%.3f reuses=%llu "
                   "scene-classes=normal:%llu,flash:%llu,fade:%llu,"
                   "hard-cut:%llu,uncertain:%llu\n",
                stats.optimization_profile,
                (unsigned long long)stats.pairs,
                (unsigned long long)stats.inferred_pairs,
                (unsigned long long)stats.scene_cuts,
                (unsigned long long)stats.failures,
                stats.inferred_pairs
                    ? stats.inference_total_ms / stats.inferred_pairs : 0,
                stats.inference_p95_ms, stats.inference_max_ms,
                stats.pairs ? stats.scene_total_ms / stats.pairs : 0,
                stats.scene_max_ms,
                stats.runtime_cache_hit ? "hit" : "cold",
                stats.runtime_initialization_ms,
                (unsigned long long)stats.runtime_reuses,
                (unsigned long long)stats.scene_classes[RIFE_SCENE_NORMAL],
                (unsigned long long)stats.scene_classes[RIFE_SCENE_FLASH],
                (unsigned long long)stats.scene_classes[
                    RIFE_SCENE_FADE_DISSOLVE],
                (unsigned long long)stats.scene_classes[RIFE_SCENE_HARD_CUT],
                (unsigned long long)stats.scene_classes[
                    RIFE_SCENE_UNCERTAIN]);
    }
    p->rife.destroy(p->rife.runtime);
    p->rife.runtime = NULL;
    p->rife.width = 0;
    p->rife.height = 0;
    p->rife.color_matrix = 0;
    p->rife.limited_range = 0;
}

static void destroy_rife_bridge(struct mp_filter *f)
{
    struct priv *p = f->priv;
    destroy_rife_session(f);
    if (p->rife.module)
        FreeLibrary(p->rife.module);
    p->rife = (struct rife_state){0};
}

static bool load_rife_symbol(struct mp_filter *f, const char *name,
                             FARPROC *address)
{
    struct priv *p = f->priv;
    *address = GetProcAddress(p->rife.module, name);
    if (*address)
        return true;
    MP_ERR(f, "RIFE runtime export is missing name=%s win32=%lu\n",
           name, GetLastError());
    return false;
}

static bool load_rife_bridge(struct mp_filter *f)
{
    struct priv *p = f->priv;
    wchar_t *runtime_path = utf8_to_wide(f, p->opts->rife_runtime_dll);
    wchar_t *engine_path = utf8_to_wide(f, p->opts->rife_engine);
    wchar_t *cudart_path = utf8_to_wide(f, p->opts->rife_cudart);
    if (!runtime_path || !engine_path || !cudart_path ||
        !absolute_windows_path(runtime_path) ||
        !absolute_windows_path(engine_path) ||
        !absolute_windows_path(cudart_path)) {
        MP_ERR(f, "RIFE requires absolute UTF-8 paths for runtime DLL, "
                  "TensorRT engine, and CUDA Runtime\n");
        talloc_free(runtime_path);
        talloc_free(engine_path);
        talloc_free(cudart_path);
        return false;
    }
    p->rife.module = LoadLibraryExW(
        runtime_path, NULL, LOAD_WITH_ALTERED_SEARCH_PATH);
    talloc_free(runtime_path);
    talloc_free(engine_path);
    talloc_free(cudart_path);
    if (!p->rife.module) {
        MP_ERR(f, "RIFE runtime DLL could not be loaded win32=%lu\n",
               GetLastError());
        return false;
    }
    if (!load_rife_symbol(f, "rife_runtime_abi_version",
                          (FARPROC *)&p->rife.abi_version) ||
        !load_rife_symbol(f, "rife_runtime_create",
                          (FARPROC *)&p->rife.create) ||
        !load_rife_symbol(f, "rife_runtime_process",
                          (FARPROC *)&p->rife.process) ||
        !load_rife_symbol(f, "rife_runtime_reset",
                          (FARPROC *)&p->rife.reset) ||
        !load_rife_symbol(f, "rife_runtime_get_stats",
                          (FARPROC *)&p->rife.get_stats) ||
        !load_rife_symbol(f, "rife_runtime_destroy",
                          (FARPROC *)&p->rife.destroy) ||
        !load_rife_symbol(f, "rife_runtime_queue_prewarm_with_device",
                          (FARPROC *)&p->rife.queue_prewarm)) {
        destroy_rife_bridge(f);
        return false;
    }
    uint32_t abi = p->rife.abi_version();
    if (abi != RIFE_RUNTIME_ABI_VERSION) {
        MP_ERR(f, "RIFE runtime ABI mismatch expected=%u actual=%u\n",
               RIFE_RUNTIME_ABI_VERSION, abi);
        destroy_rife_bridge(f);
        return false;
    }
    MP_INFO(f, "RIFE runtime bridge loaded ABI=%u\n", abi);
    return true;
}

static bool queue_rife_prewarm(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!p->opts->rife)
        return true;
    if (p->opts->rife_source_width <= 0
        || p->opts->rife_source_height <= 0
        || (p->opts->rife_shape_alignment != 64
            && p->opts->rife_shape_alignment != 128)) {
        MP_ERR(f, "RIFE background prewarm requires source dimensions and "
                  "a 64 or 128 shape alignment\n");
        return false;
    }
    wchar_t *engine_path = utf8_to_wide(f, p->opts->rife_engine);
    wchar_t *cudart_path = utf8_to_wide(f, p->opts->rife_cudart);
    if (!engine_path || !cudart_path) {
        MP_ERR(f, "RIFE background prewarm paths are not valid UTF-8\n");
        talloc_free(engine_path);
        talloc_free(cudart_path);
        return false;
    }
    char error[1024] = {0};
    const int status = p->rife.queue_prewarm(
        engine_path, cudart_path,
        (uint32_t)p->opts->rife_source_width,
        (uint32_t)p->opts->rife_source_height,
        (uint32_t)p->opts->rife_shape_alignment,
        p->device, p->context, error, sizeof(error));
    talloc_free(engine_path);
    talloc_free(cudart_path);
    if (status != RIFE_RUNTIME_OK) {
        MP_ERR(f, "RIFE background prewarm could not be queued status=%d "
                  "detail=%s\n", status, error[0] ? error : "unknown");
        return false;
    }
    MP_INFO(f, "RIFE background prewarm queued source=%dx%d\n",
            p->opts->rife_source_width, p->opts->rife_source_height);
    return true;
}

static bool rife_color_config(struct mp_filter *f, struct mp_image *frame,
                              int *matrix, int *limited_range)
{
    switch (frame->params.repr.sys) {
    case PL_COLOR_SYSTEM_BT_601:
        *matrix = RIFE_COLOR_MATRIX_BT601;
        break;
    case PL_COLOR_SYSTEM_BT_709:
        *matrix = RIFE_COLOR_MATRIX_BT709;
        break;
    case PL_COLOR_SYSTEM_BT_2020_NC:
    case PL_COLOR_SYSTEM_BT_2100_PQ:
        *matrix = RIFE_COLOR_MATRIX_BT2020_NCL;
        break;
    default:
        MP_ERR(f, "RIFE does not support decoded color matrix=%d\n",
               frame->params.repr.sys);
        return false;
    }
    if (frame->params.repr.levels == PL_COLOR_LEVELS_LIMITED) {
        *limited_range = 1;
    } else if (frame->params.repr.levels == PL_COLOR_LEVELS_FULL) {
        *limited_range = 0;
    } else {
        MP_ERR(f, "RIFE requires explicit limited or full color range, "
                  "received=%d\n", frame->params.repr.levels);
        return false;
    }
    return true;
}

static bool ensure_rife_session(struct mp_filter *f, struct mp_image *frame)
{
    struct priv *p = f->priv;
    int matrix = 0;
    int limited_range = 0;
    if (!rife_color_config(f, frame, &matrix, &limited_range))
        return false;
    if (p->rife.runtime && p->rife.width == frame->w &&
        p->rife.height == frame->h && p->rife.color_matrix == matrix &&
        p->rife.limited_range == limited_range)
        return true;
    destroy_rife_session(f);

    wchar_t *engine_path = utf8_to_wide(f, p->opts->rife_engine);
    wchar_t *cudart_path = utf8_to_wide(f, p->opts->rife_cudart);
    if (!engine_path || !cudart_path) {
        MP_ERR(f, "RIFE engine or CUDA Runtime path is not valid UTF-8\n");
        talloc_free(engine_path);
        talloc_free(cudart_path);
        return false;
    }
    struct rife_runtime_config config = {
        .abi_version = RIFE_RUNTIME_ABI_VERSION,
        .device = p->device,
        .context = p->context,
        .engine_path = engine_path,
        .cuda_runtime_path = cudart_path,
        .source_width = frame->w,
        .source_height = frame->h,
        .shape_alignment = p->opts->rife_shape_alignment,
        .color_matrix = matrix,
        .limited_range = limited_range,
        .scene_sample_stride = p->opts->rife_scene_sample_stride,
        .scene_pixel_threshold = p->opts->rife_scene_pixel_threshold,
        .scene_average_threshold = p->opts->rife_scene_average_threshold,
        .scene_changed_ratio = p->opts->rife_scene_changed_ratio,
        .profiling_enabled = 0,
    };
    char error[1024] = {0};
    MP_VERBOSE(f, "RIFE runtime create begin source=%dx%d matrix=%d "
                  "range=%s\n", frame->w, frame->h, matrix,
               limited_range ? "limited" : "full");
    p->rife.runtime = p->rife.create(&config, error, sizeof(error));
    talloc_free(engine_path);
    talloc_free(cudart_path);
    if (!p->rife.runtime) {
        MP_ERR(f, "RIFE runtime initialization failed detail=%s\n",
               error[0] ? error : "unknown");
        return false;
    }
    p->rife.width = frame->w;
    p->rife.height = frame->h;
    p->rife.color_matrix = matrix;
    p->rife.limited_range = limited_range;
    struct rife_runtime_stats stats = {0};
    if (p->rife.get_stats(p->rife.runtime, &stats) != RIFE_RUNTIME_OK) {
        MP_ERR(f, "RIFE runtime initialization diagnostics unavailable\n");
        destroy_rife_session(f);
        return false;
    }
    MP_INFO(f, "RIFE runtime initialized source=%dx%d format=P010 "
               "matrix=%d range=%s model=%s implementation=1 "
               "backend=TensorRT-RTX FP16 strict-x2 profile=%u cache=%s "
               "init-ms=%.3f reuses=%llu\n",
            frame->w, frame->h, matrix,
            limited_range ? "limited" : "full",
            p->opts->rife_model,
            stats.optimization_profile,
            stats.runtime_cache_hit ? "hit" : "cold",
            stats.runtime_initialization_ms,
            (unsigned long long)stats.runtime_reuses);
    return true;
}

static void format_shader_number(char *buffer, size_t size, double value)
{
    snprintf(buffer, size, "%.9g", value);
    for (char *cursor = buffer; *cursor; cursor++) {
        if (*cursor == ',')
            *cursor = '.';
    }
}

static const char *gpu_profile_stage_name(enum gpu_profile_stage stage)
{
    switch (stage) {
    case GPU_PROFILE_P010_COPY: return "p010-copy";
    case GPU_PROFILE_LUMA_EXTRACT: return "luma-extract";
    case GPU_PROFILE_SCENE_CUT: return "scene-cut";
    case GPU_PROFILE_OCCUPANCY: return "projected-ownership";
    case GPU_PROFILE_FLOW_INFILL: return "flow-infill";
    case GPU_PROFILE_SYNTH: return "p010-synth";
    default: return "unknown";
    }
}

static void release_gpu_profile_queries(struct priv *p)
{
    for (int stage = 0; stage < GPU_PROFILE_STAGE_COUNT; stage++) {
        struct gpu_profile_stats *stats = &p->nvof.gpu_profile[stage];
        for (int index = 0; index < GPU_PROFILE_RING_SIZE; index++) {
            struct gpu_profile_query *query = &stats->queries[index];
            if (query->disjoint)
                ID3D11Query_Release(query->disjoint);
            if (query->start)
                ID3D11Query_Release(query->start);
            if (query->end)
                ID3D11Query_Release(query->end);
            query->disjoint = NULL;
            query->start = NULL;
            query->end = NULL;
            query->pending = false;
        }
    }
}

static bool create_gpu_profile_queries(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!p->opts->gpu_timing)
        return true;
    D3D11_QUERY_DESC disjoint_desc = {
        .Query = D3D11_QUERY_TIMESTAMP_DISJOINT,
    };
    D3D11_QUERY_DESC timestamp_desc = {
        .Query = D3D11_QUERY_TIMESTAMP,
    };
    for (int stage = 0; stage < GPU_PROFILE_STAGE_COUNT; stage++) {
        struct gpu_profile_stats *stats = &p->nvof.gpu_profile[stage];
        for (int index = 0; index < GPU_PROFILE_RING_SIZE; index++) {
            struct gpu_profile_query *query = &stats->queries[index];
            HRESULT hr = ID3D11Device_CreateQuery(
                p->device, &disjoint_desc, &query->disjoint);
            if (SUCCEEDED(hr)) {
                hr = ID3D11Device_CreateQuery(
                    p->device, &timestamp_desc, &query->start);
            }
            if (SUCCEEDED(hr)) {
                hr = ID3D11Device_CreateQuery(
                    p->device, &timestamp_desc, &query->end);
            }
            if (FAILED(hr)) {
                MP_ERR(f, "Frame interpolation GPU timestamp query creation "
                          "failed "
                          "stage=%s slot=%d hr=0x%08lx\n",
                       gpu_profile_stage_name(stage), index,
                       (unsigned long)hr);
                release_gpu_profile_queries(p);
                return false;
            }
        }
    }
    MP_INFO(f, "Frame interpolation asynchronous GPU timestamp queries ready "
               "stages=%d ring-size=%d bucket-ms=%.2f\n",
            GPU_PROFILE_STAGE_COUNT, GPU_PROFILE_RING_SIZE,
            GPU_PROFILE_BUCKET_MS);
    return true;
}

static bool collect_gpu_profile_query_locked(
    struct priv *p, struct gpu_profile_stats *stats,
    struct gpu_profile_query *query)
{
    if (!query->pending)
        return true;
    D3D11_QUERY_DATA_TIMESTAMP_DISJOINT disjoint = {0};
    UINT64 start = 0;
    UINT64 end = 0;
    HRESULT disjoint_hr = ID3D11DeviceContext_GetData(
        p->context, (ID3D11Asynchronous *)query->disjoint,
        &disjoint, sizeof(disjoint), D3D11_ASYNC_GETDATA_DONOTFLUSH);
    HRESULT start_hr = ID3D11DeviceContext_GetData(
        p->context, (ID3D11Asynchronous *)query->start,
        &start, sizeof(start), D3D11_ASYNC_GETDATA_DONOTFLUSH);
    HRESULT end_hr = ID3D11DeviceContext_GetData(
        p->context, (ID3D11Asynchronous *)query->end,
        &end, sizeof(end), D3D11_ASYNC_GETDATA_DONOTFLUSH);
    if (disjoint_hr == S_FALSE || start_hr == S_FALSE || end_hr == S_FALSE)
        return false;
    query->pending = false;
    if (FAILED(disjoint_hr) || FAILED(start_hr) || FAILED(end_hr) ||
        disjoint.Disjoint || !disjoint.Frequency || end < start) {
        stats->invalid++;
        return true;
    }
    double elapsed_ms = (end - start) * 1000.0 / disjoint.Frequency;
    stats->samples++;
    stats->total_ms += elapsed_ms;
    stats->max_ms = MPMAX(stats->max_ms, elapsed_ms);
    int bucket = (int)(elapsed_ms / GPU_PROFILE_BUCKET_MS);
    bucket = MPCLAMP(bucket, 0, GPU_PROFILE_BUCKET_COUNT - 1);
    stats->buckets[bucket]++;
    return true;
}

static int collect_gpu_profile_stage_locked(struct priv *p,
                                             enum gpu_profile_stage stage)
{
    struct gpu_profile_stats *stats = &p->nvof.gpu_profile[stage];
    int pending = 0;
    for (int index = 0; index < GPU_PROFILE_RING_SIZE; index++) {
        struct gpu_profile_query *query = &stats->queries[index];
        if (!collect_gpu_profile_query_locked(p, stats, query))
            pending++;
    }
    return pending;
}

static struct gpu_profile_token gpu_profile_begin_locked(
    struct priv *p, enum gpu_profile_stage stage)
{
    struct gpu_profile_token token = {0};
    if (!p->opts->gpu_timing)
        return token;
    struct gpu_profile_stats *stats = &p->nvof.gpu_profile[stage];
    collect_gpu_profile_stage_locked(p, stage);
    struct gpu_profile_query *query = &stats->queries[stats->cursor];
    stats->cursor = (stats->cursor + 1) % GPU_PROFILE_RING_SIZE;
    if (query->pending) {
        stats->skipped++;
        return token;
    }
    ID3D11DeviceContext_Begin(
        p->context, (ID3D11Asynchronous *)query->disjoint);
    ID3D11DeviceContext_End(
        p->context, (ID3D11Asynchronous *)query->start);
    token.query = query;
    return token;
}

static void gpu_profile_end_locked(struct priv *p,
                                   struct gpu_profile_token token)
{
    if (!token.query)
        return;
    ID3D11DeviceContext_End(
        p->context, (ID3D11Asynchronous *)token.query->end);
    ID3D11DeviceContext_End(
        p->context, (ID3D11Asynchronous *)token.query->disjoint);
    token.query->pending = true;
}

static int drain_gpu_profile_queries(struct priv *p)
{
    if (!p->opts->gpu_timing || !p->context)
        return 0;
    lock_d3d11_context(p);
    ID3D11DeviceContext_Flush(p->context);
    unlock_d3d11_context(p);
    int pending = 0;
    for (int attempt = 0; attempt < 2000; attempt++) {
        pending = 0;
        lock_d3d11_context(p);
        for (int stage = 0; stage < GPU_PROFILE_STAGE_COUNT; stage++)
            pending += collect_gpu_profile_stage_locked(p, stage);
        unlock_d3d11_context(p);
        if (!pending)
            break;
        Sleep(1);
    }
    return pending;
}

static double gpu_profile_percentile(const struct gpu_profile_stats *stats,
                                     double percentile)
{
    if (!stats->samples)
        return 0;
    uint64_t target = (uint64_t)ceil(stats->samples * percentile);
    target = MPMAX(target, 1);
    uint64_t accumulated = 0;
    for (int bucket = 0; bucket < GPU_PROFILE_BUCKET_COUNT; bucket++) {
        accumulated += stats->buckets[bucket];
        if (accumulated >= target)
            return (bucket + 1) * GPU_PROFILE_BUCKET_MS;
    }
    return stats->max_ms;
}

typedef NV_OF_STATUS(NVOFAPI *nvof_get_max_version_fn)(uint32_t *version);
typedef NV_OF_STATUS(NVOFAPI *nvof_create_instance_d3d11_fn)(
    uint32_t api_version, NV_OF_D3D11_API_FUNCTION_LIST *functions);

static const char extract_luma_shader_source[] =
    "Texture2D<float> source_y : register(t0);\n"
    "RWTexture2D<float> output_gray : register(u0);\n"
    "[numthreads(16, 16, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID)\n"
    "{\n"
    "    uint width, height;\n"
    "    output_gray.GetDimensions(width, height);\n"
    "    if (id.x >= width || id.y >= height)\n"
    "        return;\n"
    "    float stored = source_y.Load(int3(id.xy, 0));\n"
    "    float y10 = round(stored * (65535.0 / 64.0));\n"
    "    float gray8 = round(y10 * (255.0 / 1023.0));\n"
    "    output_gray[id.xy] = gray8 / 255.0;\n"
    "}\n";

static const char promote_nv12_shader_source[] =
    "Texture2D<float> source_y : register(t0);\n"
    "Texture2D<float2> source_uv : register(t1);\n"
    "RWTexture2D<float> output_y : register(u0);\n"
    "RWTexture2D<float2> output_uv : register(u1);\n"
    "float promote_code(float value)\n"
    "{\n"
    "    float code8 = round(saturate(value) * 255.0);\n"
    "    return code8 * 256.0 / 65535.0;\n"
    "}\n"
    "[numthreads(8, 8, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID)\n"
    "{\n"
    "    uint w, h;\n"
    "    output_y.GetDimensions(w, h);\n"
    "    if (id.x < w && id.y < h)\n"
    "        output_y[id.xy] = promote_code(source_y.Load(int3(id.xy, 0)));\n"
    "    uint uvw, uvh;\n"
    "    output_uv.GetDimensions(uvw, uvh);\n"
    "    if (id.x < uvw && id.y < uvh) {\n"
    "        float2 value = source_uv.Load(int3(id.xy, 0));\n"
    "        output_uv[id.xy] = float2(promote_code(value.x),\n"
    "                                      promote_code(value.y));\n"
    "    }\n"
    "}\n";

static const char scene_cut_shader_source[] =
    "Texture2D<float> gray0 : register(t0);\n"
    "Texture2D<float> gray1 : register(t1);\n"
    "RWStructuredBuffer<uint> output_counters : register(u0);\n"
    "groupshared uint counters[4];\n"
    "[numthreads(16, 16, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID,\n"
    "          uint group_index : SV_GroupIndex)\n"
    "{\n"
    "    if (group_index < 4)\n"
    "        counters[group_index] = 0;\n"
    "    GroupMemoryBarrierWithGroupSync();\n"
    "    uint w, h;\n"
    "    gray0.GetDimensions(w, h);\n"
    "    uint sampled_w = (w + SCENE_SAMPLE_STRIDE - 1) /\n"
    "                     SCENE_SAMPLE_STRIDE;\n"
    "    uint sampled_h = (h + SCENE_SAMPLE_STRIDE - 1) /\n"
    "                     SCENE_SAMPLE_STRIDE;\n"
    "    if (id.x < sampled_w && id.y < sampled_h) {\n"
    "        uint2 p = min(id.xy * SCENE_SAMPLE_STRIDE +\n"
    "                      SCENE_SAMPLE_STRIDE / 2, uint2(w - 1, h - 1));\n"
    "        uint delta = (uint)round(abs(gray0.Load(int3(p, 0)) -\n"
    "                                    gray1.Load(int3(p, 0))) * 255.0);\n"
    "        InterlockedAdd(counters[0], 1);\n"
    "        InterlockedAdd(counters[1], delta);\n"
    "        if (delta >= SCENE_PIXEL_THRESHOLD)\n"
    "            InterlockedAdd(counters[2], 1);\n"
    "        if (delta >= min(255, SCENE_PIXEL_THRESHOLD * 2))\n"
    "            InterlockedAdd(counters[3], 1);\n"
    "    }\n"
    "    GroupMemoryBarrierWithGroupSync();\n"
    "    if (group_index < 4)\n"
    "        InterlockedAdd(output_counters[group_index],\n"
    "                       counters[group_index]);\n"
    "}\n";

static const char robust_scene_descriptor_shader_source[] =
    "Texture2D<float> gray0 : register(t0);\n"
    "Texture2D<float> gray1 : register(t1);\n"
    "Texture2D<float2> uv0 : register(t2);\n"
    "Texture2D<float2> uv1 : register(t3);\n"
    "RWStructuredBuffer<uint> descriptor : register(u0);\n"
    "groupshared uint counters[ROBUST_DESCRIPTOR_COUNT];\n"
    "\n"
    "float edge_strength(Texture2D<float> image, int2 p, uint w, uint h)\n"
    "{\n"
    "    int2 left = int2(max(p.x - 1, 0), p.y);\n"
    "    int2 right = int2(min(p.x + 1, (int)w - 1), p.y);\n"
    "    int2 top = int2(p.x, max(p.y - 1, 0));\n"
    "    int2 bottom = int2(p.x, min(p.y + 1, (int)h - 1));\n"
    "    float dx = abs(image.Load(int3(right, 0)) -\n"
    "                   image.Load(int3(left, 0)));\n"
    "    float dy = abs(image.Load(int3(bottom, 0)) -\n"
    "                   image.Load(int3(top, 0)));\n"
    "    return saturate((dx + dy) * 0.5);\n"
    "}\n"
    "\n"
    "[numthreads(16, 16, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID,\n"
    "          uint group_index : SV_GroupIndex)\n"
    "{\n"
    "    for (uint clear_index = group_index;\n"
    "         clear_index < ROBUST_DESCRIPTOR_COUNT; clear_index += 256)\n"
    "        counters[clear_index] = 0;\n"
    "    GroupMemoryBarrierWithGroupSync();\n"
    "    uint w, h;\n"
    "    gray0.GetDimensions(w, h);\n"
    "    uint sampled_w = (w + SCENE_SAMPLE_STRIDE - 1) /\n"
    "                     SCENE_SAMPLE_STRIDE;\n"
    "    uint sampled_h = (h + SCENE_SAMPLE_STRIDE - 1) /\n"
    "                     SCENE_SAMPLE_STRIDE;\n"
    "    if (id.x < sampled_w && id.y < sampled_h) {\n"
    "        uint2 p = min(id.xy * SCENE_SAMPLE_STRIDE +\n"
    "                      SCENE_SAMPLE_STRIDE / 2, uint2(w - 1, h - 1));\n"
    "        float luma0 = saturate(gray0.Load(int3(p, 0)));\n"
    "        float luma1 = saturate(gray1.Load(int3(p, 0)));\n"
    "        uint region_x = min(p.x * 3 / max(w, 1), 2);\n"
    "        uint region_y = min(p.y * 3 / max(h, 1), 2);\n"
    "        uint region = region_y * 3 + region_x;\n"
    "        uint bin0 = min((uint)(luma0 * ROBUST_HISTOGRAM_BINS),\n"
    "                        ROBUST_HISTOGRAM_BINS - 1);\n"
    "        uint bin1 = min((uint)(luma1 * ROBUST_HISTOGRAM_BINS),\n"
    "                        ROBUST_HISTOGRAM_BINS - 1);\n"
    "        uint histogram0 = region * ROBUST_HISTOGRAM_BINS + bin0;\n"
    "        uint histogram1 = ROBUST_HISTOGRAM_COUNT +\n"
    "                          region * ROBUST_HISTOGRAM_BINS + bin1;\n"
    "        InterlockedAdd(counters[histogram0], 1);\n"
    "        InterlockedAdd(counters[histogram1], 1);\n"
    "\n"
    "        uint luma_delta = (uint)round(abs(luma0 - luma1) * 255.0);\n"
    "        uint2 uvp = min(p / 2, uint2((w - 1) / 2, (h - 1) / 2));\n"
    "        float2 chroma0 = uv0.Load(int3(uvp, 0));\n"
    "        float2 chroma1 = uv1.Load(int3(uvp, 0));\n"
    "        uint chroma_delta = (uint)round(\n"
    "            (abs(chroma0.x - chroma1.x) +\n"
    "             abs(chroma0.y - chroma1.y)) * 127.5);\n"
    "        float edge0 = edge_strength(gray0, (int2)p, w, h);\n"
    "        float edge1 = edge_strength(gray1, (int2)p, w, h);\n"
    "        uint edge_delta = (uint)round(abs(edge0 - edge1) * 255.0);\n"
    "        InterlockedAdd(counters[ROBUST_HISTOGRAM_COUNT * 2 + 0],\n"
    "                       luma_delta);\n"
    "        InterlockedAdd(counters[ROBUST_HISTOGRAM_COUNT * 2 + 1],\n"
    "                       chroma_delta);\n"
    "        InterlockedAdd(counters[ROBUST_HISTOGRAM_COUNT * 2 + 2],\n"
    "                       edge_delta);\n"
    "        if (luma_delta >= SCENE_PIXEL_THRESHOLD)\n"
    "            InterlockedAdd(\n"
    "                counters[ROBUST_HISTOGRAM_COUNT * 2 + 3], 1);\n"
    "        if (luma_delta >= min(255, SCENE_PIXEL_THRESHOLD * 2))\n"
    "            InterlockedAdd(\n"
    "                counters[ROBUST_HISTOGRAM_COUNT * 2 + 4], 1);\n"
    "        InterlockedAdd(counters[ROBUST_HISTOGRAM_COUNT * 2 + 5], 1);\n"
    "    }\n"
    "    GroupMemoryBarrierWithGroupSync();\n"
    "    for (uint output_index = group_index;\n"
    "         output_index < ROBUST_DESCRIPTOR_COUNT; output_index += 256)\n"
    "        InterlockedAdd(descriptor[output_index],\n"
    "                       counters[output_index]);\n"
    "}\n";

static const char robust_scene_classify_shader_source[] =
    "StructuredBuffer<uint> descriptor : register(t0);\n"
    "RWStructuredBuffer<uint> scene_state : register(u0);\n"
    "RWStructuredBuffer<uint> scene_summary : register(u1);\n"
    "\n"
    "float symmetric_kl(float a, float b)\n"
    "{\n"
    "    return 0.5 * (a * log2(a / b) + b * log2(b / a));\n"
    "}\n"
    "\n"
    "[numthreads(1, 1, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID)\n"
    "{\n"
    "    float kl_sum = 0.0;\n"
    "    float kl_max = 0.0;\n"
    "    float exposure_sum = 0.0;\n"
    "    float exposure_min = 255.0;\n"
    "    float exposure_max = -255.0;\n"
    "    [unroll]\n"
    "    for (uint region = 0; region < ROBUST_REGION_COUNT; region++) {\n"
    "        float samples0 = 0.0;\n"
    "        float samples1 = 0.0;\n"
    "        float mean0 = 0.0;\n"
    "        float mean1 = 0.0;\n"
    "        [unroll]\n"
    "        for (uint mean_bin = 0; mean_bin < ROBUST_HISTOGRAM_BINS;\n"
    "             mean_bin++) {\n"
    "            uint index = region * ROBUST_HISTOGRAM_BINS + mean_bin;\n"
    "            float count0 = (float)descriptor[index];\n"
    "            float count1 = (float)descriptor[\n"
    "                ROBUST_HISTOGRAM_COUNT + index];\n"
    "            float center = ((float)mean_bin + 0.5) *\n"
    "                           (255.0 / ROBUST_HISTOGRAM_BINS);\n"
    "            samples0 += count0;\n"
    "            samples1 += count1;\n"
    "            mean0 += count0 * center;\n"
    "            mean1 += count1 * center;\n"
    "        }\n"
    "        float denominator0 = samples0 +\n"
    "            0.5 * ROBUST_HISTOGRAM_BINS;\n"
    "        float denominator1 = samples1 +\n"
    "            0.5 * ROBUST_HISTOGRAM_BINS;\n"
    "        float region_kl = 0.0;\n"
    "        [unroll]\n"
    "        for (uint kl_bin = 0; kl_bin < ROBUST_HISTOGRAM_BINS;\n"
    "             kl_bin++) {\n"
    "            uint index = region * ROBUST_HISTOGRAM_BINS + kl_bin;\n"
    "            float probability0 = ((float)descriptor[index] + 0.5) /\n"
    "                                 denominator0;\n"
    "            float probability1 = ((float)descriptor[\n"
    "                ROBUST_HISTOGRAM_COUNT + index] + 0.5) /\n"
    "                denominator1;\n"
    "            region_kl += symmetric_kl(probability0, probability1);\n"
    "        }\n"
    "        float shift = samples0 > 0.0 && samples1 > 0.0\n"
    "            ? mean1 / samples1 - mean0 / samples0 : 0.0;\n"
    "        kl_sum += region_kl;\n"
    "        kl_max = max(kl_max, region_kl);\n"
    "        exposure_sum += abs(shift);\n"
    "        exposure_min = min(exposure_min, shift);\n"
    "        exposure_max = max(exposure_max, shift);\n"
    "    }\n"
    "\n"
    "    float samples = max((float)descriptor[\n"
    "        ROBUST_HISTOGRAM_COUNT * 2 + 5], 1.0);\n"
    "    float average_delta = (float)descriptor[\n"
    "        ROBUST_HISTOGRAM_COUNT * 2 + 0] / samples;\n"
    "    float chroma_delta = (float)descriptor[\n"
    "        ROBUST_HISTOGRAM_COUNT * 2 + 1] / samples;\n"
    "    float edge_delta = (float)descriptor[\n"
    "        ROBUST_HISTOGRAM_COUNT * 2 + 2] / samples;\n"
    "    float changed_ratio = (float)descriptor[\n"
    "        ROBUST_HISTOGRAM_COUNT * 2 + 3] / samples;\n"
    "    float average_kl = kl_sum / ROBUST_REGION_COUNT;\n"
    "    float exposure_delta = exposure_sum / ROBUST_REGION_COUNT;\n"
    "    float exposure_spread = exposure_max - exposure_min;\n"
    "    uint previous = scene_state[ROBUST_STATE_CLASS];\n"
    "    uint previous_streak = scene_state[ROBUST_STATE_STREAK];\n"
    "    float previous_kl = (float)scene_state[\n"
    "        ROBUST_STATE_KL_MILLI] * 0.001;\n"
    "    float previous_delta = (float)scene_state[\n"
    "        ROBUST_STATE_AVERAGE_DELTA_MILLI] * 0.001;\n"
    "    float previous_changed_ratio = (float)scene_state[\n"
    "        ROBUST_STATE_CHANGED_RATIO_MILLI] * 0.001;\n"
    "\n"
    "    bool flash = exposure_delta >= 18.0 &&\n"
    "        exposure_spread <= 12.0 && changed_ratio >= 0.25 &&\n"
    "        average_kl < 0.14 && edge_delta < 12.0 &&\n"
    "        chroma_delta < 12.0;\n"
    "    bool hard_cut = average_delta >= 24.0 &&\n"
    "        changed_ratio >= 0.42 && average_kl >= 0.11 &&\n"
    "        (kl_max >= 0.22 || chroma_delta >= 10.0 ||\n"
    "         edge_delta >= 10.0);\n"
    "    bool exposure_fade = exposure_delta >= 6.0 &&\n"
    "        exposure_spread <= 20.0 && changed_ratio >= 0.12 &&\n"
    "        average_kl >= 0.025 && average_kl < 0.18 &&\n"
    "        edge_delta < 18.0;\n"
    "    bool sustained_dissolve =\n"
    "        (previous == ROBUST_SCENE_UNCERTAIN ||\n"
    "         previous == ROBUST_SCENE_FADE_DISSOLVE) &&\n"
    "        previous_kl >= 0.30 && average_kl >= 0.30 &&\n"
    "        previous_delta >= 3.0 && previous_delta <= 16.0 &&\n"
    "        average_delta >= 3.0 && average_delta <= 16.0 &&\n"
    "        abs(previous_delta - average_delta) <= 8.0 &&\n"
    "        previous_changed_ratio <= 0.08 && changed_ratio <= 0.08 &&\n"
    "        edge_delta < 4.0;\n"
    "    bool fade = exposure_fade || sustained_dissolve;\n"
    "    bool uncertain = (average_delta >= 12.0 &&\n"
    "                      changed_ratio >= 0.18) ||\n"
    "                     average_kl >= 0.08;\n"
    "\n"
    "    uint candidate = ROBUST_SCENE_NORMAL;\n"
    "    if (flash)\n"
    "        candidate = ROBUST_SCENE_FLASH;\n"
    "    else if (hard_cut)\n"
    "        candidate = ROBUST_SCENE_HARD_CUT;\n"
    "    else if (fade)\n"
    "        candidate = ROBUST_SCENE_FADE_DISSOLVE;\n"
    "    else if (uncertain)\n"
    "        candidate = ROBUST_SCENE_UNCERTAIN;\n"
    "\n"
    "    uint classification = candidate;\n"
    "    if (candidate == ROBUST_SCENE_UNCERTAIN &&\n"
    "        (previous == ROBUST_SCENE_FLASH ||\n"
    "         previous == ROBUST_SCENE_FADE_DISSOLVE) &&\n"
    "        previous_streak < 3)\n"
    "        classification = previous;\n"
    "    else if (candidate == ROBUST_SCENE_NORMAL &&\n"
    "             previous == ROBUST_SCENE_FADE_DISSOLVE &&\n"
    "             exposure_delta >= 3.0 && average_kl >= 0.012)\n"
    "        classification = previous;\n"
    "\n"
    "    scene_state[ROBUST_STATE_PREVIOUS_CLASS] = previous;\n"
    "    scene_state[ROBUST_STATE_CLASS] = classification;\n"
    "    scene_state[ROBUST_STATE_STREAK] = classification == previous\n"
    "        ? min(previous_streak + 1, 255) : 1;\n"
    "    scene_state[ROBUST_STATE_KL_MILLI] =\n"
    "        (uint)round(average_kl * 1000.0);\n"
    "    scene_state[ROBUST_STATE_AVERAGE_DELTA_MILLI] =\n"
    "        (uint)round(average_delta * 1000.0);\n"
    "    scene_state[ROBUST_STATE_CHANGED_RATIO_MILLI] =\n"
    "        (uint)round(changed_ratio * 1000.0);\n"
    "    scene_state[ROBUST_STATE_CHROMA_DELTA_MILLI] =\n"
    "        (uint)round(chroma_delta * 1000.0);\n"
    "    scene_state[ROBUST_STATE_EDGE_DELTA_MILLI] =\n"
    "        (uint)round(edge_delta * 1000.0);\n"
    "    scene_state[ROBUST_STATE_EXPOSURE_DELTA_MILLI] =\n"
    "        (uint)round(exposure_delta * 1000.0);\n"
    "    scene_state[ROBUST_STATE_REGIONAL_KL_MAX_MILLI] =\n"
    "        (uint)round(kl_max * 1000.0);\n"
    "    scene_state[ROBUST_STATE_EXPOSURE_SPREAD_MILLI] =\n"
    "        (uint)round(exposure_spread * 1000.0);\n"
    "    InterlockedAdd(scene_summary[0], 1);\n"
    "    InterlockedAdd(scene_summary[1 + classification], 1);\n"
    "    uint metric_base = 1 + ROBUST_SCENE_CLASS_COUNT +\n"
    "        classification * ROBUST_SCENE_METRIC_COUNT;\n"
    "    InterlockedAdd(scene_summary[metric_base + 0],\n"
    "                   (uint)round(average_kl * 1000.0));\n"
    "    InterlockedAdd(scene_summary[metric_base + 1],\n"
    "                   (uint)round(average_delta));\n"
    "    InterlockedAdd(scene_summary[metric_base + 2],\n"
    "                   (uint)round(changed_ratio * 1000.0));\n"
    "    InterlockedAdd(scene_summary[metric_base + 3],\n"
    "                   (uint)round(chroma_delta));\n"
    "    InterlockedAdd(scene_summary[metric_base + 4],\n"
    "                   (uint)round(edge_delta));\n"
    "    InterlockedAdd(scene_summary[metric_base + 5],\n"
    "                   (uint)round(exposure_delta));\n"
    "    InterlockedAdd(scene_summary[metric_base + 6],\n"
    "                   (uint)round(kl_max * 1000.0));\n"
    "}\n";

static const char flow_diagnostics_shader_source[] =
    "Texture2D<int2> flow0 : register(t0);\n"
    "Texture2D<int2> flow1 : register(t1);\n"
    "Texture2D<uint> cost0 : register(t2);\n"
    "Texture2D<uint> cost1 : register(t3);\n"
    "Texture2D<float> gray0 : register(t4);\n"
    "Texture2D<float> gray1 : register(t5);\n"
    "#if USE_FILLED_FLOW\n"
    "Texture2D<float4> final_state0 : register(t6);\n"
    "Texture2D<float4> final_state1 : register(t7);\n"
    "#endif\n"
    "RWStructuredBuffer<uint> output_counters : register(u0);\n"
    "groupshared uint counters[34];\n"
    "\n"
    "float2 clamp_coord(float2 p, uint w, uint h)\n"
    "{\n"
    "    return clamp(p, float2(0.0, 0.0),\n"
    "                  float2((float)w - 1.0, (float)h - 1.0));\n"
    "}\n"
    "\n"
    "bool in_frame(float2 p, uint w, uint h)\n"
    "{\n"
    "    return p.x >= 0.0 && p.y >= 0.0 && p.x <= (float)w - 1.0 &&\n"
    "           p.y <= (float)h - 1.0;\n"
    "}\n"
    "\n"
    "float2 sample_flow(Texture2D<int2> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp_coord(p, w, h);\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float2 v00 = (float2)tex.Load(int3(a, 0));\n"
    "    float2 v10 = (float2)tex.Load(int3(int2(b.x, a.y), 0));\n"
    "    float2 v01 = (float2)tex.Load(int3(int2(a.x, b.y), 0));\n"
    "    float2 v11 = (float2)tex.Load(int3(b, 0));\n"
    "    return lerp(lerp(v00, v10, f.x),\n"
    "                lerp(v01, v11, f.x), f.y) / 32.0;\n"
    "}\n"
    "\n"
    "float sample_cost(Texture2D<uint> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp_coord(p, w, h);\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float v00 = (float)(tex.Load(int3(a, 0)) & 255);\n"
    "    float v10 = (float)(tex.Load(int3(int2(b.x, a.y), 0)) & 255);\n"
    "    float v01 = (float)(tex.Load(int3(int2(a.x, b.y), 0)) & 255);\n"
    "    float v11 = (float)(tex.Load(int3(b, 0)) & 255);\n"
    "    return lerp(lerp(v00, v10, f.x),\n"
    "                lerp(v01, v11, f.x), f.y) / 255.0;\n"
    "}\n"
    "\n"
    "void solve_sources(float2 target, uint w, uint h, out float2 p0,\n"
    "                   out float2 p1)\n"
    "{\n"
    "    p0 = target;\n"
    "    p1 = target;\n"
    "    [unroll]\n"
    "    for (uint n = 0; n < 2; n++) {\n"
    "        p0 = target - 0.5 * sample_flow(flow0, p0, w, h);\n"
    "        p1 = target - 0.5 * sample_flow(flow1, p1, w, h);\n"
    "    }\n"
    "}\n"
    "\n"
    "float4 analyze_forward(float2 p, uint w, uint h)\n"
    "{\n"
    "    float cost = sample_cost(cost0, p, w, h);\n"
    "    float2 f = sample_flow(flow0, p, w, h);\n"
    "    float2 q = p + f;\n"
    "    if (!in_frame(q, w, h))\n"
    "        return float4(0.0, 1.0, 0.0, cost);\n"
    "    float2 b = sample_flow(flow1, q, w, h);\n"
    "    float residual = length(f + b);\n"
    "    float threshold = 1.5 + 0.05 * length(f);\n"
    "    float consistency = saturate(1.0 - residual / threshold);\n"
    "    return float4(consistency * (1.0 - cost), 0.0, residual, cost);\n"
    "}\n"
    "\n"
    "float4 analyze_backward(float2 p, uint w, uint h)\n"
    "{\n"
    "    float cost = sample_cost(cost1, p, w, h);\n"
    "    float2 b = sample_flow(flow1, p, w, h);\n"
    "    float2 q = p + b;\n"
    "    if (!in_frame(q, w, h))\n"
    "        return float4(0.0, 1.0, 0.0, cost);\n"
    "    float2 f = sample_flow(flow0, q, w, h);\n"
    "    float residual = length(b + f);\n"
    "    float threshold = 1.5 + 0.05 * length(b);\n"
    "    float consistency = saturate(1.0 - residual / threshold);\n"
    "    return float4(consistency * (1.0 - cost), 0.0, residual, cost);\n"
    "}\n"
    "\n"
    "[numthreads(16, 16, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID,\n"
    "          uint group_index : SV_GroupIndex)\n"
    "{\n"
    "    if (group_index < 34)\n"
    "        counters[group_index] = 0;\n"
    "    GroupMemoryBarrierWithGroupSync();\n"
    "    uint w, h;\n"
    "    gray0.GetDimensions(w, h);\n"
    "    bool active = id.x < w && id.y < h;\n"
    "    if (active) {\n"
    "\n"
    "    float2 target = (float2)id.xy;\n"
    "#if USE_FILLED_FLOW\n"
    "    float4 final0 = final_state0.Load(int3(id.xy, 0));\n"
    "    float4 final1 = final_state1.Load(int3(id.xy, 0));\n"
    "    bool valid0 = final0.z > FLOW_CONFIDENCE_MIN;\n"
    "    bool valid1 = final1.z > FLOW_CONFIDENCE_MIN;\n"
    "    float flow0_length = length(final0.xy);\n"
    "    float flow1_length = length(final1.xy);\n"
    "    float2 q0 = target + final0.xy;\n"
    "    float2 q1 = target + final1.xy;\n"
    "    bool oob0 = !in_frame(q0, w, h);\n"
    "    bool oob1 = !in_frame(q1, w, h);\n"
    "    float residual0 = oob0 ? 0.0 : length(final0.xy +\n"
    "        final_state1.Load(int3((int2)round(q0), 0)).xy);\n"
    "    float residual1 = oob1 ? 0.0 : length(final1.xy +\n"
    "        final_state0.Load(int3((int2)round(q1), 0)).xy);\n"
    "    float cost_fwd = sample_cost(cost0, target, w, h);\n"
    "    float cost_bwd = sample_cost(cost1, target, w, h);\n"
    "    float4 fwd = float4(final0.z, oob0 ? 1.0 : 0.0,\n"
    "                        residual0, cost_fwd);\n"
    "    float4 bwd = float4(final1.z, oob1 ? 1.0 : 0.0,\n"
    "                        residual1, cost_bwd);\n"
    "#else\n"
    "    float2 p0, p1;\n"
    "    solve_sources(target, w, h, p0, p1);\n"
    "    float4 fwd = analyze_forward(p0, w, h);\n"
    "    float4 bwd = analyze_backward(p1, w, h);\n"
    "    bool valid0 = fwd.x > FLOW_CONFIDENCE_MIN;\n"
    "    bool valid1 = bwd.x > FLOW_CONFIDENCE_MIN;\n"
    "    float flow0_length = length(sample_flow(flow0, p0, w, h));\n"
    "    float flow1_length = length(sample_flow(flow1, p1, w, h));\n"
    "#endif\n"
    "\n"
    "    InterlockedAdd(counters[0], 1);\n"
    "    if (valid0 && valid1)\n"
    "        InterlockedAdd(counters[1], 1);\n"
    "    else if (valid0)\n"
    "        InterlockedAdd(counters[2], 1);\n"
    "    else if (valid1)\n"
    "        InterlockedAdd(counters[3], 1);\n"
    "    else\n"
    "        InterlockedAdd(counters[4], 1);\n"
    "    if (fwd.y > 0.5) InterlockedAdd(counters[5], 1);\n"
    "    if (bwd.y > 0.5) InterlockedAdd(counters[6], 1);\n"
    "    if (fwd.y < 0.5 && fwd.z >= 0.95 * (1.5 + 0.05 * flow0_length))\n"
    "        InterlockedAdd(counters[7], 1);\n"
    "    if (bwd.y < 0.5 && bwd.z >= 0.95 * (1.5 + 0.05 * flow1_length))\n"
    "        InterlockedAdd(counters[8], 1);\n"
    "    if (fwd.w >= 0.95) InterlockedAdd(counters[9], 1);\n"
    "    if (bwd.w >= 0.95) InterlockedAdd(counters[10], 1);\n"
    "    InterlockedAdd(counters[11], (uint)round(fwd.w * 255.0));\n"
    "    InterlockedAdd(counters[12], (uint)round(bwd.w * 255.0));\n"
    "\n"
    "    uint luma_diff = (uint)round(abs(\n"
    "        gray0.Load(int3(id.xy, 0)) - gray1.Load(int3(id.xy, 0))) * 255.0);\n"
    "    InterlockedAdd(counters[13], luma_diff);\n"
    "    if (luma_diff >= 32) InterlockedAdd(counters[14], 1);\n"
    "    float flow_length = max(flow0_length, flow1_length);\n"
    "    if (flow_length >= 64.0) InterlockedAdd(counters[15], 1);\n"
    "    if (fwd.y < 0.5) {\n"
    "        if (fwd.z <= 3.0) InterlockedAdd(counters[16], 1);\n"
    "        if (fwd.z <= 6.0) InterlockedAdd(counters[17], 1);\n"
    "        if (fwd.z <= 12.0) InterlockedAdd(counters[18], 1);\n"
    "        InterlockedAdd(counters[22], (uint)round(min(fwd.z, 255.0)));\n"
    "    }\n"
    "    if (bwd.y < 0.5) {\n"
    "        if (bwd.z <= 3.0) InterlockedAdd(counters[19], 1);\n"
    "        if (bwd.z <= 6.0) InterlockedAdd(counters[20], 1);\n"
    "        if (bwd.z <= 12.0) InterlockedAdd(counters[21], 1);\n"
    "        InterlockedAdd(counters[23], (uint)round(min(bwd.z, 255.0)));\n"
    "    }\n"
    "#if USE_FILLED_FLOW\n"
    "    if (final0.z <= FLOW_CONFIDENCE_MIN)\n"
    "        InterlockedAdd(counters[26], 1);\n"
    "    else if (final0.w > 0.5)\n"
    "        InterlockedAdd(counters[25], 1);\n"
    "    else\n"
    "        InterlockedAdd(counters[24], 1);\n"
    "    if (final1.z <= FLOW_CONFIDENCE_MIN)\n"
    "        InterlockedAdd(counters[29], 1);\n"
    "    else if (final1.w > 0.5)\n"
    "        InterlockedAdd(counters[28], 1);\n"
    "    else\n"
    "        InterlockedAdd(counters[27], 1);\n"
    "    if (final1.z <= FLOW_CONFIDENCE_MIN &&\n"
    "        final1.w < -1.5 && final1.w > -2.5)\n"
    "        InterlockedAdd(counters[30], 1);\n"
    "    if ((final0.z <= FLOW_CONFIDENCE_MIN &&\n"
    "         final0.w < -2.5 && final0.w > -3.5) ||\n"
    "        (final1.z <= FLOW_CONFIDENCE_MIN &&\n"
    "         final1.w < -2.5 && final1.w > -3.5))\n"
    "        InterlockedAdd(counters[31], 1);\n"
    "    if ((final0.z <= FLOW_CONFIDENCE_MIN &&\n"
    "         final0.w < -0.5 && final0.w > -1.5) ||\n"
    "        (final1.z <= FLOW_CONFIDENCE_MIN &&\n"
    "         final1.w < -0.5 && final1.w > -1.5))\n"
    "        InterlockedAdd(counters[32], 1);\n"
    "    if ((final0.z <= FLOW_CONFIDENCE_MIN && final0.w < -3.5) ||\n"
    "        (final1.z <= FLOW_CONFIDENCE_MIN && final1.w < -3.5))\n"
    "        InterlockedAdd(counters[33], 1);\n"
    "#endif\n"
    "    }\n"
    "    GroupMemoryBarrierWithGroupSync();\n"
    "    if (group_index < 34)\n"
    "        InterlockedAdd(output_counters[group_index],\n"
    "                       counters[group_index]);\n"
    "}\n";

static const char prepare_flow_shader_source[] =
    "Texture2D<int2> flow0 : register(t0);\n"
    "Texture2D<uint> cost0 : register(t1);\n"
    "Texture2D<int2> flow1 : register(t2);\n"
    "Texture2D<uint> cost1 : register(t3);\n"
    "Texture2D<float> gray0 : register(t4);\n"
    "Texture2D<float> gray1 : register(t5);\n"
    "RWTexture2D<float4> state0 : register(u0);\n"
    "RWTexture2D<float4> state1 : register(u1);\n"
    "\n"
    "float2 clamp_coord(float2 p, uint w, uint h)\n"
    "{\n"
    "    return clamp(p, float2(0.0, 0.0),\n"
    "                  float2((float)w - 1.0, (float)h - 1.0));\n"
    "}\n"
    "\n"
    "bool in_frame(float2 p, uint w, uint h)\n"
    "{\n"
    "    return p.x >= 0.0 && p.y >= 0.0 && p.x <= (float)w - 1.0 &&\n"
    "           p.y <= (float)h - 1.0;\n"
    "}\n"
    "\n"
    "float2 sample_flow(Texture2D<int2> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp_coord(p, w, h);\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float2 v00 = (float2)tex.Load(int3(a, 0));\n"
    "    float2 v10 = (float2)tex.Load(int3(int2(b.x, a.y), 0));\n"
    "    float2 v01 = (float2)tex.Load(int3(int2(a.x, b.y), 0));\n"
    "    float2 v11 = (float2)tex.Load(int3(b, 0));\n"
    "    return lerp(lerp(v00, v10, f.x),\n"
    "                lerp(v01, v11, f.x), f.y) / 32.0;\n"
    "}\n"
    "\n"
    "float sample_cost(Texture2D<uint> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp_coord(p, w, h);\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float v00 = (float)(tex.Load(int3(a, 0)) & 255);\n"
    "    float v10 = (float)(tex.Load(int3(int2(b.x, a.y), 0)) & 255);\n"
    "    float v01 = (float)(tex.Load(int3(int2(a.x, b.y), 0)) & 255);\n"
    "    float v11 = (float)(tex.Load(int3(b, 0)) & 255);\n"
    "    return lerp(lerp(v00, v10, f.x),\n"
    "                lerp(v01, v11, f.x), f.y) / 255.0;\n"
    "}\n"
    "\n"
    "float sample_gray(Texture2D<float> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp_coord(p, w, h);\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float v00 = tex.Load(int3(a, 0));\n"
    "    float v10 = tex.Load(int3(int2(b.x, a.y), 0));\n"
    "    float v01 = tex.Load(int3(int2(a.x, b.y), 0));\n"
    "    float v11 = tex.Load(int3(b, 0));\n"
    "    return lerp(lerp(v00, v10, f.x),\n"
    "                lerp(v01, v11, f.x), f.y);\n"
    "}\n"
    "\n"
    "float photometric_confidence(float luma_delta)\n"
    "{\n"
    "    return saturate(1.0 - luma_delta /\n"
    "                    max(INFILL_LUMA_THRESHOLD, 0.001));\n"
    "}\n"
    "\n"
    "float4 prepare_forward(float2 p, uint w, uint h)\n"
    "{\n"
    "    float2 f = sample_flow(flow0, p, w, h);\n"
    "    float2 q = p + f;\n"
    "    if (!in_frame(q, w, h))\n"
    "        return float4(f, 0.0, -1.0);\n"
    "    float2 b = sample_flow(flow1, q, w, h);\n"
    "    float residual = length(f + b);\n"
    "    float threshold = FLOW_FB_ABS + FLOW_FB_REL * length(f);\n"
    "    if (residual >= threshold)\n"
    "        return float4(f, 0.0, -2.0);\n"
    "    float cost = max(sample_cost(cost0, p, w, h),\n"
    "                     sample_cost(cost1, q, w, h));\n"
    "    if (cost > FLOW_COST_MAX)\n"
    "        return float4(f, 0.0, -4.0);\n"
    "    float luma_delta = abs(sample_gray(gray0, p, w, h) -\n"
    "                           sample_gray(gray1, q, w, h)) * 255.0;\n"
    "    if (luma_delta >= INFILL_LUMA_THRESHOLD)\n"
    "        return float4(f, 0.0, -3.0);\n"
    "    float consistency = saturate(1.0 - residual / threshold);\n"
    "    float confidence = consistency * (1.0 - cost) *\n"
    "                       photometric_confidence(luma_delta);\n"
    "    return float4(f, confidence, 0.0);\n"
    "}\n"
    "\n"
    "float4 prepare_backward(float2 q, uint w, uint h)\n"
    "{\n"
    "    float2 b = sample_flow(flow1, q, w, h);\n"
    "    float2 p = q + b;\n"
    "    if (!in_frame(p, w, h))\n"
    "        return float4(b, 0.0, -1.0);\n"
    "    float2 f = sample_flow(flow0, p, w, h);\n"
    "    float residual = length(b + f);\n"
    "    float threshold = FLOW_FB_ABS + FLOW_FB_REL * length(b);\n"
    "    if (residual >= threshold)\n"
    "        return float4(b, 0.0, -2.0);\n"
    "    float cost = max(sample_cost(cost1, q, w, h),\n"
    "                     sample_cost(cost0, p, w, h));\n"
    "    if (cost > FLOW_COST_MAX)\n"
    "        return float4(b, 0.0, -4.0);\n"
    "    float luma_delta = abs(sample_gray(gray0, p, w, h) -\n"
    "                           sample_gray(gray1, q, w, h)) * 255.0;\n"
    "    if (luma_delta >= INFILL_LUMA_THRESHOLD)\n"
    "        return float4(b, 0.0, -3.0);\n"
    "    float consistency = saturate(1.0 - residual / threshold);\n"
    "    float confidence = consistency * (1.0 - cost) *\n"
    "                       photometric_confidence(luma_delta);\n"
    "    return float4(b, confidence, 0.0);\n"
    "}\n"
    "\n"
    "[numthreads(16, 16, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID)\n"
    "{\n"
    "    uint w, h;\n"
    "    state0.GetDimensions(w, h);\n"
    "    if (id.x >= w || id.y >= h)\n"
    "        return;\n"
    "    float2 p = (float2)id.xy;\n"
    "    state0[id.xy] = prepare_forward(p, w, h);\n"
    "    state1[id.xy] = prepare_backward(p, w, h);\n"
    "}\n";

static const char infill_flow_shader_source[] =
    "Texture2D<float4> source0 : register(t0);\n"
    "Texture2D<float4> source1 : register(t1);\n"
    "Texture2D<float> gray0 : register(t2);\n"
    "Texture2D<float> gray1 : register(t3);\n"
    "RWTexture2D<float4> output0 : register(u0);\n"
    "RWTexture2D<float4> output1 : register(u1);\n"
    "\n"
    "float4 fill_state(Texture2D<float4> source, Texture2D<float> gray,\n"
    "                  int2 p, uint w, uint h)\n"
    "{\n"
    "    float4 base = source.Load(int3(p, 0));\n"
    "    if (base.z > FLOW_CONFIDENCE_MIN)\n"
    "        return base;\n"
    "    float center = gray.Load(int3(p, 0));\n"
    "    float4 best = base;\n"
    "    float best_score = 0.0;\n"
    "    [unroll]\n"
    "    for (int step = 0; step < 3; step++) {\n"
    "        int radius = 1 << step;\n"
    "        [unroll]\n"
    "        for (int oy = -1; oy <= 1; oy++) {\n"
    "            [unroll]\n"
    "            for (int ox = -1; ox <= 1; ox++) {\n"
    "                if (ox == 0 && oy == 0)\n"
    "                    continue;\n"
    "                int2 q = p + int2(ox, oy) * radius;\n"
    "                if (q.x < 0 || q.y < 0 || q.x >= (int)w || q.y >= (int)h)\n"
    "                    continue;\n"
    "                float4 candidate = source.Load(int3(q, 0));\n"
    "                if (candidate.z <= FLOW_CONFIDENCE_MIN)\n"
    "                    continue;\n"
    "                float luma_delta = abs(center - gray.Load(int3(q, 0))) * 255.0;\n"
    "                if (luma_delta > INFILL_LUMA_THRESHOLD)\n"
    "                    continue;\n"
    "                float axial = abs(ox) + abs(oy) == 1 ? 1.0 : 0.85;\n"
    "                float score = candidate.z * axial *\n"
    "                              exp2(-0.25 * luma_delta) / radius;\n"
    "                if (score > best_score) {\n"
    "                    best_score = score;\n"
    "                    best = candidate;\n"
    "                }\n"
    "            }\n"
    "        }\n"
    "    }\n"
    "    if (best_score > 0.0)\n"
    "        return float4(best.xy, max(best.z * 0.9,\n"
    "                                      FLOW_CONFIDENCE_MIN + 0.001), 1.0);\n"
    "    return base;\n"
    "}\n"
    "\n"
    "[numthreads(16, 16, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID)\n"
    "{\n"
    "    uint w, h;\n"
    "    output0.GetDimensions(w, h);\n"
    "    if (id.x >= w || id.y >= h)\n"
    "        return;\n"
    "    output0[id.xy] = fill_state(source0, gray0, (int2)id.xy, w, h);\n"
    "    output1[id.xy] = fill_state(source1, gray1, (int2)id.xy, w, h);\n"
    "}\n";

static const char robust_occupancy_shader_source[] =
    "Texture2D<int2> flow0 : register(t0);\n"
    "Texture2D<int2> flow1 : register(t1);\n"
    "Texture2D<uint> cost0 : register(t2);\n"
    "Texture2D<uint> cost1 : register(t3);\n"
    "Texture2D<float> y0 : register(t4);\n"
    "Texture2D<float> y1 : register(t5);\n"
    "RWTexture2D<uint> occupancy0 : register(u0);\n"
    "RWTexture2D<uint> occupancy1 : register(u1);\n"
    "RWTexture2D<uint> projected_owner0 : register(u2);\n"
    "RWTexture2D<uint> projected_owner1 : register(u3);\n"
    "\n"
    "bool in_frame(float2 p, uint w, uint h)\n"
    "{\n"
    "    return p.x >= 0.0 && p.y >= 0.0 &&\n"
    "           p.x <= (float)w - 1.0 && p.y <= (float)h - 1.0;\n"
    "}\n"
    "\n"
    "float sample_y(Texture2D<float> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp(p, float2(0.0, 0.0),\n"
    "              float2((float)w - 1.0, (float)h - 1.0));\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float v00 = tex.Load(int3(a, 0));\n"
    "    float v10 = tex.Load(int3(int2(b.x, a.y), 0));\n"
    "    float v01 = tex.Load(int3(int2(a.x, b.y), 0));\n"
    "    float v11 = tex.Load(int3(b, 0));\n"
    "    return lerp(lerp(v00, v10, f.x), lerp(v01, v11, f.x), f.y);\n"
    "}\n"
    "\n"
    "float projected_quality(Texture2D<int2> reverse_flow,\n"
    "                        Texture2D<uint> primary_cost,\n"
    "                        Texture2D<uint> reverse_cost,\n"
    "                        Texture2D<float> primary_y,\n"
    "                        Texture2D<float> reverse_y,\n"
    "                        int2 source, float2 motion, uint w, uint h)\n"
    "{\n"
    "    float2 endpoint = (float2)source + motion;\n"
    "    if (!in_frame(endpoint, w, h))\n"
    "        return 0.0;\n"
    "    int2 reverse_source = clamp((int2)round(endpoint), int2(0, 0),\n"
    "        int2((int)w - 1, (int)h - 1));\n"
    "    float2 reverse = (float2)reverse_flow.Load(\n"
    "        int3(reverse_source, 0)) / 32.0;\n"
    "    float threshold = 2.0 + 0.06 * length(motion);\n"
    "    float consistency = saturate(\n"
    "        1.0 - length(motion + reverse) / threshold);\n"
    "    float cost = max(\n"
    "        (float)(primary_cost.Load(int3(source, 0)) & 255),\n"
    "        (float)(reverse_cost.Load(int3(reverse_source, 0)) & 255)) /\n"
    "        255.0;\n"
    "    float cost_confidence = 1.0 - cost;\n"
    "    float photo_delta = abs(primary_y.Load(int3(source, 0)) -\n"
    "        sample_y(reverse_y, endpoint, w, h)) * 255.0;\n"
    "    float photo = saturate(1.0 - photo_delta / 24.0);\n"
    "    return cost_confidence * cost_confidence *\n"
    "        (0.60 * consistency * consistency + 0.40 * photo * photo);\n"
    "}\n"
    "\n"
    "uint pack_projected_owner(int2 source, float quality)\n"
    "{\n"
    "    uint quality_code = max(1u,\n"
    "        (uint)round(saturate(quality) * 255.0));\n"
    "    return (quality_code << 24) |\n"
    "           (((uint)source.y & 4095) << 12) |\n"
    "           ((uint)source.x & 4095);\n"
    "}\n"
    "\n"
    "[numthreads(16, 16, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID)\n"
    "{\n"
    "    uint w, h;\n"
    "    occupancy0.GetDimensions(w, h);\n"
    "    if (id.x >= w || id.y >= h)\n"
    "        return;\n"
    "    float2 source = (float2)id.xy;\n"
    "    float2 forward = (float2)flow0.Load(int3(id.xy, 0)) / 32.0;\n"
    "    float2 midpoint0 = source + 0.5 * forward;\n"
    "    if (in_frame(midpoint0, w, h)) {\n"
    "        int2 target0 = (int2)round(midpoint0);\n"
    "        InterlockedAdd(occupancy0[target0], 1);\n"
    "        float quality0 = projected_quality(\n"
    "            flow1, cost0, cost1, y0, y1, (int2)id.xy,\n"
    "            forward, w, h);\n"
    "        InterlockedMax(projected_owner0[target0],\n"
    "            pack_projected_owner((int2)id.xy, quality0));\n"
    "    }\n"
    "    float2 backward = (float2)flow1.Load(int3(id.xy, 0)) / 32.0;\n"
    "    float2 midpoint1 = source + 0.5 * backward;\n"
    "    if (in_frame(midpoint1, w, h)) {\n"
    "        int2 target1 = (int2)round(midpoint1);\n"
    "        InterlockedAdd(occupancy1[target1], 1);\n"
    "        float quality1 = projected_quality(\n"
    "            flow0, cost1, cost0, y1, y0, (int2)id.xy,\n"
    "            backward, w, h);\n"
    "        InterlockedMax(projected_owner1[target1],\n"
    "            pack_projected_owner((int2)id.xy, quality1));\n"
    "    }\n"
    "}\n";

static const char synthesize_p010_shader_source[] =
    "Texture2D<float> y0 : register(t0);\n"
    "Texture2D<float> y1 : register(t1);\n"
    "Texture2D<float2> uv0 : register(t2);\n"
    "Texture2D<float2> uv1 : register(t3);\n"
    "#if USE_FILLED_FLOW\n"
    "Texture2D<float4> flow_state0 : register(t4);\n"
    "Texture2D<float4> flow_state1 : register(t5);\n"
    "#else\n"
    "Texture2D<int2> flow0 : register(t4);\n"
    "Texture2D<int2> flow1 : register(t5);\n"
    "Texture2D<uint> cost0 : register(t6);\n"
    "Texture2D<uint> cost1 : register(t7);\n"
    "#endif\n"
    "#if USE_ROBUST_SCENE\n"
    "StructuredBuffer<uint> robust_scene_state : register(t8);\n"
    "Texture2D<uint> occupancy0 : register(t9);\n"
    "Texture2D<uint> occupancy1 : register(t10);\n"
    "Texture2D<int2> previous_flow_backward : register(t11);\n"
    "Texture2D<int2> next_flow_forward : register(t12);\n"
    "StructuredBuffer<uint> previous_scene_state : register(t13);\n"
    "StructuredBuffer<uint> next_scene_state : register(t14);\n"
    "Texture2D<uint> projected_owner0 : register(t15);\n"
    "Texture2D<uint> projected_owner1 : register(t16);\n"
    "cbuffer RobustTemporalContext : register(b0)\n"
    "{\n"
    "    uint temporal_previous_available;\n"
    "    uint temporal_next_available;\n"
    "    uint temporal_reserved0;\n"
    "    uint temporal_reserved1;\n"
    "};\n"
    "#else\n"
    "StructuredBuffer<uint> scene_counters : register(t8);\n"
    "#endif\n"
    "RWTexture2D<float> out_y : register(u0);\n"
    "RWTexture2D<float2> out_uv : register(u1);\n"
    "#if USE_ROBUST_SCENE\n"
    "RWStructuredBuffer<uint> robust_synthesis_summary : register(u2);\n"
    "groupshared uint robust_counters[ROBUST_SYNTHESIS_COUNTER_COUNT];\n"
    "#else\n"
    "RWStructuredBuffer<uint> scene_summary : register(u2);\n"
    "#endif\n"
    "\n"
    "float2 clamp_luma(float2 p, uint w, uint h)\n"
    "{\n"
    "    return clamp(p, float2(0.0, 0.0),\n"
    "                  float2((float)w - 1.0, (float)h - 1.0));\n"
    "}\n"
    "\n"
    "float sample_y(Texture2D<float> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp_luma(p, w, h);\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float v00 = tex.Load(int3(a, 0));\n"
    "    float v10 = tex.Load(int3(int2(b.x, a.y), 0));\n"
    "    float v01 = tex.Load(int3(int2(a.x, b.y), 0));\n"
    "    float v11 = tex.Load(int3(b, 0));\n"
    "    return lerp(lerp(v00, v10, f.x), lerp(v01, v11, f.x), f.y);\n"
    "}\n"
    "\n"
    "float2 sample_uv(Texture2D<float2> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp(p, float2(0.0, 0.0),\n"
    "               float2((float)w - 1.0, (float)h - 1.0));\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float2 v00 = tex.Load(int3(a, 0));\n"
    "    float2 v10 = tex.Load(int3(int2(b.x, a.y), 0));\n"
    "    float2 v01 = tex.Load(int3(int2(a.x, b.y), 0));\n"
    "    float2 v11 = tex.Load(int3(b, 0));\n"
    "    return lerp(lerp(v00, v10, f.x), lerp(v01, v11, f.x), f.y);\n"
    "}\n"
    "\n"
    "float2 sample_raw_flow(Texture2D<int2> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp_luma(p, w, h);\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float2 v00 = (float2)tex.Load(int3(a, 0));\n"
    "    float2 v10 = (float2)tex.Load(int3(int2(b.x, a.y), 0));\n"
    "    float2 v01 = (float2)tex.Load(int3(int2(a.x, b.y), 0));\n"
    "    float2 v11 = (float2)tex.Load(int3(b, 0));\n"
    "    return lerp(lerp(v00, v10, f.x), lerp(v01, v11, f.x), f.y) / 32.0;\n"
    "}\n"
    "\n"
    "float sample_cost(Texture2D<uint> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp_luma(p, w, h);\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float v00 = (float)(tex.Load(int3(a, 0)) & 255);\n"
    "    float v10 = (float)(tex.Load(int3(int2(b.x, a.y), 0)) & 255);\n"
    "    float v01 = (float)(tex.Load(int3(int2(a.x, b.y), 0)) & 255);\n"
    "    float v11 = (float)(tex.Load(int3(b, 0)) & 255);\n"
    "    return lerp(lerp(v00, v10, f.x), lerp(v01, v11, f.x), f.y) / 255.0;\n"
    "}\n"
    "\n"
    "#if USE_FILLED_FLOW\n"
    "float4 sample_flow_state(Texture2D<float4> tex, float2 p, uint w, uint h)\n"
    "{\n"
    "    p = clamp_luma(p, w, h);\n"
    "    int2 a = (int2)floor(p);\n"
    "    int2 b = min(a + 1, int2((int)w - 1, (int)h - 1));\n"
    "    float2 f = p - a;\n"
    "    float4 v00 = tex.Load(int3(a, 0));\n"
    "    float4 v10 = tex.Load(int3(int2(b.x, a.y), 0));\n"
    "    float4 v01 = tex.Load(int3(int2(a.x, b.y), 0));\n"
    "    float4 v11 = tex.Load(int3(b, 0));\n"
    "    return lerp(lerp(v00, v10, f.x), lerp(v01, v11, f.x), f.y);\n"
    "}\n"
    "#endif\n"
    "\n"
    "float2 active_flow0(float2 p, uint w, uint h)\n"
    "{\n"
    "#if USE_FILLED_FLOW\n"
    "    return sample_flow_state(flow_state0, p, w, h).xy;\n"
    "#else\n"
    "    return sample_raw_flow(flow0, p, w, h);\n"
    "#endif\n"
    "}\n"
    "\n"
    "float2 active_flow1(float2 p, uint w, uint h)\n"
    "{\n"
    "#if USE_FILLED_FLOW\n"
    "    return sample_flow_state(flow_state1, p, w, h).xy;\n"
    "#else\n"
    "    return sample_raw_flow(flow1, p, w, h);\n"
    "#endif\n"
    "}\n"
    "\n"
    "bool in_luma(float2 p, uint w, uint h)\n"
    "{\n"
    "    return p.x >= 0.0 && p.y >= 0.0 && p.x <= (float)w - 1.0 &&\n"
    "           p.y <= (float)h - 1.0;\n"
    "}\n"
    "\n"
    "float confidence_forward(float2 p, uint w, uint h)\n"
    "{\n"
    "#if USE_FILLED_FLOW\n"
    "    return sample_flow_state(flow_state0, p, w, h).z;\n"
    "#else\n"
    "    float2 f = active_flow0(p, w, h);\n"
    "    float2 q = p + f;\n"
    "    if (!in_luma(q, w, h))\n"
    "        return 0.0;\n"
    "    float2 b = active_flow1(q, w, h);\n"
    "    float residual = length(f + b);\n"
    "    float threshold = 1.5 + 0.05 * length(f);\n"
    "    float consistency = saturate(1.0 - residual / threshold);\n"
    "    return consistency * (1.0 - sample_cost(cost0, p, w, h));\n"
    "#endif\n"
    "}\n"
    "\n"
    "float confidence_backward(float2 p, uint w, uint h)\n"
    "{\n"
    "#if USE_FILLED_FLOW\n"
    "    return sample_flow_state(flow_state1, p, w, h).z;\n"
    "#else\n"
    "    float2 b = active_flow1(p, w, h);\n"
    "    float2 q = p + b;\n"
    "    if (!in_luma(q, w, h))\n"
    "        return 0.0;\n"
    "    float2 f = active_flow0(q, w, h);\n"
    "    float residual = length(b + f);\n"
    "    float threshold = 1.5 + 0.05 * length(b);\n"
    "    float consistency = saturate(1.0 - residual / threshold);\n"
    "    return consistency * (1.0 - sample_cost(cost1, p, w, h));\n"
    "#endif\n"
    "}\n"
    "\n"
    "void solve_sources(float2 target, uint w, uint h, out float2 p0,\n"
    "                   out float2 p1)\n"
    "{\n"
    "    p0 = target;\n"
    "    p1 = target;\n"
    "    [unroll]\n"
    "    for (uint n = 0; n < 2; n++) {\n"
    "        p0 = target - 0.5 * active_flow0(p0, w, h);\n"
    "        p1 = target - 0.5 * active_flow1(p1, w, h);\n"
    "    }\n"
    "}\n"
    "\n"
    "#if USE_ROBUST_SCENE\n"
    "void order_pair(inout float a, inout float b)\n"
    "{\n"
    "    if (a > b) {\n"
    "        float temporary = a;\n"
    "        a = b;\n"
    "        b = temporary;\n"
    "    }\n"
    "}\n"
    "\n"
    "float median5(float a, float b, float c, float d, float e)\n"
    "{\n"
    "    order_pair(a, b); order_pair(b, c); order_pair(c, d);\n"
    "    order_pair(d, e); order_pair(a, b); order_pair(b, c);\n"
    "    order_pair(c, d); order_pair(a, b); order_pair(b, c);\n"
    "    order_pair(a, b);\n"
    "    return c;\n"
    "}\n"
    "\n"
    "float2 median_flow(Texture2D<int2> flow, float2 p, uint w, uint h)\n"
    "{\n"
    "    float2 center = sample_raw_flow(flow, p, w, h);\n"
    "    float2 left = sample_raw_flow(flow, p + float2(-2.0, 0.0), w, h);\n"
    "    float2 right = sample_raw_flow(flow, p + float2(2.0, 0.0), w, h);\n"
    "    float2 top = sample_raw_flow(flow, p + float2(0.0, -2.0), w, h);\n"
    "    float2 bottom = sample_raw_flow(flow, p + float2(0.0, 2.0), w, h);\n"
    "    return float2(\n"
    "        median5(center.x, left.x, right.x, top.x, bottom.x),\n"
    "        median5(center.y, left.y, right.y, top.y, bottom.y));\n"
    "}\n"
    "\n"
    "float2 block_flow(Texture2D<int2> flow, float2 p, uint w, uint h)\n"
    "{\n"
    "    float2 value = sample_raw_flow(flow, p, w, h) * 2.0;\n"
    "    value += sample_raw_flow(flow, p + float2(-8.0, 0.0), w, h);\n"
    "    value += sample_raw_flow(flow, p + float2(8.0, 0.0), w, h);\n"
    "    value += sample_raw_flow(flow, p + float2(0.0, -8.0), w, h);\n"
    "    value += sample_raw_flow(flow, p + float2(0.0, 8.0), w, h);\n"
    "    return value / 6.0;\n"
    "}\n"
    "\n"
    "float edge_luma_weight(float center, float sample)\n"
    "{\n"
    "    float delta = abs(center - sample) * 255.0;\n"
    "    return 0.08 + 0.92 * exp2(-0.22 * delta);\n"
    "}\n"
    "\n"
    "float edge_vector_score(float2 candidate, float candidate_weight,\n"
    "                        float2 center, float2 left, float2 right,\n"
    "                        float2 top, float2 bottom,\n"
    "                        float center_weight, float left_weight,\n"
    "                        float right_weight, float top_weight,\n"
    "                        float bottom_weight)\n"
    "{\n"
    "    float total_weight = center_weight + left_weight + right_weight +\n"
    "                         top_weight + bottom_weight;\n"
    "    float distance_score =\n"
    "        center_weight * length(candidate - center) +\n"
    "        left_weight * length(candidate - left) +\n"
    "        right_weight * length(candidate - right) +\n"
    "        top_weight * length(candidate - top) +\n"
    "        bottom_weight * length(candidate - bottom);\n"
    "    float boundary_penalty = (1.0 - candidate_weight) *\n"
    "        (3.0 + 0.20 * length(candidate - center));\n"
    "    return distance_score / max(total_weight, 0.001) +\n"
    "           boundary_penalty;\n"
    "}\n"
    "\n"
    "float2 edge_vector_flow(Texture2D<int2> flow, Texture2D<float> luma,\n"
    "                        float2 p, uint w, uint h)\n"
    "{\n"
    "    float2 center_p = clamp_luma(p, w, h);\n"
    "    float2 left_p = clamp_luma(p + float2(-2.0, 0.0), w, h);\n"
    "    float2 right_p = clamp_luma(p + float2(2.0, 0.0), w, h);\n"
    "    float2 top_p = clamp_luma(p + float2(0.0, -2.0), w, h);\n"
    "    float2 bottom_p = clamp_luma(p + float2(0.0, 2.0), w, h);\n"
    "    float2 center = sample_raw_flow(flow, center_p, w, h);\n"
    "    float2 left = sample_raw_flow(flow, left_p, w, h);\n"
    "    float2 right = sample_raw_flow(flow, right_p, w, h);\n"
    "    float2 top = sample_raw_flow(flow, top_p, w, h);\n"
    "    float2 bottom = sample_raw_flow(flow, bottom_p, w, h);\n"
    "    float center_luma = sample_y(luma, center_p, w, h);\n"
    "    float center_weight = 1.0;\n"
    "    float left_weight = edge_luma_weight(\n"
    "        center_luma, sample_y(luma, left_p, w, h));\n"
    "    float right_weight = edge_luma_weight(\n"
    "        center_luma, sample_y(luma, right_p, w, h));\n"
    "    float top_weight = edge_luma_weight(\n"
    "        center_luma, sample_y(luma, top_p, w, h));\n"
    "    float bottom_weight = edge_luma_weight(\n"
    "        center_luma, sample_y(luma, bottom_p, w, h));\n"
    "    float2 best = center;\n"
    "    float best_score = edge_vector_score(\n"
    "        center, center_weight, center, left, right, top, bottom,\n"
    "        center_weight, left_weight, right_weight, top_weight,\n"
    "        bottom_weight);\n"
    "    float candidate_score = edge_vector_score(\n"
    "        left, left_weight, center, left, right, top, bottom,\n"
    "        center_weight, left_weight, right_weight, top_weight,\n"
    "        bottom_weight);\n"
    "    if (candidate_score < best_score) {\n"
    "        best = left;\n"
    "        best_score = candidate_score;\n"
    "    }\n"
    "    candidate_score = edge_vector_score(\n"
    "        right, right_weight, center, left, right, top, bottom,\n"
    "        center_weight, left_weight, right_weight, top_weight,\n"
    "        bottom_weight);\n"
    "    if (candidate_score < best_score) {\n"
    "        best = right;\n"
    "        best_score = candidate_score;\n"
    "    }\n"
    "    candidate_score = edge_vector_score(\n"
    "        top, top_weight, center, left, right, top, bottom,\n"
    "        center_weight, left_weight, right_weight, top_weight,\n"
    "        bottom_weight);\n"
    "    if (candidate_score < best_score) {\n"
    "        best = top;\n"
    "        best_score = candidate_score;\n"
    "    }\n"
    "    candidate_score = edge_vector_score(\n"
    "        bottom, bottom_weight, center, left, right, top, bottom,\n"
    "        center_weight, left_weight, right_weight, top_weight,\n"
    "        bottom_weight);\n"
    "    if (candidate_score < best_score)\n"
    "        best = bottom;\n"
    "    return best;\n"
    "}\n"
    "\n"
    "float2 candidate_flow0(float2 p, uint candidate, uint w, uint h)\n"
    "{\n"
    "    return candidate == 0 ? sample_raw_flow(flow0, p, w, h) :\n"
    "           candidate == 1 ? median_flow(flow0, p, w, h) :\n"
    "           candidate == 2 ? block_flow(flow0, p, w, h) :\n"
    "                            edge_vector_flow(flow0, y0, p, w, h);\n"
    "}\n"
    "\n"
    "float2 candidate_flow1(float2 p, uint candidate, uint w, uint h)\n"
    "{\n"
    "    return candidate == 0 ? sample_raw_flow(flow1, p, w, h) :\n"
    "           candidate == 1 ? median_flow(flow1, p, w, h) :\n"
    "           candidate == 2 ? block_flow(flow1, p, w, h) :\n"
    "                            edge_vector_flow(flow1, y1, p, w, h);\n"
    "}\n"
    "\n"
    "bool temporal_scene_is_normal(StructuredBuffer<uint> state)\n"
    "{\n"
    "    return state[ROBUST_STATE_CLASS] == ROBUST_SCENE_NORMAL;\n"
    "}\n"
    "\n"
    "float temporal_vector_support(float2 adjacent, float2 central)\n"
    "{\n"
    "    float threshold = 3.0 + 0.30 * length(central);\n"
    "    return saturate(1.0 - length(adjacent - central) / threshold);\n"
    "}\n"
    "\n"
    "float2 temporal_midpoint_from_frame0(float2 source, uint w, uint h,\n"
    "                                     out float support)\n"
    "{\n"
    "    float2 central = sample_raw_flow(flow0, source, w, h);\n"
    "    float2 endpoint = source + central;\n"
    "    float2 tangent0 = central;\n"
    "    float2 tangent1 = central;\n"
    "    float support0 = 0.0;\n"
    "    float support1 = 0.0;\n"
    "    if (temporal_previous_available != 0 &&\n"
    "        temporal_scene_is_normal(previous_scene_state)) {\n"
    "        float2 incoming = -sample_raw_flow(\n"
    "            previous_flow_backward, source, w, h);\n"
    "        support0 = temporal_vector_support(incoming, central);\n"
    "        tangent0 = lerp(central, incoming, support0);\n"
    "    }\n"
    "    if (temporal_next_available != 0 &&\n"
    "        temporal_scene_is_normal(next_scene_state)) {\n"
    "        float2 outgoing = sample_raw_flow(\n"
    "            next_flow_forward, endpoint, w, h);\n"
    "        support1 = temporal_vector_support(outgoing, central);\n"
    "        tangent1 = lerp(central, outgoing, support1);\n"
    "    }\n"
    "    support = max(support0, support1);\n"
    "    return source + 0.5 * central + 0.125 * (tangent0 - tangent1);\n"
    "}\n"
    "\n"
    "float2 temporal_midpoint_from_frame1(float2 source, uint w, uint h,\n"
    "                                     out float support)\n"
    "{\n"
    "    float2 backward = sample_raw_flow(flow1, source, w, h);\n"
    "    float2 start = source + backward;\n"
    "    float2 central = -backward;\n"
    "    float2 tangent0 = central;\n"
    "    float2 tangent1 = central;\n"
    "    float support0 = 0.0;\n"
    "    float support1 = 0.0;\n"
    "    if (temporal_previous_available != 0 &&\n"
    "        temporal_scene_is_normal(previous_scene_state)) {\n"
    "        float2 incoming = -sample_raw_flow(\n"
    "            previous_flow_backward, start, w, h);\n"
    "        support0 = temporal_vector_support(incoming, central);\n"
    "        tangent0 = lerp(central, incoming, support0);\n"
    "    }\n"
    "    if (temporal_next_available != 0 &&\n"
    "        temporal_scene_is_normal(next_scene_state)) {\n"
    "        float2 outgoing = sample_raw_flow(\n"
    "            next_flow_forward, source, w, h);\n"
    "        support1 = temporal_vector_support(outgoing, central);\n"
    "        tangent1 = lerp(central, outgoing, support1);\n"
    "    }\n"
    "    support = max(support0, support1);\n"
    "    return start + 0.5 * central + 0.125 * (tangent0 - tangent1);\n"
    "}\n"
    "\n"
    "float2 solve_temporal_candidate0(float2 target, uint w, uint h,\n"
    "                                 out float support)\n"
    "{\n"
    "    float2 source = target - 0.5 *\n"
    "        sample_raw_flow(flow0, target, w, h);\n"
    "    float midpoint_support = 0.0;\n"
    "    float2 midpoint = temporal_midpoint_from_frame0(\n"
    "        source, w, h, midpoint_support);\n"
    "    source += target - midpoint;\n"
    "    midpoint = temporal_midpoint_from_frame0(\n"
    "        source, w, h, midpoint_support);\n"
    "    source += target - midpoint;\n"
    "    support = midpoint_support;\n"
    "    return source;\n"
    "}\n"
    "\n"
    "float2 solve_temporal_candidate1(float2 target, uint w, uint h,\n"
    "                                 out float support)\n"
    "{\n"
    "    float2 source = target - 0.5 *\n"
    "        sample_raw_flow(flow1, target, w, h);\n"
    "    float midpoint_support = 0.0;\n"
    "    float2 midpoint = temporal_midpoint_from_frame1(\n"
    "        source, w, h, midpoint_support);\n"
    "    source += target - midpoint;\n"
    "    midpoint = temporal_midpoint_from_frame1(\n"
    "        source, w, h, midpoint_support);\n"
    "    source += target - midpoint;\n"
    "    support = midpoint_support;\n"
    "    return source;\n"
    "}\n"
    "\n"
    "float2 solve_candidate0(float2 target, uint candidate, uint w, uint h)\n"
    "{\n"
    "    float2 source = target - 0.5 *\n"
    "        candidate_flow0(target, candidate, w, h);\n"
    "    if (candidate == 0)\n"
    "        source = target - 0.5 * candidate_flow0(source, candidate, w, h);\n"
    "    return source;\n"
    "}\n"
    "\n"
    "float2 solve_candidate1(float2 target, uint candidate, uint w, uint h)\n"
    "{\n"
    "    float2 source = target - 0.5 *\n"
    "        candidate_flow1(target, candidate, w, h);\n"
    "    if (candidate == 0)\n"
    "        source = target - 0.5 * candidate_flow1(source, candidate, w, h);\n"
    "    return source;\n"
    "}\n"
    "\n"
    "float2 unpack_projected_owner_source(uint packed)\n"
    "{\n"
    "    return float2((float)(packed & 4095),\n"
    "                  (float)((packed >> 12) & 4095));\n"
    "}\n"
    "\n"
    "struct ProjectedOwnerContext\n"
    "{\n"
    "    float2 source;\n"
    "    float2 motion;\n"
    "    float quality;\n"
    "    uint valid;\n"
    "};\n"
    "\n"
    "ProjectedOwnerContext load_projected_owner_context(\n"
    "    Texture2D<uint> owner_map, Texture2D<int2> owner_flow,\n"
    "    float2 target, uint count, uint w, uint h)\n"
    "{\n"
    "    ProjectedOwnerContext owner;\n"
    "    owner.source = target;\n"
    "    owner.motion = float2(0.0, 0.0);\n"
    "    owner.quality = 0.0;\n"
    "    owner.valid = 0;\n"
    "    if (count <= 1)\n"
    "        return owner;\n"
    "    int2 owner_target = clamp((int2)round(target), int2(0, 0),\n"
    "        int2((int)w - 1, (int)h - 1));\n"
    "    uint packed = owner_map.Load(int3(owner_target, 0));\n"
    "    if (packed == 0)\n"
    "        return owner;\n"
    "    owner.source = unpack_projected_owner_source(packed);\n"
    "    owner.motion = (float2)owner_flow.Load(\n"
    "        int3((int2)owner.source, 0)) / 32.0;\n"
    "    owner.quality = (float)((packed >> 24) & 255) / 255.0;\n"
    "    owner.valid = 1;\n"
    "    return owner;\n"
    "}\n"
    "\n"
    "float projected_layer_support(ProjectedOwnerContext owner,\n"
    "                              float2 source, float2 implied)\n"
    "{\n"
    "    if (owner.valid == 0)\n"
    "        return 0.0;\n"
    "    float spatial_threshold = 2.0 + 0.02 * length(implied);\n"
    "    float spatial_match = saturate(1.0 -\n"
    "        length(source - owner.source) / spatial_threshold);\n"
    "    float motion_threshold = 2.5 + 0.08 * length(implied);\n"
    "    float motion_match = saturate(1.0 -\n"
    "        length(implied - owner.motion) / motion_threshold);\n"
    "    float layer_match = max(spatial_match, 0.90 * motion_match);\n"
    "    return layer_match * layer_match *\n"
    "        (0.65 + 0.35 * owner.quality);\n"
    "}\n"
    "\n"
    "float projected_layer_support0(float2 target, float2 source,\n"
    "                               uint w, uint h)\n"
    "{\n"
    "    int2 owner_target = clamp((int2)round(target), int2(0, 0),\n"
    "        int2((int)w - 1, (int)h - 1));\n"
    "    uint count = occupancy0.Load(int3(owner_target, 0));\n"
    "    ProjectedOwnerContext owner = load_projected_owner_context(\n"
    "        projected_owner0, flow0, target, count, w, h);\n"
    "    float2 implied = 2.0 * (target - source);\n"
    "    return projected_layer_support(owner, source, implied);\n"
    "}\n"
    "\n"
    "float projected_layer_support1(float2 target, float2 source,\n"
    "                               uint w, uint h)\n"
    "{\n"
    "    int2 owner_target = clamp((int2)round(target), int2(0, 0),\n"
    "        int2((int)w - 1, (int)h - 1));\n"
    "    uint count = occupancy1.Load(int3(owner_target, 0));\n"
    "    ProjectedOwnerContext owner = load_projected_owner_context(\n"
    "        projected_owner1, flow1, target, count, w, h);\n"
    "    float2 implied = 2.0 * (target - source);\n"
    "    return projected_layer_support(owner, source, implied);\n"
    "}\n"
    "\n"
    "float occupancy_confidence(uint count, float layer_support)\n"
    "{\n"
    "    if (count == 0)\n"
    "        return 0.0;\n"
    "    if (count == 1)\n"
    "        return 1.0;\n"
    "    float count_penalty = 0.85 + 0.15 * rsqrt((float)count);\n"
    "    return (0.22 + 0.78 * layer_support) * count_penalty;\n"
    "}\n"
    "\n"
    "struct RobustCandidate\n"
    "{\n"
    "    float2 source;\n"
    "    float score;\n"
    "    uint id;\n"
    "};\n"
    "\n"
    "float robust_candidate_score0(float2 target, float2 source,\n"
    "                              uint occupancy_count,\n"
    "                              ProjectedOwnerContext owner,\n"
    "                              uint w, uint h)\n"
    "{\n"
    "    float2 implied = 2.0 * (target - source);\n"
    "    float2 correspondent = source + implied;\n"
    "    if (!in_luma(source, w, h) || !in_luma(correspondent, w, h))\n"
    "        return 0.0;\n"
    "    float2 raw = active_flow0(source, w, h);\n"
    "    float2 reverse = active_flow1(correspondent, w, h);\n"
    "    float model_threshold = 2.0 + 0.08 * length(implied);\n"
    "    float fb_threshold = 2.0 + 0.06 * length(implied);\n"
    "    float model = saturate(1.0 - length(implied - raw) /\n"
    "                           model_threshold);\n"
    "    float consistency = saturate(1.0 - length(implied + reverse) /\n"
    "                                 fb_threshold);\n"
    "    float cost = max(sample_cost(cost0, source, w, h),\n"
    "                     sample_cost(cost1, correspondent, w, h));\n"
    "    float photo_delta = abs(sample_y(y0, source, w, h) -\n"
    "                            sample_y(y1, correspondent, w, h)) * 255.0;\n"
    "    float photo = saturate(1.0 - photo_delta / 24.0);\n"
    "    float layer_support = 0.0;\n"
    "    if (occupancy_count > 1)\n"
    "        layer_support = projected_layer_support(\n"
    "            owner, source, implied);\n"
    "    float cost_confidence = 1.0 - cost;\n"
    "    float motion_quality = 0.30 * model * model +\n"
    "                           0.45 * consistency * consistency +\n"
    "                           0.25 * photo;\n"
    "    return cost_confidence * cost_confidence * motion_quality *\n"
    "           occupancy_confidence(occupancy_count, layer_support);\n"
    "}\n"
    "\n"
    "float robust_candidate_score1(float2 target, float2 source,\n"
    "                              uint occupancy_count,\n"
    "                              ProjectedOwnerContext owner,\n"
    "                              uint w, uint h)\n"
    "{\n"
    "    float2 implied = 2.0 * (target - source);\n"
    "    float2 correspondent = source + implied;\n"
    "    if (!in_luma(source, w, h) || !in_luma(correspondent, w, h))\n"
    "        return 0.0;\n"
    "    float2 raw = active_flow1(source, w, h);\n"
    "    float2 reverse = active_flow0(correspondent, w, h);\n"
    "    float model_threshold = 2.0 + 0.08 * length(implied);\n"
    "    float fb_threshold = 2.0 + 0.06 * length(implied);\n"
    "    float model = saturate(1.0 - length(implied - raw) /\n"
    "                           model_threshold);\n"
    "    float consistency = saturate(1.0 - length(implied + reverse) /\n"
    "                                 fb_threshold);\n"
    "    float cost = max(sample_cost(cost1, source, w, h),\n"
    "                     sample_cost(cost0, correspondent, w, h));\n"
    "    float photo_delta = abs(sample_y(y1, source, w, h) -\n"
    "                            sample_y(y0, correspondent, w, h)) * 255.0;\n"
    "    float photo = saturate(1.0 - photo_delta / 24.0);\n"
    "    float layer_support = 0.0;\n"
    "    if (occupancy_count > 1)\n"
    "        layer_support = projected_layer_support(\n"
    "            owner, source, implied);\n"
    "    float cost_confidence = 1.0 - cost;\n"
    "    float motion_quality = 0.30 * model * model +\n"
    "                           0.45 * consistency * consistency +\n"
    "                           0.25 * photo;\n"
    "    return cost_confidence * cost_confidence * motion_quality *\n"
    "           occupancy_confidence(occupancy_count, layer_support);\n"
    "}\n"
    "\n"
    "RobustCandidate evaluate_candidate0(float2 target, uint candidate,\n"
    "                                    uint occupancy_count,\n"
    "                                    ProjectedOwnerContext owner,\n"
    "                                    uint w, uint h)\n"
    "{\n"
    "    RobustCandidate result;\n"
    "    result.source = solve_candidate0(target, candidate, w, h);\n"
    "    result.score = robust_candidate_score0(\n"
    "        target, result.source, occupancy_count, owner, w, h);\n"
    "    result.id = candidate;\n"
    "    return result;\n"
    "}\n"
    "\n"
    "RobustCandidate evaluate_candidate1(float2 target, uint candidate,\n"
    "                                    uint occupancy_count,\n"
    "                                    ProjectedOwnerContext owner,\n"
    "                                    uint w, uint h)\n"
    "{\n"
    "    RobustCandidate result;\n"
    "    result.source = solve_candidate1(target, candidate, w, h);\n"
    "    result.score = robust_candidate_score1(\n"
    "        target, result.source, occupancy_count, owner, w, h);\n"
    "    result.id = candidate;\n"
    "    return result;\n"
    "}\n"
    "\n"
    "RobustCandidate evaluate_projected_owner_candidate0(\n"
    "    float2 target, uint occupancy_count,\n"
    "    ProjectedOwnerContext owner, uint w, uint h)\n"
    "{\n"
    "    RobustCandidate result;\n"
    "    result.source = owner.valid != 0 ? owner.source : target;\n"
    "    result.score = occupancy_count > 1 && owner.valid != 0\n"
    "        ? robust_candidate_score0(\n"
    "            target, result.source, occupancy_count, owner, w, h) : 0.0;\n"
    "    result.id = 5;\n"
    "    return result;\n"
    "}\n"
    "\n"
    "RobustCandidate evaluate_projected_owner_candidate1(\n"
    "    float2 target, uint occupancy_count,\n"
    "    ProjectedOwnerContext owner, uint w, uint h)\n"
    "{\n"
    "    RobustCandidate result;\n"
    "    result.source = owner.valid != 0 ? owner.source : target;\n"
    "    result.score = occupancy_count > 1 && owner.valid != 0\n"
    "        ? robust_candidate_score1(\n"
    "            target, result.source, occupancy_count, owner, w, h) : 0.0;\n"
    "    result.id = 5;\n"
    "    return result;\n"
    "}\n"
    "\n"
    "RobustCandidate evaluate_temporal_candidate0(float2 target,\n"
    "                                             uint occupancy_count,\n"
    "                                             ProjectedOwnerContext owner,\n"
    "                                             uint w, uint h)\n"
    "{\n"
    "    RobustCandidate result;\n"
    "    float support = 0.0;\n"
    "    result.source = solve_temporal_candidate0(\n"
    "        target, w, h, support);\n"
    "    float base_score = robust_candidate_score0(\n"
    "        target, result.source, occupancy_count, owner, w, h);\n"
    "    result.score = support >= 0.35\n"
    "        ? base_score * (0.70 + 0.30 * support) : 0.0;\n"
    "    result.id = 3;\n"
    "    return result;\n"
    "}\n"
    "\n"
    "RobustCandidate evaluate_temporal_candidate1(float2 target,\n"
    "                                             uint occupancy_count,\n"
    "                                             ProjectedOwnerContext owner,\n"
    "                                             uint w, uint h)\n"
    "{\n"
    "    RobustCandidate result;\n"
    "    float support = 0.0;\n"
    "    result.source = solve_temporal_candidate1(\n"
    "        target, w, h, support);\n"
    "    float base_score = robust_candidate_score1(\n"
    "        target, result.source, occupancy_count, owner, w, h);\n"
    "    result.score = support >= 0.35\n"
    "        ? base_score * (0.70 + 0.30 * support) : 0.0;\n"
    "    result.id = 3;\n"
    "    return result;\n"
    "}\n"
    "\n"
    "RobustCandidate better_spatial_candidate(RobustCandidate current,\n"
    "                                         RobustCandidate candidate,\n"
    "                                         float ratio, float margin)\n"
    "{\n"
    "    float required = current.score * ratio + margin;\n"
    "    if (candidate.score > required)\n"
    "        return candidate;\n"
    "    return current;\n"
    "}\n"
    "\n"
    "RobustCandidate better_projected_owner_candidate(\n"
    "    RobustCandidate current, RobustCandidate candidate)\n"
    "{\n"
    "    float required = current.score * 1.03 + 0.004;\n"
    "    if (candidate.score > required)\n"
    "        return candidate;\n"
    "    return current;\n"
    "}\n"
    "\n"
    "RobustCandidate better_temporal_candidate(RobustCandidate current,\n"
    "                                          RobustCandidate candidate)\n"
    "{\n"
    "    float required = current.score * 1.08 + 0.01;\n"
    "    if (candidate.score > required)\n"
    "        return candidate;\n"
    "    return current;\n"
    "}\n"
    "\n"
    "struct RobustSelection\n"
    "{\n"
    "    RobustCandidate side0;\n"
    "    RobustCandidate side1;\n"
    "    uint mode;\n"
    "};\n"
    "\n"
    "RobustSelection robust_selection(float2 target, uint w, uint h)\n"
    "{\n"
    "    RobustSelection selection;\n"
    "    int2 owner_target = clamp((int2)round(target), int2(0, 0),\n"
    "        int2((int)w - 1, (int)h - 1));\n"
    "    uint occupancy_count0 = occupancy0.Load(int3(owner_target, 0));\n"
    "    uint occupancy_count1 = occupancy1.Load(int3(owner_target, 0));\n"
    "    ProjectedOwnerContext owner0 = load_projected_owner_context(\n"
    "        projected_owner0, flow0, target, occupancy_count0, w, h);\n"
    "    ProjectedOwnerContext owner1 = load_projected_owner_context(\n"
    "        projected_owner1, flow1, target, occupancy_count1, w, h);\n"
    "    selection.side0 = evaluate_candidate0(\n"
    "        target, 0, occupancy_count0, owner0, w, h);\n"
    "    selection.side0 = better_spatial_candidate(selection.side0,\n"
    "        evaluate_candidate0(target, 1, occupancy_count0, owner0, w, h),\n"
    "        1.02, 0.002);\n"
    "    selection.side0 = better_spatial_candidate(selection.side0,\n"
    "        evaluate_candidate0(target, 2, occupancy_count0, owner0, w, h),\n"
    "        1.02, 0.002);\n"
    "    selection.side0 = better_spatial_candidate(selection.side0,\n"
    "        evaluate_candidate0(target, 4, occupancy_count0, owner0, w, h),\n"
    "        1.03, 0.004);\n"
    "    selection.side0 = better_projected_owner_candidate(\n"
    "        selection.side0,\n"
    "        evaluate_projected_owner_candidate0(\n"
    "            target, occupancy_count0, owner0, w, h));\n"
    "    selection.side0 = better_temporal_candidate(selection.side0,\n"
    "        evaluate_temporal_candidate0(\n"
    "            target, occupancy_count0, owner0, w, h));\n"
    "    selection.side1 = evaluate_candidate1(\n"
    "        target, 0, occupancy_count1, owner1, w, h);\n"
    "    selection.side1 = better_spatial_candidate(selection.side1,\n"
    "        evaluate_candidate1(target, 1, occupancy_count1, owner1, w, h),\n"
    "        1.02, 0.002);\n"
    "    selection.side1 = better_spatial_candidate(selection.side1,\n"
    "        evaluate_candidate1(target, 2, occupancy_count1, owner1, w, h),\n"
    "        1.02, 0.002);\n"
    "    selection.side1 = better_spatial_candidate(selection.side1,\n"
    "        evaluate_candidate1(target, 4, occupancy_count1, owner1, w, h),\n"
    "        1.03, 0.004);\n"
    "    selection.side1 = better_projected_owner_candidate(\n"
    "        selection.side1,\n"
    "        evaluate_projected_owner_candidate1(\n"
    "            target, occupancy_count1, owner1, w, h));\n"
    "    selection.side1 = better_temporal_candidate(selection.side1,\n"
    "        evaluate_temporal_candidate1(\n"
    "            target, occupancy_count1, owner1, w, h));\n"
    "    bool valid0 = selection.side0.score >= 0.002;\n"
    "    bool valid1 = selection.side1.score >= 0.002;\n"
    "    if (!valid0 && !valid1)\n"
    "        selection.mode = 0;\n"
    "    else if (valid0 && !valid1)\n"
    "        selection.mode = 1;\n"
    "    else if (!valid0 && valid1)\n"
    "        selection.mode = 2;\n"
    "    else {\n"
    "        float value0 = sample_y(y0, selection.side0.source, w, h);\n"
    "        float value1 = sample_y(y1, selection.side1.source, w, h);\n"
    "        bool equivalent = min(selection.side0.score,\n"
    "                              selection.side1.score) >= 0.45 &&\n"
    "                          abs(value0 - value1) * 1023.0 <= 2.5;\n"
    "        selection.mode = equivalent ? 3 :\n"
    "            selection.side0.score >= selection.side1.score ? 1 : 2;\n"
    "    }\n"
    "    return selection;\n"
    "}\n"
    "#endif\n"
    "\n"
    "float encode_p010_code(float value)\n"
    "{\n"
    "    float code = clamp(round(value), 0.0, 1023.0);\n"
    "    return code * 64.0 / 65535.0;\n"
    "}\n"
    "\n"
    "float2 encode_p010_code_uv(float2 value)\n"
    "{\n"
    "    float2 code = clamp(round(value),\n"
    "                        float2(0.0, 0.0), float2(1023.0, 1023.0));\n"
    "    return code * 64.0 / 65535.0;\n"
    "}\n"
    "\n"
    "bool is_scene_cut()\n"
    "{\n"
    "#if USE_ROBUST_SCENE\n"
    "    return robust_scene_state[ROBUST_STATE_CLASS] ==\n"
    "           ROBUST_SCENE_HARD_CUT;\n"
    "#else\n"
    "    float samples = (float)scene_counters[0];\n"
    "    if (samples <= 0.0)\n"
    "        return false;\n"
    "    float average_delta = (float)scene_counters[1] / samples;\n"
    "    float changed_ratio = (float)scene_counters[2] / samples;\n"
    "    return average_delta >= SCENE_AVERAGE_THRESHOLD &&\n"
    "           changed_ratio >= SCENE_CHANGED_RATIO;\n"
    "#endif\n"
    "}\n"
    "\n"
    "uint scene_classification()\n"
    "{\n"
    "#if USE_ROBUST_SCENE\n"
    "    return robust_scene_state[ROBUST_STATE_CLASS];\n"
    "#else\n"
    "    return is_scene_cut() ? ROBUST_SCENE_HARD_CUT :\n"
    "                            ROBUST_SCENE_NORMAL;\n"
    "#endif\n"
    "}\n"
    "\n"
    "float interpolate_code(float a, float b, float fallback,\n"
    "                       float2 p0, float2 p1, uint w, uint h)\n"
    "{\n"
    "    float v0 = confidence_forward(p0, w, h);\n"
    "    float v1 = confidence_backward(p1, w, h);\n"
    "    if (v0 > FLOW_CONFIDENCE_MIN && v1 > FLOW_CONFIDENCE_MIN)\n"
    "        return (a * v0 + b * v1) / (v0 + v1);\n"
    "    if (v0 > FLOW_CONFIDENCE_MIN)\n"
    "        return a;\n"
    "    if (v1 > FLOW_CONFIDENCE_MIN)\n"
    "        return b;\n"
    "    return fallback;\n"
    "}\n"
    "\n"
    "[numthreads(8, 8, 1)]\n"
    "void main(uint3 id : SV_DispatchThreadID,\n"
    "          uint group_index : SV_GroupIndex)\n"
    "{\n"
    "#if USE_ROBUST_SCENE\n"
    "    if (group_index < ROBUST_SYNTHESIS_COUNTER_COUNT)\n"
    "        robust_counters[group_index] = 0;\n"
    "    GroupMemoryBarrierWithGroupSync();\n"
    "#endif\n"
    "    uint w, h;\n"
    "    out_y.GetDimensions(w, h);\n"
    "    uint uvw, uvh;\n"
    "    out_uv.GetDimensions(uvw, uvh);\n"
    "    uint scene = scene_classification();\n"
    "    bool hold_previous = scene == ROBUST_SCENE_HARD_CUT ||\n"
    "                         scene == ROBUST_SCENE_FLASH ||\n"
    "                         scene == ROBUST_SCENE_UNCERTAIN;\n"
    "    bool crossfade = scene == ROBUST_SCENE_FADE_DISSOLVE;\n"
    "    if (id.x < w && id.y < h) {\n"
    "        float2 target = (float2)id.xy;\n"
    "#if USE_ROBUST_SCENE\n"
    "        float c = sample_y(y0, target, w, h) * (65535.0 / 64.0);\n"
    "        float d = sample_y(y1, target, w, h) * (65535.0 / 64.0);\n"
    "        uint mode = 0;\n"
    "        uint selected_candidate = 0;\n"
    "        float2 selected_source0 = target;\n"
    "        float2 selected_source1 = target;\n"
    "        float value = c;\n"
    "        if (crossfade) {\n"
    "            mode = 3;\n"
    "            value = 0.5 * (c + d);\n"
    "        } else if (!hold_previous) {\n"
    "            RobustSelection selection = robust_selection(target, w, h);\n"
    "            mode = selection.mode;\n"
    "            selected_source0 = selection.side0.source;\n"
    "            selected_source1 = selection.side1.source;\n"
    "            float a = sample_y(y0, selection.side0.source, w, h) *\n"
    "                      (65535.0 / 64.0);\n"
    "            float b = sample_y(y1, selection.side1.source, w, h) *\n"
    "                      (65535.0 / 64.0);\n"
    "            value = mode == 1 ? a : mode == 2 ? b :\n"
    "                    mode == 3 ? 0.5 * (a + b) : c;\n"
    "            selected_candidate = mode == 2 ? selection.side1.id :\n"
    "                mode == 3 && selection.side1.score > selection.side0.score\n"
    "                    ? selection.side1.id : selection.side0.id;\n"
    "        }\n"
    "        out_y[id.xy] = encode_p010_code(value);\n"
    "        if ((id.x & 31) == 0 && (id.y & 31) == 0) {\n"
    "            InterlockedAdd(robust_counters[0], 1);\n"
    "            if (mode == 1)\n"
    "                InterlockedAdd(robust_counters[1], 1);\n"
    "            else if (mode == 2)\n"
    "                InterlockedAdd(robust_counters[2], 1);\n"
    "            else if (mode == 3)\n"
    "                InterlockedAdd(robust_counters[3], 1);\n"
    "            else\n"
    "                InterlockedAdd(robust_counters[4], 1);\n"
    "            if (!hold_previous && !crossfade && mode != 0) {\n"
    "                if (selected_candidate < 3)\n"
    "                    InterlockedAdd(robust_counters[\n"
    "                        5 + selected_candidate], 1);\n"
    "                else if (selected_candidate == 3)\n"
    "                    InterlockedAdd(robust_counters[12], 1);\n"
    "                else if (selected_candidate == 4)\n"
    "                    InterlockedAdd(robust_counters[13], 1);\n"
    "                else if (selected_candidate == 5)\n"
    "                    InterlockedAdd(robust_counters[18], 1);\n"
    "            }\n"
    "            uint occupancy_count0 = occupancy0.Load(int3(id.xy, 0));\n"
    "            uint occupancy_count1 = occupancy1.Load(int3(id.xy, 0));\n"
    "            if (occupancy_count0 == 0)\n"
    "                InterlockedAdd(robust_counters[8], 1);\n"
    "            else if (occupancy_count0 > 1)\n"
    "                InterlockedAdd(robust_counters[10], 1);\n"
    "            if (occupancy_count1 == 0)\n"
    "                InterlockedAdd(robust_counters[9], 1);\n"
    "            else if (occupancy_count1 > 1)\n"
    "                InterlockedAdd(robust_counters[11], 1);\n"
    "            if (!hold_previous && !crossfade) {\n"
    "                if (occupancy_count0 > 1) {\n"
    "                    float owner_support0 = projected_layer_support0(\n"
    "                        target, selected_source0, w, h);\n"
    "                    InterlockedAdd(robust_counters[\n"
    "                        owner_support0 >= 0.55 ? 14 : 16], 1);\n"
    "                }\n"
    "                if (occupancy_count1 > 1) {\n"
    "                    float owner_support1 = projected_layer_support1(\n"
    "                        target, selected_source1, w, h);\n"
    "                    InterlockedAdd(robust_counters[\n"
    "                        owner_support1 >= 0.55 ? 15 : 17], 1);\n"
    "                }\n"
    "            }\n"
    "        }\n"
    "#else\n"
    "        float2 p0, p1;\n"
    "        solve_sources(target, w, h, p0, p1);\n"
    "        float a = sample_y(y0, p0, w, h) * (65535.0 / 64.0);\n"
    "        float b = sample_y(y1, p1, w, h) * (65535.0 / 64.0);\n"
    "        float c = sample_y(y0, target, w, h) * (65535.0 / 64.0);\n"
    "        float d = sample_y(y1, target, w, h) * (65535.0 / 64.0);\n"
    "        float value = hold_previous ? c : crossfade ? 0.5 * (c + d) :\n"
    "            interpolate_code(a, b, c, p0, p1, w, h);\n"
    "        out_y[id.xy] = encode_p010_code(value);\n"
    "#endif\n"
    "    }\n"
    "    if (id.x < uvw && id.y < uvh) {\n"
    "        float2 target = float2(id.x * 2.0, id.y * 2.0 + 0.5);\n"
    "#if USE_ROBUST_SCENE\n"
    "        float2 direct_uv = (target - float2(0.0, 0.5)) * 0.5;\n"
    "        float2 uvc = sample_uv(uv0, direct_uv, uvw, uvh) *\n"
    "                     (65535.0 / 64.0);\n"
    "        float2 uvd = sample_uv(uv1, direct_uv, uvw, uvh) *\n"
    "                     (65535.0 / 64.0);\n"
    "        float2 value = uvc;\n"
    "        if (crossfade) {\n"
    "            value = 0.5 * (uvc + uvd);\n"
    "        } else if (!hold_previous) {\n"
    "            RobustSelection selection = robust_selection(target, w, h);\n"
    "            float2 uvp0 = (selection.side0.source -\n"
    "                           float2(0.0, 0.5)) * 0.5;\n"
    "            float2 uvp1 = (selection.side1.source -\n"
    "                           float2(0.0, 0.5)) * 0.5;\n"
    "            float2 uva = sample_uv(uv0, uvp0, uvw, uvh) *\n"
    "                         (65535.0 / 64.0);\n"
    "            float2 uvb = sample_uv(uv1, uvp1, uvw, uvh) *\n"
    "                         (65535.0 / 64.0);\n"
    "            value = selection.mode == 1 ? uva :\n"
    "                    selection.mode == 2 ? uvb :\n"
    "                    selection.mode == 3 ? 0.5 * (uva + uvb) : uvc;\n"
    "        }\n"
    "        out_uv[id.xy] = encode_p010_code_uv(value);\n"
    "#else\n"
    "        float2 p0, p1;\n"
    "        solve_sources(target, w, h, p0, p1);\n"
    "        float2 uvp0 = (p0 - float2(0.0, 0.5)) * 0.5;\n"
    "        float2 uvp1 = (p1 - float2(0.0, 0.5)) * 0.5;\n"
    "        float2 uva = sample_uv(uv0, uvp0, uvw, uvh) * (65535.0 / 64.0);\n"
    "        float2 uvb = sample_uv(uv1, uvp1, uvw, uvh) * (65535.0 / 64.0);\n"
    "        float2 uvc = sample_uv(uv0, (target - float2(0.0, 0.5)) * 0.5,\n"
    "                               uvw, uvh) * (65535.0 / 64.0);\n"
    "        float2 uvd = sample_uv(uv1, (target - float2(0.0, 0.5)) * 0.5,\n"
    "                               uvw, uvh) * (65535.0 / 64.0);\n"
    "        float v0 = confidence_forward(p0, w, h);\n"
    "        float v1 = confidence_backward(p1, w, h);\n"
    "        float2 value = v0 > FLOW_CONFIDENCE_MIN &&\n"
    "                       v1 > FLOW_CONFIDENCE_MIN\n"
    "            ? (uva * v0 + uvb * v1) / (v0 + v1)\n"
    "            : v0 > FLOW_CONFIDENCE_MIN ? uva\n"
    "            : v1 > FLOW_CONFIDENCE_MIN ? uvb : uvc;\n"
    "        value = hold_previous ? uvc : crossfade ? 0.5 * (uvc + uvd) :\n"
    "                value;\n"
    "        out_uv[id.xy] = encode_p010_code_uv(value);\n"
    "#endif\n"
    "    }\n"
    "#if USE_ROBUST_SCENE\n"
    "    GroupMemoryBarrierWithGroupSync();\n"
    "    if (group_index < ROBUST_SYNTHESIS_COUNTER_COUNT)\n"
    "        InterlockedAdd(robust_synthesis_summary[group_index],\n"
    "                       robust_counters[group_index]);\n"
    "#else\n"
    "    if (id.x == 0 && id.y == 0) {\n"
    "        InterlockedAdd(scene_summary[0], 1);\n"
    "        if (is_scene_cut())\n"
    "            InterlockedAdd(scene_summary[1], 1);\n"
    "    }\n"
    "#endif\n"
    "}\n";

static const char *nvof_status_name(NV_OF_STATUS status)
{
    switch (status) {
    case NV_OF_SUCCESS: return "success";
    case NV_OF_ERR_OF_NOT_AVAILABLE: return "not_available";
    case NV_OF_ERR_UNSUPPORTED_DEVICE: return "unsupported_device";
    case NV_OF_ERR_DEVICE_DOES_NOT_EXIST: return "device_missing";
    case NV_OF_ERR_INVALID_PTR: return "invalid_pointer";
    case NV_OF_ERR_INVALID_PARAM: return "invalid_parameter";
    case NV_OF_ERR_INVALID_CALL: return "invalid_call";
    case NV_OF_ERR_INVALID_VERSION: return "invalid_version";
    case NV_OF_ERR_OUT_OF_MEMORY: return "out_of_memory";
    case NV_OF_ERR_NOT_INITIALIZED: return "not_initialized";
    case NV_OF_ERR_UNSUPPORTED_FEATURE: return "unsupported_feature";
    default: return "generic_error";
    }
}

static const char *robust_scene_class_name(enum robust_scene_class scene)
{
    switch (scene) {
    case ROBUST_SCENE_NORMAL: return "normal";
    case ROBUST_SCENE_FLASH: return "flash";
    case ROBUST_SCENE_FADE_DISSOLVE: return "fade-dissolve";
    case ROBUST_SCENE_HARD_CUT: return "hard-cut";
    case ROBUST_SCENE_UNCERTAIN: return "uncertain";
    default: return "unknown";
    }
}

static bool check_nvof_status(struct mp_filter *f, const char *operation,
                              NV_OF_STATUS status)
{
    if (status == NV_OF_SUCCESS)
        return true;

    struct priv *p = f->priv;
    char detail[512] = {0};
    uint32_t detail_size = sizeof(detail);
    if (p->nvof.handle && p->nvof.api.nvOFGetLastError) {
        p->nvof.api.nvOFGetLastError(p->nvof.handle, detail, &detail_size);
        detail[sizeof(detail) - 1] = '\0';
    }
    MP_ERR(f, "NVOF operation failed operation=%s status=%u name=%s "
              "detail=%s\n",
           operation, (unsigned)status, nvof_status_name(status),
           detail[0] ? detail : "none");
    return false;
}

static void release_nvof_resource(struct mp_filter *f,
                                  struct nvof_resource *resource)
{
    struct priv *p = f->priv;
    if (resource->handle && p->nvof.api.nvOFUnregisterResourceD3D11) {
        check_nvof_status(f, "nvOFUnregisterResourceD3D11",
            p->nvof.api.nvOFUnregisterResourceD3D11(resource->handle));
    }
    resource->handle = NULL;
    if (resource->srv)
        ID3D11ShaderResourceView_Release(resource->srv);
    resource->srv = NULL;
    if (resource->uav)
        ID3D11UnorderedAccessView_Release(resource->uav);
    resource->uav = NULL;
    if (resource->texture)
        ID3D11Texture2D_Release(resource->texture);
    resource->texture = NULL;
}

static void release_shader_texture_resource(
    struct shader_texture_resource *resource)
{
    if (resource->srv)
        ID3D11ShaderResourceView_Release(resource->srv);
    resource->srv = NULL;
    if (resource->uav)
        ID3D11UnorderedAccessView_Release(resource->uav);
    resource->uav = NULL;
    if (resource->texture)
        ID3D11Texture2D_Release(resource->texture);
    resource->texture = NULL;
}

static void release_robust_pair_cache(struct robust_pair_cache *cache)
{
    release_shader_texture_resource(&cache->flow_forward);
    release_shader_texture_resource(&cache->flow_backward);
    release_shader_texture_resource(&cache->cost_forward);
    release_shader_texture_resource(&cache->cost_backward);
    if (cache->scene_state_srv)
        ID3D11ShaderResourceView_Release(cache->scene_state_srv);
    cache->scene_state_srv = NULL;
    if (cache->scene_state_buffer)
        ID3D11Buffer_Release(cache->scene_state_buffer);
    cache->scene_state_buffer = NULL;
    cache->valid = false;
}

static void release_nvof_session(struct mp_filter *f)
{
    struct priv *p = f->priv;
    struct nvof_resource *resources[] = {
        &p->nvof.gray[0],
        &p->nvof.gray[1],
        &p->nvof.flow_forward,
        &p->nvof.flow_backward,
        &p->nvof.cost_forward,
        &p->nvof.cost_backward,
        &p->nvof.global_flow,
        &p->nvof.flow_state_forward[0],
        &p->nvof.flow_state_forward[1],
        &p->nvof.flow_state_backward[0],
        &p->nvof.flow_state_backward[1],
    };
    for (int n = 0; n < MP_ARRAY_SIZE(resources); n++)
        release_nvof_resource(f, resources[n]);
    for (int n = 0; n < MP_ARRAY_SIZE(p->nvof.occupancy); n++)
        release_shader_texture_resource(&p->nvof.occupancy[n]);
    for (int n = 0; n < MP_ARRAY_SIZE(p->nvof.projected_owner); n++)
        release_shader_texture_resource(&p->nvof.projected_owner[n]);
    for (int n = 0; n < MP_ARRAY_SIZE(p->nvof.pair_cache); n++)
        release_robust_pair_cache(&p->nvof.pair_cache[n]);
    if (p->nvof.completion_readback)
        ID3D11Texture2D_Release(p->nvof.completion_readback);
    p->nvof.completion_readback = NULL;
    if (p->nvof.handle && p->nvof.api.nvOFDestroy) {
        check_nvof_status(f, "nvOFDestroy",
                          p->nvof.api.nvOFDestroy(p->nvof.handle));
    }
    p->nvof.handle = NULL;
    p->nvof.width = 0;
    p->nvof.height = 0;
}

static void release_flow_diagnostics(struct priv *p)
{
    if (p->nvof.flow_diagnostics_uav)
        ID3D11UnorderedAccessView_Release(p->nvof.flow_diagnostics_uav);
    p->nvof.flow_diagnostics_uav = NULL;
    if (p->nvof.flow_diagnostics_readback)
        ID3D11Buffer_Release(p->nvof.flow_diagnostics_readback);
    p->nvof.flow_diagnostics_readback = NULL;
    if (p->nvof.flow_diagnostics_buffer)
        ID3D11Buffer_Release(p->nvof.flow_diagnostics_buffer);
    p->nvof.flow_diagnostics_buffer = NULL;
    if (p->nvof.flow_diagnostics_shader)
        ID3D11ComputeShader_Release(p->nvof.flow_diagnostics_shader);
    p->nvof.flow_diagnostics_shader = NULL;
}

static void release_scene_cut_resources(struct priv *p)
{
    if (p->nvof.scene_cut_summary_uav)
        ID3D11UnorderedAccessView_Release(p->nvof.scene_cut_summary_uav);
    p->nvof.scene_cut_summary_uav = NULL;
    if (p->nvof.scene_cut_summary_readback)
        ID3D11Buffer_Release(p->nvof.scene_cut_summary_readback);
    p->nvof.scene_cut_summary_readback = NULL;
    if (p->nvof.scene_cut_summary_buffer)
        ID3D11Buffer_Release(p->nvof.scene_cut_summary_buffer);
    p->nvof.scene_cut_summary_buffer = NULL;
    if (p->nvof.scene_cut_srv)
        ID3D11ShaderResourceView_Release(p->nvof.scene_cut_srv);
    p->nvof.scene_cut_srv = NULL;
    if (p->nvof.scene_cut_uav)
        ID3D11UnorderedAccessView_Release(p->nvof.scene_cut_uav);
    p->nvof.scene_cut_uav = NULL;
    if (p->nvof.scene_cut_buffer)
        ID3D11Buffer_Release(p->nvof.scene_cut_buffer);
    p->nvof.scene_cut_buffer = NULL;
    if (p->nvof.scene_cut_shader)
        ID3D11ComputeShader_Release(p->nvof.scene_cut_shader);
    p->nvof.scene_cut_shader = NULL;
}

static void release_robust_scene_resources(struct priv *p)
{
    if (p->nvof.robust_temporal_constants_buffer)
        ID3D11Buffer_Release(p->nvof.robust_temporal_constants_buffer);
    p->nvof.robust_temporal_constants_buffer = NULL;
    if (p->nvof.robust_synthesis_summary_readback)
        ID3D11Buffer_Release(p->nvof.robust_synthesis_summary_readback);
    p->nvof.robust_synthesis_summary_readback = NULL;
    if (p->nvof.robust_synthesis_summary_uav)
        ID3D11UnorderedAccessView_Release(
            p->nvof.robust_synthesis_summary_uav);
    p->nvof.robust_synthesis_summary_uav = NULL;
    if (p->nvof.robust_synthesis_summary_buffer)
        ID3D11Buffer_Release(p->nvof.robust_synthesis_summary_buffer);
    p->nvof.robust_synthesis_summary_buffer = NULL;
    if (p->nvof.robust_scene_summary_readback)
        ID3D11Buffer_Release(p->nvof.robust_scene_summary_readback);
    p->nvof.robust_scene_summary_readback = NULL;
    if (p->nvof.robust_scene_summary_uav)
        ID3D11UnorderedAccessView_Release(p->nvof.robust_scene_summary_uav);
    p->nvof.robust_scene_summary_uav = NULL;
    if (p->nvof.robust_scene_summary_buffer)
        ID3D11Buffer_Release(p->nvof.robust_scene_summary_buffer);
    p->nvof.robust_scene_summary_buffer = NULL;
    if (p->nvof.robust_scene_state_srv)
        ID3D11ShaderResourceView_Release(p->nvof.robust_scene_state_srv);
    p->nvof.robust_scene_state_srv = NULL;
    if (p->nvof.robust_scene_state_uav)
        ID3D11UnorderedAccessView_Release(p->nvof.robust_scene_state_uav);
    p->nvof.robust_scene_state_uav = NULL;
    if (p->nvof.robust_scene_state_buffer)
        ID3D11Buffer_Release(p->nvof.robust_scene_state_buffer);
    p->nvof.robust_scene_state_buffer = NULL;
    if (p->nvof.robust_scene_descriptor_srv)
        ID3D11ShaderResourceView_Release(
            p->nvof.robust_scene_descriptor_srv);
    p->nvof.robust_scene_descriptor_srv = NULL;
    if (p->nvof.robust_scene_descriptor_uav)
        ID3D11UnorderedAccessView_Release(
            p->nvof.robust_scene_descriptor_uav);
    p->nvof.robust_scene_descriptor_uav = NULL;
    if (p->nvof.robust_scene_descriptor_buffer)
        ID3D11Buffer_Release(p->nvof.robust_scene_descriptor_buffer);
    p->nvof.robust_scene_descriptor_buffer = NULL;
    if (p->nvof.robust_scene_classify_shader)
        ID3D11ComputeShader_Release(
            p->nvof.robust_scene_classify_shader);
    p->nvof.robust_scene_classify_shader = NULL;
    if (p->nvof.robust_scene_descriptor_shader)
        ID3D11ComputeShader_Release(
            p->nvof.robust_scene_descriptor_shader);
    p->nvof.robust_scene_descriptor_shader = NULL;
    if (p->nvof.robust_occupancy_shader)
        ID3D11ComputeShader_Release(p->nvof.robust_occupancy_shader);
    p->nvof.robust_occupancy_shader = NULL;
}

static void destroy_nvof(struct mp_filter *f)
{
    struct priv *p = f->priv;
    release_nvof_session(f);
    if (p->nvof.promote_nv12_shader)
        ID3D11ComputeShader_Release(p->nvof.promote_nv12_shader);
    p->nvof.promote_nv12_shader = NULL;
    if (p->nvof.extract_luma_shader)
        ID3D11ComputeShader_Release(p->nvof.extract_luma_shader);
    p->nvof.extract_luma_shader = NULL;
    if (p->nvof.synthesize_p010_shader)
        ID3D11ComputeShader_Release(p->nvof.synthesize_p010_shader);
    p->nvof.synthesize_p010_shader = NULL;
    if (p->nvof.prepare_flow_shader)
        ID3D11ComputeShader_Release(p->nvof.prepare_flow_shader);
    p->nvof.prepare_flow_shader = NULL;
    if (p->nvof.infill_flow_shader)
        ID3D11ComputeShader_Release(p->nvof.infill_flow_shader);
    p->nvof.infill_flow_shader = NULL;
    release_flow_diagnostics(p);
    release_scene_cut_resources(p);
    release_robust_scene_resources(p);
    release_gpu_profile_queries(p);
    if (p->nvof.d3dcompiler_module)
        FreeLibrary(p->nvof.d3dcompiler_module);
    p->nvof.d3dcompiler_module = NULL;
    p->nvof.d3d_compile = NULL;
    if (p->nvof.module)
        FreeLibrary(p->nvof.module);
    p->nvof.module = NULL;
}

static bool load_nvof_api(struct mp_filter *f)
{
    struct priv *p = f->priv;
    p->nvof.module = LoadLibraryW(L"nvofapi64.dll");
    if (!p->nvof.module) {
        MP_ERR(f, "NVOF could not load nvofapi64.dll win32-error=%lu\n",
               (unsigned long)GetLastError());
        return false;
    }
    nvof_get_max_version_fn get_max_version =
        (nvof_get_max_version_fn)GetProcAddress(
            p->nvof.module, "NvOFGetMaxSupportedApiVersion");
    nvof_create_instance_d3d11_fn create_instance =
        (nvof_create_instance_d3d11_fn)GetProcAddress(
            p->nvof.module, "NvOFAPICreateInstanceD3D11");
    if (!get_max_version || !create_instance) {
        MP_ERR(f, "NVOF required D3D11 entry points are missing\n");
        return false;
    }
    if (!check_nvof_status(f, "NvOFGetMaxSupportedApiVersion",
                           get_max_version(&p->nvof.driver_api_version)))
        return false;
    if (p->nvof.driver_api_version < NV_OF_API_VERSION) {
        MP_ERR(f, "NVOF driver API is too old driver=%u.%u required=%u.%u\n",
               p->nvof.driver_api_version >> 4,
               p->nvof.driver_api_version & 0xf,
               NV_OF_API_VERSION >> 4, NV_OF_API_VERSION & 0xf);
        return false;
    }
    if (!check_nvof_status(f, "NvOFAPICreateInstanceD3D11",
                           create_instance(NV_OF_API_VERSION, &p->nvof.api)))
        return false;
    p->nvof.disable_temporal_hints_next = true;
    MP_INFO(f, "NVOF API loaded driver-api=%u.%u client-api=%u.%u\n",
            p->nvof.driver_api_version >> 4,
            p->nvof.driver_api_version & 0xf,
            NV_OF_API_VERSION >> 4, NV_OF_API_VERSION & 0xf);
    return true;
}

static bool load_d3dcompiler_runtime(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->nvof.d3d_compile)
        return true;
    static const wchar_t *compiler_names[] = {
        L"d3dcompiler_47.dll",
        L"d3dcompiler_46.dll",
        L"d3dcompiler_43.dll",
    };
    for (int n = 0; n < MP_ARRAY_SIZE(compiler_names); n++) {
        p->nvof.d3dcompiler_module = LoadLibraryW(compiler_names[n]);
        if (p->nvof.d3dcompiler_module)
            break;
    }
    if (!p->nvof.d3dcompiler_module) {
        MP_ERR(f, "Frame interpolation could not load a D3DCompiler "
                  "runtime\n");
        return false;
    }
    p->nvof.d3d_compile = (pD3DCompile)GetProcAddress(
        p->nvof.d3dcompiler_module, "D3DCompile");
    if (!p->nvof.d3d_compile) {
        MP_ERR(f, "Frame interpolation D3DCompile entry point is missing\n");
        return false;
    }
    return true;
}

static bool load_extract_luma_shader(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->nvof.extract_luma_shader)
        return true;
    if (!load_d3dcompiler_runtime(f))
        return false;

    ID3DBlob *bytecode = NULL;
    ID3DBlob *errors = NULL;
    HRESULT hr = p->nvof.d3d_compile(
        extract_luma_shader_source, sizeof(extract_luma_shader_source) - 1,
        "extract_luma.hlsl", NULL, NULL, "main", "cs_5_0",
        D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_WARNINGS_ARE_ERRORS,
        0, &bytecode, &errors);
    if (FAILED(hr)) {
        const char *message = errors
            ? ID3D10Blob_GetBufferPointer(errors) : "no compiler diagnostics";
        MP_ERR(f, "NVOF extract-luma shader compilation failed hr=0x%08lx "
                  "detail=%s\n", (unsigned long)hr, message);
        if (errors)
            ID3D10Blob_Release(errors);
        if (bytecode)
            ID3D10Blob_Release(bytecode);
        return false;
    }
    if (errors)
        ID3D10Blob_Release(errors);
    hr = ID3D11Device_CreateComputeShader(
        p->device, ID3D10Blob_GetBufferPointer(bytecode),
        ID3D10Blob_GetBufferSize(bytecode), NULL,
        &p->nvof.extract_luma_shader);
    ID3D10Blob_Release(bytecode);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF could not create extract-luma compute shader "
                  "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }
    MP_INFO(f, "NVOF extract-luma shader ready input=P010-R16_UNORM "
               "output=GRAY8\n");
    return true;
}

static bool load_promote_nv12_shader(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->nvof.promote_nv12_shader)
        return true;
    if (!load_d3dcompiler_runtime(f))
        return false;

    ID3DBlob *bytecode = NULL;
    ID3DBlob *errors = NULL;
    HRESULT hr = p->nvof.d3d_compile(
        promote_nv12_shader_source, sizeof(promote_nv12_shader_source) - 1,
        "promote_nv12.hlsl", NULL, NULL, "main", "cs_5_0",
        D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_WARNINGS_ARE_ERRORS,
        0, &bytecode, &errors);
    if (FAILED(hr)) {
        const char *message = errors
            ? ID3D10Blob_GetBufferPointer(errors) : "no compiler diagnostics";
        MP_ERR(f, "NVOF NV12 promotion shader compilation failed "
                  "hr=0x%08lx detail=%s\n", (unsigned long)hr, message);
        if (errors)
            ID3D10Blob_Release(errors);
        if (bytecode)
            ID3D10Blob_Release(bytecode);
        return false;
    }
    if (errors)
        ID3D10Blob_Release(errors);
    hr = ID3D11Device_CreateComputeShader(
        p->device, ID3D10Blob_GetBufferPointer(bytecode),
        ID3D10Blob_GetBufferSize(bytecode), NULL,
        &p->nvof.promote_nv12_shader);
    ID3D10Blob_Release(bytecode);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF could not create NV12 promotion compute shader "
                  "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }
    MP_INFO(f, "NVOF NV12 promotion shader ready input=NV12 "
               "output=P010 mapping=code8<<2\n");
    return true;
}

static bool load_scene_cut_shader(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->nvof.scene_cut_shader && p->nvof.scene_cut_buffer &&
        p->nvof.scene_cut_uav && p->nvof.scene_cut_srv &&
        p->nvof.scene_cut_summary_buffer &&
        p->nvof.scene_cut_summary_readback &&
        p->nvof.scene_cut_summary_uav)
        return true;
    release_scene_cut_resources(p);
    if (!p->nvof.d3d_compile) {
        MP_ERR(f, "NVOF scene-cut detection requires the D3DCompiler "
                  "runtime\n");
        return false;
    }

    char sample_stride[16];
    char pixel_threshold[16];
    snprintf(sample_stride, sizeof(sample_stride), "%d",
             p->opts->scene_cut_sample_stride);
    snprintf(pixel_threshold, sizeof(pixel_threshold), "%d",
             p->opts->scene_cut_pixel_threshold);
    D3D_SHADER_MACRO macros[] = {
        {"SCENE_SAMPLE_STRIDE", sample_stride},
        {"SCENE_PIXEL_THRESHOLD", pixel_threshold},
        {NULL, NULL},
    };
    ID3DBlob *bytecode = NULL;
    ID3DBlob *errors = NULL;
    HRESULT hr = p->nvof.d3d_compile(
        scene_cut_shader_source, sizeof(scene_cut_shader_source) - 1,
        "scene_cut.hlsl", macros, NULL, "main", "cs_5_0",
        D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_WARNINGS_ARE_ERRORS,
        0, &bytecode, &errors);
    if (FAILED(hr)) {
        const char *message = errors
            ? ID3D10Blob_GetBufferPointer(errors) : "no compiler diagnostics";
        MP_ERR(f, "NVOF scene-cut shader compilation failed hr=0x%08lx "
                  "detail=%s\n", (unsigned long)hr, message);
        if (errors)
            ID3D10Blob_Release(errors);
        if (bytecode)
            ID3D10Blob_Release(bytecode);
        return false;
    }
    if (errors)
        ID3D10Blob_Release(errors);
    hr = ID3D11Device_CreateComputeShader(
        p->device, ID3D10Blob_GetBufferPointer(bytecode),
        ID3D10Blob_GetBufferSize(bytecode), NULL,
        &p->nvof.scene_cut_shader);
    ID3D10Blob_Release(bytecode);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF could not create scene-cut compute shader "
                  "hr=0x%08lx\n", (unsigned long)hr);
        release_scene_cut_resources(p);
        return false;
    }

    D3D11_BUFFER_DESC counter_desc = {
        .ByteWidth = SCENE_CUT_COUNTER_COUNT * sizeof(uint32_t),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_UNORDERED_ACCESS |
                     D3D11_BIND_SHADER_RESOURCE,
        .MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
        .StructureByteStride = sizeof(uint32_t),
    };
    hr = ID3D11Device_CreateBuffer(
        p->device, &counter_desc, NULL, &p->nvof.scene_cut_buffer);
    D3D11_UNORDERED_ACCESS_VIEW_DESC uav_desc = {
        .Format = DXGI_FORMAT_UNKNOWN,
        .ViewDimension = D3D11_UAV_DIMENSION_BUFFER,
        .Buffer = {
            .FirstElement = 0,
            .NumElements = SCENE_CUT_COUNTER_COUNT,
        },
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device, (ID3D11Resource *)p->nvof.scene_cut_buffer,
            &uav_desc, &p->nvof.scene_cut_uav);
    }
    if (SUCCEEDED(hr)) {
        D3D11_SHADER_RESOURCE_VIEW_DESC srv_desc = {
            .Format = DXGI_FORMAT_UNKNOWN,
            .ViewDimension = D3D11_SRV_DIMENSION_BUFFER,
            .Buffer = {
                .FirstElement = 0,
                .NumElements = SCENE_CUT_COUNTER_COUNT,
            },
        };
        hr = ID3D11Device_CreateShaderResourceView(
            p->device, (ID3D11Resource *)p->nvof.scene_cut_buffer,
            &srv_desc, &p->nvof.scene_cut_srv);
    }
    D3D11_BUFFER_DESC summary_desc = {
        .ByteWidth = SCENE_CUT_SUMMARY_COUNTER_COUNT * sizeof(uint32_t),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_UNORDERED_ACCESS,
        .MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
        .StructureByteStride = sizeof(uint32_t),
    };
    const uint32_t zero_summary[SCENE_CUT_SUMMARY_COUNTER_COUNT] = {0};
    D3D11_SUBRESOURCE_DATA summary_data = {
        .pSysMem = zero_summary,
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateBuffer(
            p->device, &summary_desc, &summary_data,
            &p->nvof.scene_cut_summary_buffer);
    }
    uav_desc.Buffer.NumElements = SCENE_CUT_SUMMARY_COUNTER_COUNT;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device,
            (ID3D11Resource *)p->nvof.scene_cut_summary_buffer,
            &uav_desc, &p->nvof.scene_cut_summary_uav);
    }
    D3D11_BUFFER_DESC readback_desc = {
        .ByteWidth = summary_desc.ByteWidth,
        .Usage = D3D11_USAGE_STAGING,
        .CPUAccessFlags = D3D11_CPU_ACCESS_READ,
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateBuffer(
            p->device, &readback_desc, NULL,
            &p->nvof.scene_cut_summary_readback);
    }
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF scene-cut buffer creation failed hr=0x%08lx\n",
               (unsigned long)hr);
        release_scene_cut_resources(p);
        return false;
    }
    MP_INFO(f, "NVOF GPU-resident scene-cut detector ready sample-stride=%d "
               "pixel-threshold=%d average-threshold=%.2f "
               "changed-ratio=%.3f\n",
            p->opts->scene_cut_sample_stride,
            p->opts->scene_cut_pixel_threshold,
            p->opts->scene_cut_average_threshold,
            p->opts->scene_cut_changed_ratio);
    return true;
}

static bool load_robust_scene_shaders(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->nvof.robust_scene_descriptor_shader &&
        p->nvof.robust_scene_classify_shader &&
        p->nvof.robust_occupancy_shader &&
        p->nvof.robust_scene_descriptor_buffer &&
        p->nvof.robust_scene_descriptor_uav &&
        p->nvof.robust_scene_descriptor_srv &&
        p->nvof.robust_scene_state_buffer &&
        p->nvof.robust_scene_state_uav &&
        p->nvof.robust_scene_state_srv &&
        p->nvof.robust_scene_summary_buffer &&
        p->nvof.robust_scene_summary_uav &&
        p->nvof.robust_scene_summary_readback &&
        p->nvof.robust_synthesis_summary_buffer &&
        p->nvof.robust_synthesis_summary_uav &&
        p->nvof.robust_synthesis_summary_readback &&
        p->nvof.robust_temporal_constants_buffer)
        return true;
    release_robust_scene_resources(p);
    if (!p->nvof.d3d_compile) {
        MP_ERR(f, "NVOF robust scene analysis requires the D3DCompiler "
                  "runtime\n");
        return false;
    }

    char sample_stride[16];
    char pixel_threshold[16];
    char region_count[16];
    char histogram_bins[16];
    char histogram_count[16];
    char descriptor_count[16];
    snprintf(sample_stride, sizeof(sample_stride), "%d",
             p->opts->scene_cut_sample_stride);
    snprintf(pixel_threshold, sizeof(pixel_threshold), "%d",
             p->opts->scene_cut_pixel_threshold);
    snprintf(region_count, sizeof(region_count), "%d",
             ROBUST_SCENE_REGION_COUNT);
    snprintf(histogram_bins, sizeof(histogram_bins), "%d",
             ROBUST_SCENE_HISTOGRAM_BINS);
    snprintf(histogram_count, sizeof(histogram_count), "%d",
             ROBUST_SCENE_HISTOGRAM_COUNT);
    snprintf(descriptor_count, sizeof(descriptor_count), "%d",
             ROBUST_SCENE_DESCRIPTOR_COUNT);
    D3D_SHADER_MACRO descriptor_macros[] = {
        {"SCENE_SAMPLE_STRIDE", sample_stride},
        {"SCENE_PIXEL_THRESHOLD", pixel_threshold},
        {"ROBUST_REGION_COUNT", region_count},
        {"ROBUST_HISTOGRAM_BINS", histogram_bins},
        {"ROBUST_HISTOGRAM_COUNT", histogram_count},
        {"ROBUST_DESCRIPTOR_COUNT", descriptor_count},
        {NULL, NULL},
    };
    D3D_SHADER_MACRO classify_macros[] = {
        {"ROBUST_REGION_COUNT", region_count},
        {"ROBUST_HISTOGRAM_BINS", histogram_bins},
        {"ROBUST_HISTOGRAM_COUNT", histogram_count},
        {"ROBUST_SCENE_CLASS_COUNT", "5"},
        {"ROBUST_SCENE_METRIC_COUNT", "7"},
        {"ROBUST_SCENE_NORMAL", "0"},
        {"ROBUST_SCENE_FLASH", "1"},
        {"ROBUST_SCENE_FADE_DISSOLVE", "2"},
        {"ROBUST_SCENE_HARD_CUT", "3"},
        {"ROBUST_SCENE_UNCERTAIN", "4"},
        {"ROBUST_STATE_CLASS", "0"},
        {"ROBUST_STATE_PREVIOUS_CLASS", "1"},
        {"ROBUST_STATE_STREAK", "2"},
        {"ROBUST_STATE_KL_MILLI", "3"},
        {"ROBUST_STATE_AVERAGE_DELTA_MILLI", "4"},
        {"ROBUST_STATE_CHANGED_RATIO_MILLI", "5"},
        {"ROBUST_STATE_CHROMA_DELTA_MILLI", "6"},
        {"ROBUST_STATE_EDGE_DELTA_MILLI", "7"},
        {"ROBUST_STATE_EXPOSURE_DELTA_MILLI", "8"},
        {"ROBUST_STATE_REGIONAL_KL_MAX_MILLI", "9"},
        {"ROBUST_STATE_EXPOSURE_SPREAD_MILLI", "10"},
        {NULL, NULL},
    };
    const char *sources[] = {
        robust_scene_descriptor_shader_source,
        robust_scene_classify_shader_source,
        robust_occupancy_shader_source,
    };
    const size_t source_sizes[] = {
        sizeof(robust_scene_descriptor_shader_source) - 1,
        sizeof(robust_scene_classify_shader_source) - 1,
        sizeof(robust_occupancy_shader_source) - 1,
    };
    const char *names[] = {
        "robust_scene_descriptor.hlsl",
        "robust_scene_classify.hlsl",
        "robust_occupancy.hlsl",
    };
    const D3D_SHADER_MACRO *macro_sets[] = {
        descriptor_macros,
        classify_macros,
        NULL,
    };
    ID3D11ComputeShader **shaders[] = {
        &p->nvof.robust_scene_descriptor_shader,
        &p->nvof.robust_scene_classify_shader,
        &p->nvof.robust_occupancy_shader,
    };
    for (int n = 0; n < MP_ARRAY_SIZE(sources); n++) {
        ID3DBlob *bytecode = NULL;
        ID3DBlob *errors = NULL;
        HRESULT hr = p->nvof.d3d_compile(
            sources[n], source_sizes[n], names[n], macro_sets[n], NULL,
            "main", "cs_5_0",
            D3DCOMPILE_OPTIMIZATION_LEVEL3 |
            D3DCOMPILE_WARNINGS_ARE_ERRORS,
            0, &bytecode, &errors);
        if (FAILED(hr)) {
            const char *message = errors
                ? ID3D10Blob_GetBufferPointer(errors)
                : "no compiler diagnostics";
            MP_ERR(f, "NVOF robust scene shader compilation failed "
                      "shader=%s hr=0x%08lx detail=%s\n",
                   names[n], (unsigned long)hr, message);
            if (errors)
                ID3D10Blob_Release(errors);
            if (bytecode)
                ID3D10Blob_Release(bytecode);
            release_robust_scene_resources(p);
            return false;
        }
        if (errors)
            ID3D10Blob_Release(errors);
        hr = ID3D11Device_CreateComputeShader(
            p->device, ID3D10Blob_GetBufferPointer(bytecode),
            ID3D10Blob_GetBufferSize(bytecode), NULL, shaders[n]);
        ID3D10Blob_Release(bytecode);
        if (FAILED(hr)) {
            MP_ERR(f, "NVOF could not create robust scene shader "
                      "shader=%s hr=0x%08lx\n",
                   names[n], (unsigned long)hr);
            release_robust_scene_resources(p);
            return false;
        }
    }

    D3D11_BUFFER_DESC descriptor_desc = {
        .ByteWidth = ROBUST_SCENE_DESCRIPTOR_COUNT * sizeof(uint32_t),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_UNORDERED_ACCESS |
                     D3D11_BIND_SHADER_RESOURCE,
        .MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
        .StructureByteStride = sizeof(uint32_t),
    };
    HRESULT hr = ID3D11Device_CreateBuffer(
        p->device, &descriptor_desc, NULL,
        &p->nvof.robust_scene_descriptor_buffer);
    D3D11_UNORDERED_ACCESS_VIEW_DESC uav_desc = {
        .Format = DXGI_FORMAT_UNKNOWN,
        .ViewDimension = D3D11_UAV_DIMENSION_BUFFER,
        .Buffer = {
            .FirstElement = 0,
            .NumElements = ROBUST_SCENE_DESCRIPTOR_COUNT,
        },
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device,
            (ID3D11Resource *)p->nvof.robust_scene_descriptor_buffer,
            &uav_desc, &p->nvof.robust_scene_descriptor_uav);
    }
    D3D11_SHADER_RESOURCE_VIEW_DESC srv_desc = {
        .Format = DXGI_FORMAT_UNKNOWN,
        .ViewDimension = D3D11_SRV_DIMENSION_BUFFER,
        .Buffer = {
            .FirstElement = 0,
            .NumElements = ROBUST_SCENE_DESCRIPTOR_COUNT,
        },
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateShaderResourceView(
            p->device,
            (ID3D11Resource *)p->nvof.robust_scene_descriptor_buffer,
            &srv_desc, &p->nvof.robust_scene_descriptor_srv);
    }

    const uint32_t zero_state[ROBUST_SCENE_STATE_COUNT] = {0};
    D3D11_SUBRESOURCE_DATA state_data = { .pSysMem = zero_state };
    D3D11_BUFFER_DESC state_desc = {
        .ByteWidth = ROBUST_SCENE_STATE_COUNT * sizeof(uint32_t),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_UNORDERED_ACCESS |
                     D3D11_BIND_SHADER_RESOURCE,
        .MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
        .StructureByteStride = sizeof(uint32_t),
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateBuffer(
            p->device, &state_desc, &state_data,
            &p->nvof.robust_scene_state_buffer);
    }
    uav_desc.Buffer.NumElements = ROBUST_SCENE_STATE_COUNT;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device,
            (ID3D11Resource *)p->nvof.robust_scene_state_buffer,
            &uav_desc, &p->nvof.robust_scene_state_uav);
    }
    srv_desc.Buffer.NumElements = ROBUST_SCENE_STATE_COUNT;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateShaderResourceView(
            p->device,
            (ID3D11Resource *)p->nvof.robust_scene_state_buffer,
            &srv_desc, &p->nvof.robust_scene_state_srv);
    }

    const uint32_t zero_summary[ROBUST_SCENE_SUMMARY_COUNT] = {0};
    D3D11_SUBRESOURCE_DATA summary_data = { .pSysMem = zero_summary };
    D3D11_BUFFER_DESC summary_desc = {
        .ByteWidth = ROBUST_SCENE_SUMMARY_COUNT * sizeof(uint32_t),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_UNORDERED_ACCESS,
        .MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
        .StructureByteStride = sizeof(uint32_t),
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateBuffer(
            p->device, &summary_desc, &summary_data,
            &p->nvof.robust_scene_summary_buffer);
    }
    uav_desc.Buffer.NumElements = ROBUST_SCENE_SUMMARY_COUNT;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device,
            (ID3D11Resource *)p->nvof.robust_scene_summary_buffer,
            &uav_desc, &p->nvof.robust_scene_summary_uav);
    }
    D3D11_BUFFER_DESC readback_desc = {
        .ByteWidth = summary_desc.ByteWidth,
        .Usage = D3D11_USAGE_STAGING,
        .CPUAccessFlags = D3D11_CPU_ACCESS_READ,
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateBuffer(
            p->device, &readback_desc, NULL,
            &p->nvof.robust_scene_summary_readback);
    }
    const uint32_t zero_synthesis_summary[
        ROBUST_SYNTHESIS_COUNTER_COUNT] = {0};
    D3D11_SUBRESOURCE_DATA synthesis_summary_data = {
        .pSysMem = zero_synthesis_summary,
    };
    D3D11_BUFFER_DESC synthesis_summary_desc = {
        .ByteWidth = ROBUST_SYNTHESIS_COUNTER_COUNT * sizeof(uint32_t),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_UNORDERED_ACCESS,
        .MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
        .StructureByteStride = sizeof(uint32_t),
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateBuffer(
            p->device, &synthesis_summary_desc, &synthesis_summary_data,
            &p->nvof.robust_synthesis_summary_buffer);
    }
    uav_desc.Buffer.NumElements = ROBUST_SYNTHESIS_COUNTER_COUNT;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device,
            (ID3D11Resource *)p->nvof.robust_synthesis_summary_buffer,
            &uav_desc, &p->nvof.robust_synthesis_summary_uav);
    }
    readback_desc.ByteWidth = synthesis_summary_desc.ByteWidth;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateBuffer(
            p->device, &readback_desc, NULL,
            &p->nvof.robust_synthesis_summary_readback);
    }
    const struct robust_temporal_constants zero_temporal_constants = {0};
    D3D11_SUBRESOURCE_DATA temporal_constants_data = {
        .pSysMem = &zero_temporal_constants,
    };
    D3D11_BUFFER_DESC temporal_constants_desc = {
        .ByteWidth = sizeof(struct robust_temporal_constants),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_CONSTANT_BUFFER,
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateBuffer(
            p->device, &temporal_constants_desc, &temporal_constants_data,
            &p->nvof.robust_temporal_constants_buffer);
    }
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF robust scene buffer creation failed hr=0x%08lx\n",
               (unsigned long)hr);
        release_robust_scene_resources(p);
        return false;
    }

    MP_INFO(f, "NVOF robust scene classifier ready regions=3x3 "
               "histogram-bins=%d sample-stride=%d descriptors="
               "luma+chroma+edge history=hysteresis "
               "synthesis=projected-ownership+raw-median-block-"
               "edge-vector-hermite\n",
            ROBUST_SCENE_HISTOGRAM_BINS,
            p->opts->scene_cut_sample_stride);
    return true;
}

static bool load_flow_diagnostics_shader(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->nvof.flow_diagnostics_shader &&
        p->nvof.flow_diagnostics_buffer &&
        p->nvof.flow_diagnostics_readback &&
        p->nvof.flow_diagnostics_uav)
        return true;
    release_flow_diagnostics(p);
    if (!p->nvof.d3d_compile) {
        MP_ERR(f, "NVOF flow diagnostics require the D3DCompiler runtime\n");
        return false;
    }

    ID3DBlob *bytecode = NULL;
    ID3DBlob *errors = NULL;
    char confidence_min[32];
    format_shader_number(confidence_min, sizeof(confidence_min),
                         p->opts->flow_confidence_min);
    D3D_SHADER_MACRO macros[] = {
        {"USE_FILLED_FLOW",
         p->opts->stage5_flow_infill_test ? "1" : "0"},
        {"FLOW_CONFIDENCE_MIN", confidence_min},
        {NULL, NULL},
    };
    HRESULT hr = p->nvof.d3d_compile(
        flow_diagnostics_shader_source,
        sizeof(flow_diagnostics_shader_source) - 1,
        "flow_diagnostics.hlsl", macros, NULL, "main", "cs_5_0",
        D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_WARNINGS_ARE_ERRORS,
        0, &bytecode, &errors);
    if (FAILED(hr)) {
        const char *message = errors
            ? ID3D10Blob_GetBufferPointer(errors) : "no compiler diagnostics";
        MP_ERR(f, "NVOF flow diagnostics shader compilation failed "
                  "hr=0x%08lx detail=%s\n", (unsigned long)hr, message);
        if (errors)
            ID3D10Blob_Release(errors);
        if (bytecode)
            ID3D10Blob_Release(bytecode);
        return false;
    }
    if (errors)
        ID3D10Blob_Release(errors);
    hr = ID3D11Device_CreateComputeShader(
        p->device, ID3D10Blob_GetBufferPointer(bytecode),
        ID3D10Blob_GetBufferSize(bytecode), NULL,
        &p->nvof.flow_diagnostics_shader);
    ID3D10Blob_Release(bytecode);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF could not create flow diagnostics compute shader "
                  "hr=0x%08lx\n", (unsigned long)hr);
        release_flow_diagnostics(p);
        return false;
    }

    D3D11_BUFFER_DESC counter_desc = {
        .ByteWidth = FLOW_DIAG_COUNTER_COUNT * sizeof(uint32_t),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_UNORDERED_ACCESS,
        .MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
        .StructureByteStride = sizeof(uint32_t),
    };
    hr = ID3D11Device_CreateBuffer(
        p->device, &counter_desc, NULL, &p->nvof.flow_diagnostics_buffer);
    D3D11_UNORDERED_ACCESS_VIEW_DESC uav_desc = {
        .Format = DXGI_FORMAT_UNKNOWN,
        .ViewDimension = D3D11_UAV_DIMENSION_BUFFER,
        .Buffer = {
            .FirstElement = 0,
            .NumElements = FLOW_DIAG_COUNTER_COUNT,
        },
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device,
            (ID3D11Resource *)p->nvof.flow_diagnostics_buffer,
            &uav_desc, &p->nvof.flow_diagnostics_uav);
    }
    D3D11_BUFFER_DESC readback_desc = {
        .ByteWidth = counter_desc.ByteWidth,
        .Usage = D3D11_USAGE_STAGING,
        .CPUAccessFlags = D3D11_CPU_ACCESS_READ,
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateBuffer(
            p->device, &readback_desc, NULL,
            &p->nvof.flow_diagnostics_readback);
    }
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF flow diagnostics buffer creation failed "
                  "hr=0x%08lx\n", (unsigned long)hr);
        release_flow_diagnostics(p);
        return false;
    }
    MP_WARN(f, "NVOF flow diagnostics ready counters=%d readback-bytes=%u "
               "final-flow-state=%s; this development mode introduces a "
               "per-pair GPU sync\n",
            FLOW_DIAG_COUNTER_COUNT, counter_desc.ByteWidth,
            p->opts->stage5_flow_infill_test ? "yes" : "no");
    return true;
}

static bool load_flow_infill_shaders(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->nvof.prepare_flow_shader && p->nvof.infill_flow_shader)
        return true;
    if (!p->nvof.d3d_compile) {
        MP_ERR(f, "NVOF flow infill requires the D3DCompiler runtime\n");
        return false;
    }
    if (p->nvof.prepare_flow_shader)
        ID3D11ComputeShader_Release(p->nvof.prepare_flow_shader);
    p->nvof.prepare_flow_shader = NULL;
    if (p->nvof.infill_flow_shader)
        ID3D11ComputeShader_Release(p->nvof.infill_flow_shader);
    p->nvof.infill_flow_shader = NULL;

    const char *sources[] = {
        prepare_flow_shader_source,
        infill_flow_shader_source,
    };
    const SIZE_T source_sizes[] = {
        sizeof(prepare_flow_shader_source) - 1,
        sizeof(infill_flow_shader_source) - 1,
    };
    const char *names[] = {
        "prepare_flow.hlsl",
        "infill_flow.hlsl",
    };
    ID3D11ComputeShader **outputs[] = {
        &p->nvof.prepare_flow_shader,
        &p->nvof.infill_flow_shader,
    };
    char fb_abs[32];
    char fb_rel[32];
    char cost_max[32];
    char confidence_min[32];
    char infill_luma_threshold[32];
    format_shader_number(fb_abs, sizeof(fb_abs), p->opts->flow_fb_abs);
    format_shader_number(fb_rel, sizeof(fb_rel), p->opts->flow_fb_rel);
    format_shader_number(cost_max, sizeof(cost_max), p->opts->flow_cost_max);
    format_shader_number(confidence_min, sizeof(confidence_min),
                         p->opts->flow_confidence_min);
    format_shader_number(infill_luma_threshold,
                         sizeof(infill_luma_threshold),
                         p->opts->infill_luma_threshold);
    D3D_SHADER_MACRO macros[] = {
        {"FLOW_FB_ABS", fb_abs},
        {"FLOW_FB_REL", fb_rel},
        {"FLOW_COST_MAX", cost_max},
        {"FLOW_CONFIDENCE_MIN", confidence_min},
        {"INFILL_LUMA_THRESHOLD", infill_luma_threshold},
        {NULL, NULL},
    };
    for (int n = 0; n < MP_ARRAY_SIZE(sources); n++) {
        ID3DBlob *bytecode = NULL;
        ID3DBlob *errors = NULL;
        HRESULT hr = p->nvof.d3d_compile(
            sources[n], source_sizes[n], names[n], macros, NULL,
            "main", "cs_5_0",
            D3DCOMPILE_OPTIMIZATION_LEVEL3 |
            D3DCOMPILE_WARNINGS_ARE_ERRORS,
            0, &bytecode, &errors);
        if (FAILED(hr)) {
            const char *message = errors
                ? ID3D10Blob_GetBufferPointer(errors)
                : "no compiler diagnostics";
            MP_ERR(f, "NVOF flow infill shader compilation failed "
                      "shader=%s hr=0x%08lx detail=%s\n",
                   names[n], (unsigned long)hr, message);
            if (errors)
                ID3D10Blob_Release(errors);
            if (bytecode)
                ID3D10Blob_Release(bytecode);
            return false;
        }
        if (errors)
            ID3D10Blob_Release(errors);
        hr = ID3D11Device_CreateComputeShader(
            p->device, ID3D10Blob_GetBufferPointer(bytecode),
            ID3D10Blob_GetBufferSize(bytecode), NULL, outputs[n]);
        ID3D10Blob_Release(bytecode);
        if (FAILED(hr)) {
            MP_ERR(f, "NVOF could not create flow infill compute shader "
                      "shader=%s hr=0x%08lx\n",
                   names[n], (unsigned long)hr);
            return false;
        }
    }
    MP_INFO(f, "NVOF flow-domain infill shaders ready fb-threshold="
               "%.3f+%.3f*flow cost-max=%.3f confidence-min=%.3f "
               "luma-delta-max=%.2f passes=%d\n",
            p->opts->flow_fb_abs, p->opts->flow_fb_rel,
            p->opts->flow_cost_max, p->opts->flow_confidence_min,
            p->opts->infill_luma_threshold, FLOW_INFILL_PASS_COUNT);
    return true;
}

static bool load_synthesize_p010_shader(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->nvof.synthesize_p010_shader)
        return true;
    if (!p->nvof.d3d_compile) {
        MP_ERR(f, "NVOF P010 synthesis requires the D3DCompiler runtime\n");
        return false;
    }

    ID3DBlob *bytecode = NULL;
    ID3DBlob *errors = NULL;
    char confidence_min[32];
    char scene_average_threshold[32];
    char scene_changed_ratio[32];
    format_shader_number(confidence_min, sizeof(confidence_min),
                         p->opts->flow_confidence_min);
    format_shader_number(scene_average_threshold,
                         sizeof(scene_average_threshold),
                         p->opts->scene_cut_average_threshold);
    format_shader_number(scene_changed_ratio, sizeof(scene_changed_ratio),
                         p->opts->scene_cut_changed_ratio);
    D3D_SHADER_MACRO macros[] = {
        {"USE_FILLED_FLOW",
         p->opts->stage5_flow_infill_test ? "1" : "0"},
        {"USE_ROBUST_SCENE",
         p->opts->stage6_robust_test ? "1" : "0"},
        {"FLOW_CONFIDENCE_MIN", confidence_min},
        {"SCENE_AVERAGE_THRESHOLD", scene_average_threshold},
        {"SCENE_CHANGED_RATIO", scene_changed_ratio},
        {"ROBUST_STATE_CLASS", "0"},
        {"ROBUST_SCENE_NORMAL", "0"},
        {"ROBUST_SCENE_FLASH", "1"},
        {"ROBUST_SCENE_FADE_DISSOLVE", "2"},
        {"ROBUST_SCENE_HARD_CUT", "3"},
        {"ROBUST_SCENE_UNCERTAIN", "4"},
        {"ROBUST_SYNTHESIS_COUNTER_COUNT", "19"},
        {NULL, NULL},
    };
    HRESULT hr = p->nvof.d3d_compile(
        synthesize_p010_shader_source,
        sizeof(synthesize_p010_shader_source) - 1,
        "synthesize_p010.hlsl", macros, NULL, "main", "cs_5_0",
        D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_WARNINGS_ARE_ERRORS,
        0, &bytecode, &errors);
    if (FAILED(hr)) {
        const char *message = errors
            ? ID3D10Blob_GetBufferPointer(errors) : "no compiler diagnostics";
        MP_ERR(f, "NVOF P010 synthesis shader compilation failed "
                  "hr=0x%08lx detail=%s\n", (unsigned long)hr, message);
        if (errors)
            ID3D10Blob_Release(errors);
        if (bytecode)
            ID3D10Blob_Release(bytecode);
        return false;
    }
    if (errors)
        ID3D10Blob_Release(errors);
    hr = ID3D11Device_CreateComputeShader(
        p->device, ID3D10Blob_GetBufferPointer(bytecode),
        ID3D10Blob_GetBufferSize(bytecode), NULL,
        &p->nvof.synthesize_p010_shader);
    ID3D10Blob_Release(bytecode);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF could not create P010 synthesis compute shader "
                  "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }
    MP_INFO(f, "NVOF P010 synthesis shader ready inputs=Y/UV+%s "
               "scene=%s output=P010 midpoint=0.5\n",
            p->opts->stage5_flow_infill_test
                ? "filled-flow-confidence" : "flow+cost",
            p->opts->stage6_robust_test
                ? "robust-3x3-histogram" : "legacy-threshold");
    return true;
}

static bool nvof_cap_contains(struct mp_filter *f, NV_OF_CAPS capability,
                              uint32_t expected)
{
    struct priv *p = f->priv;
    uint32_t count = 0;
    if (!check_nvof_status(f, "nvOFGetCaps(count)",
                           p->nvof.api.nvOFGetCaps(
                               p->nvof.handle, capability, NULL, &count)))
        return false;
    uint32_t *values = count ? talloc_array(NULL, uint32_t, count) : NULL;
    if (count && !values) {
        MP_ERR(f, "NVOF capability allocation failed count=%u\n", count);
        return false;
    }
    bool found = false;
    if (!count || check_nvof_status(f, "nvOFGetCaps(values)",
            p->nvof.api.nvOFGetCaps(
                p->nvof.handle, capability, values, &count))) {
        for (uint32_t n = 0; n < count; n++)
            found |= values[n] == expected;
    }
    talloc_free(values);
    return found;
}

static bool nvof_cap_at_least(struct mp_filter *f, NV_OF_CAPS capability,
                              uint32_t required)
{
    struct priv *p = f->priv;
    uint32_t count = 0;
    if (!check_nvof_status(f, "nvOFGetCaps(limit-count)",
                           p->nvof.api.nvOFGetCaps(
                               p->nvof.handle, capability, NULL, &count)))
        return false;
    uint32_t *values = count ? talloc_array(NULL, uint32_t, count) : NULL;
    if (count && !values) {
        MP_ERR(f, "NVOF limit allocation failed count=%u\n", count);
        return false;
    }
    uint32_t maximum = 0;
    if (!count || check_nvof_status(f, "nvOFGetCaps(limit-values)",
            p->nvof.api.nvOFGetCaps(
                p->nvof.handle, capability, values, &count))) {
        for (uint32_t n = 0; n < count; n++)
            maximum = MPMAX(maximum, values[n]);
    }
    talloc_free(values);
    return maximum >= required;
}

static bool nvof_format_supported(struct mp_filter *f,
                                  NV_OF_BUFFER_USAGE usage,
                                  DXGI_FORMAT expected)
{
    struct priv *p = f->priv;
    uint32_t count = 0;
    if (!check_nvof_status(f, "nvOFGetSurfaceFormatCountD3D11",
            p->nvof.api.nvOFGetSurfaceFormatCountD3D11(
                p->nvof.handle, usage, NV_OF_MODE_OPTICALFLOW, &count)))
        return false;
    DXGI_FORMAT *formats = count
        ? talloc_array(NULL, DXGI_FORMAT, count) : NULL;
    if (count && !formats) {
        MP_ERR(f, "NVOF format allocation failed count=%u\n", count);
        return false;
    }
    bool found = false;
    if (!count || check_nvof_status(f, "nvOFGetSurfaceFormatD3D11",
            p->nvof.api.nvOFGetSurfaceFormatD3D11(
                p->nvof.handle, usage, NV_OF_MODE_OPTICALFLOW, formats))) {
        for (uint32_t n = 0; n < count; n++)
            found |= formats[n] == expected;
    }
    talloc_free(formats);
    return found;
}

static bool create_nvof_resource(struct mp_filter *f,
                                 struct nvof_resource *resource,
                                 UINT width, UINT height, DXGI_FORMAT format,
                                 UINT bind_flags, bool create_uav,
                                 bool create_srv)
{
    struct priv *p = f->priv;
    D3D11_TEXTURE2D_DESC desc = {
        .Width = width,
        .Height = height,
        .MipLevels = 1,
        .ArraySize = 1,
        .Format = format,
        .SampleDesc = { .Count = 1 },
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = bind_flags,
    };
    HRESULT hr = ID3D11Device_CreateTexture2D(
        p->device, &desc, NULL, &resource->texture);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF resource creation failed format=%u size=%ux%u "
                  "bind-flags=0x%x hr=0x%08lx\n",
               (unsigned)format, width, height, bind_flags,
               (unsigned long)hr);
        return false;
    }
    if (create_uav) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device, (ID3D11Resource *)resource->texture, NULL,
            &resource->uav);
        if (FAILED(hr)) {
            MP_ERR(f, "NVOF resource UAV creation failed format=%u "
                      "hr=0x%08lx\n", (unsigned)format, (unsigned long)hr);
            return false;
        }
    }
    if (create_srv) {
        hr = ID3D11Device_CreateShaderResourceView(
            p->device, (ID3D11Resource *)resource->texture, NULL,
            &resource->srv);
        if (FAILED(hr)) {
            MP_ERR(f, "NVOF resource SRV creation failed format=%u "
                      "hr=0x%08lx\n", (unsigned)format, (unsigned long)hr);
            return false;
        }
    }
    if (!check_nvof_status(f, "nvOFRegisterResourceD3D11",
            p->nvof.api.nvOFRegisterResourceD3D11(
                p->nvof.handle, (ID3D11Resource *)resource->texture,
                &resource->handle)))
        return false;
    return true;
}

static bool create_flow_state_resource(struct mp_filter *f,
                                       struct nvof_resource *resource,
                                       UINT width, UINT height)
{
    struct priv *p = f->priv;
    D3D11_TEXTURE2D_DESC desc = {
        .Width = width,
        .Height = height,
        .MipLevels = 1,
        .ArraySize = 1,
        .Format = DXGI_FORMAT_R16G16B16A16_FLOAT,
        .SampleDesc = { .Count = 1 },
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_SHADER_RESOURCE |
                     D3D11_BIND_UNORDERED_ACCESS,
    };
    HRESULT hr = ID3D11Device_CreateTexture2D(
        p->device, &desc, NULL, &resource->texture);
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device, (ID3D11Resource *)resource->texture,
            NULL, &resource->uav);
    }
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateShaderResourceView(
            p->device, (ID3D11Resource *)resource->texture,
            NULL, &resource->srv);
    }
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF flow-state resource creation failed "
                  "format=R16G16B16A16_FLOAT size=%ux%u hr=0x%08lx\n",
               width, height, (unsigned long)hr);
        return false;
    }
    return true;
}

static bool create_shader_texture_resource(
    struct mp_filter *f, struct shader_texture_resource *resource,
    UINT width, UINT height, DXGI_FORMAT format, UINT bind_flags)
{
    struct priv *p = f->priv;
    D3D11_TEXTURE2D_DESC desc = {
        .Width = width,
        .Height = height,
        .MipLevels = 1,
        .ArraySize = 1,
        .Format = format,
        .SampleDesc = { .Count = 1 },
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = bind_flags,
    };
    HRESULT hr = ID3D11Device_CreateTexture2D(
        p->device, &desc, NULL, &resource->texture);
    if (SUCCEEDED(hr) && (bind_flags & D3D11_BIND_UNORDERED_ACCESS)) {
        hr = ID3D11Device_CreateUnorderedAccessView(
            p->device, (ID3D11Resource *)resource->texture,
            NULL, &resource->uav);
    }
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateShaderResourceView(
            p->device, (ID3D11Resource *)resource->texture,
            NULL, &resource->srv);
    }
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF shader texture creation failed format=%u "
                  "size=%ux%u hr=0x%08lx\n",
               (unsigned)format, width, height, (unsigned long)hr);
        return false;
    }
    return true;
}

static bool create_robust_pair_cache(struct mp_filter *f,
                                     struct robust_pair_cache *cache,
                                     UINT width, UINT height)
{
    struct priv *p = f->priv;
    bool textures_ok = create_shader_texture_resource(
        f, &cache->flow_forward, width, height, DXGI_FORMAT_R16G16_SINT,
        D3D11_BIND_SHADER_RESOURCE) &&
        create_shader_texture_resource(
            f, &cache->flow_backward, width, height,
            DXGI_FORMAT_R16G16_SINT, D3D11_BIND_SHADER_RESOURCE) &&
        create_shader_texture_resource(
            f, &cache->cost_forward, width, height, DXGI_FORMAT_R8_UINT,
            D3D11_BIND_SHADER_RESOURCE) &&
        create_shader_texture_resource(
            f, &cache->cost_backward, width, height, DXGI_FORMAT_R8_UINT,
            D3D11_BIND_SHADER_RESOURCE);
    if (!textures_ok) {
        release_robust_pair_cache(cache);
        return false;
    }

    const uint32_t zero_state[ROBUST_SCENE_STATE_COUNT] = {0};
    D3D11_SUBRESOURCE_DATA initial_data = {
        .pSysMem = zero_state,
    };
    D3D11_BUFFER_DESC buffer_desc = {
        .ByteWidth = ROBUST_SCENE_STATE_COUNT * sizeof(uint32_t),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_SHADER_RESOURCE,
        .MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED,
        .StructureByteStride = sizeof(uint32_t),
    };
    HRESULT hr = ID3D11Device_CreateBuffer(
        p->device, &buffer_desc, &initial_data, &cache->scene_state_buffer);
    D3D11_SHADER_RESOURCE_VIEW_DESC srv_desc = {
        .Format = DXGI_FORMAT_UNKNOWN,
        .ViewDimension = D3D11_SRV_DIMENSION_BUFFER,
        .Buffer = {
            .FirstElement = 0,
            .NumElements = ROBUST_SCENE_STATE_COUNT,
        },
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device_CreateShaderResourceView(
            p->device, (ID3D11Resource *)cache->scene_state_buffer,
            &srv_desc, &cache->scene_state_srv);
    }
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF robust temporal scene cache creation failed "
                  "hr=0x%08lx\n", (unsigned long)hr);
        release_robust_pair_cache(cache);
        return false;
    }
    cache->valid = false;
    return true;
}

static bool create_nvof_session(struct mp_filter *f, int width, int height)
{
    struct priv *p = f->priv;
    release_nvof_session(f);
    if (p->opts->stage6_robust_test && (width > 4096 || height > 4096)) {
        MP_ERR(f, "NVOF projected ownership packing supports at most "
                  "4096x4096 input, received=%dx%d\n", width, height);
        return false;
    }
    if (!check_nvof_status(f, "nvCreateOpticalFlowD3D11",
            p->nvof.api.nvCreateOpticalFlowD3D11(
                p->device, p->context, &p->nvof.handle)))
        return false;

    bool capabilities_ok =
        nvof_cap_contains(f, NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES, 1) &&
        nvof_cap_at_least(f, NV_OF_CAPS_WIDTH_MAX, width) &&
        nvof_cap_at_least(f, NV_OF_CAPS_HEIGHT_MAX, height) &&
        nvof_format_supported(f, NV_OF_BUFFER_USAGE_INPUT,
                              DXGI_FORMAT_R8_UNORM) &&
        nvof_format_supported(f, NV_OF_BUFFER_USAGE_OUTPUT,
                              DXGI_FORMAT_R16G16_SINT) &&
        nvof_format_supported(f, NV_OF_BUFFER_USAGE_COST,
                              DXGI_FORMAT_R8_UINT);
    if (!capabilities_ok) {
        MP_ERR(f, "NVOF strict MEMC capabilities are unavailable "
                  "resolution=%dx%d grid=1 gray8=yes flow=s16x2 "
                  "cost=u8\n", width, height);
        release_nvof_session(f);
        return false;
    }

    NV_OF_INIT_PARAMS init = {
        .width = width,
        .height = height,
        .outGridSize = NV_OF_OUTPUT_VECTOR_GRID_SIZE_1,
        .hintGridSize = NV_OF_HINT_VECTOR_GRID_SIZE_UNDEFINED,
        .mode = NV_OF_MODE_OPTICALFLOW,
        .perfLevel = NV_OF_PERF_LEVEL_SLOW,
        .enableExternalHints = NV_OF_FALSE,
        .enableOutputCost = NV_OF_TRUE,
        .disparityRange = NV_OF_STEREO_DISPARITY_RANGE_UNDEFINED,
        .enableRoi = NV_OF_FALSE,
        .predDirection = NV_OF_PRED_DIRECTION_BOTH,
        .enableGlobalFlow = NV_OF_FALSE,
        .inputBufferFormat = NV_OF_BUFFER_FORMAT_GRAYSCALE8,
    };
    if (!check_nvof_status(f, "nvOFInit(strict-memc)",
                           p->nvof.api.nvOFInit(p->nvof.handle, &init))) {
        release_nvof_session(f);
        return false;
    }

    UINT gray_bind = D3D11_BIND_SHADER_RESOURCE |
                     D3D11_BIND_UNORDERED_ACCESS |
                     D3D11_BIND_RENDER_TARGET;
    bool resources_ok =
        create_nvof_resource(f, &p->nvof.gray[0], width, height,
                             DXGI_FORMAT_R8_UNORM, gray_bind,
                              true, p->opts->flow_diagnostics ||
                                    nvof_synthesis_enabled(p)) &&
        create_nvof_resource(f, &p->nvof.gray[1], width, height,
                             DXGI_FORMAT_R8_UNORM, gray_bind,
                              true, p->opts->flow_diagnostics ||
                                    nvof_synthesis_enabled(p)) &&
        create_nvof_resource(f, &p->nvof.flow_forward, width, height,
                             DXGI_FORMAT_R16G16_SINT,
                             D3D11_BIND_SHADER_RESOURCE,
                             false, true) &&
        create_nvof_resource(f, &p->nvof.cost_forward, width, height,
                             DXGI_FORMAT_R8_UINT,
                             D3D11_BIND_SHADER_RESOURCE,
                             false, true) &&
        create_nvof_resource(f, &p->nvof.flow_backward, width, height,
                             DXGI_FORMAT_R16G16_SINT,
                             D3D11_BIND_SHADER_RESOURCE,
                             false, true) &&
        create_nvof_resource(f, &p->nvof.cost_backward, width, height,
                             DXGI_FORMAT_R8_UINT,
                             D3D11_BIND_SHADER_RESOURCE,
                             false, true);
    if (resources_ok && p->opts->stage5_flow_infill_test) {
        resources_ok =
            create_flow_state_resource(
                f, &p->nvof.flow_state_forward[0], width, height) &&
            create_flow_state_resource(
                f, &p->nvof.flow_state_forward[1], width, height) &&
            create_flow_state_resource(
                f, &p->nvof.flow_state_backward[0], width, height) &&
            create_flow_state_resource(
                f, &p->nvof.flow_state_backward[1], width, height);
    }
    if (resources_ok && p->opts->stage6_robust_test) {
        resources_ok = create_shader_texture_resource(
            f, &p->nvof.occupancy[0], width, height,
            DXGI_FORMAT_R32_UINT,
            D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS) &&
            create_shader_texture_resource(
                f, &p->nvof.occupancy[1], width, height,
                DXGI_FORMAT_R32_UINT,
                D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS) &&
            create_shader_texture_resource(
                f, &p->nvof.projected_owner[0], width, height,
                DXGI_FORMAT_R32_UINT,
                D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS) &&
            create_shader_texture_resource(
                f, &p->nvof.projected_owner[1], width, height,
                DXGI_FORMAT_R32_UINT,
                D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS);
        for (int n = 0;
             resources_ok && n < MP_ARRAY_SIZE(p->nvof.pair_cache); n++) {
            resources_ok = create_robust_pair_cache(
                f, &p->nvof.pair_cache[n], width, height);
        }
    }
    if (resources_ok && p->opts->nvof_completion_diagnostics) {
        D3D11_TEXTURE2D_DESC readback_desc = {
            .Width = width,
            .Height = height,
            .MipLevels = 1,
            .ArraySize = 1,
            .Format = DXGI_FORMAT_R16G16_SINT,
            .SampleDesc = { .Count = 1 },
            .Usage = D3D11_USAGE_STAGING,
            .CPUAccessFlags = D3D11_CPU_ACCESS_READ,
        };
        HRESULT hr = ID3D11Device_CreateTexture2D(
            p->device, &readback_desc, NULL,
            &p->nvof.completion_readback);
        if (FAILED(hr)) {
            MP_ERR(f, "NVOF completion diagnostic staging texture creation "
                       "failed "
                       "hr=0x%08lx\n", (unsigned long)hr);
            resources_ok = false;
        }
    }
    bool scene_shaders_ok = resources_ok && (!nvof_synthesis_enabled(p) ||
        (p->opts->stage6_robust_test
            ? load_robust_scene_shaders(f)
            : load_scene_cut_shader(f)));
    bool shaders_ok = scene_shaders_ok && load_extract_luma_shader(f) &&
        scene_shaders_ok &&
        (!p->opts->flow_diagnostics ||
         load_flow_diagnostics_shader(f)) &&
        (!p->opts->stage5_flow_infill_test ||
         load_flow_infill_shaders(f)) &&
        (!(p->opts->stage4_synthesis_test ||
           p->opts->stage5_flow_infill_test ||
           p->opts->stage6_robust_test) ||
         load_synthesize_p010_shader(f));
    if (!shaders_ok) {
        release_nvof_session(f);
        return false;
    }

    p->nvof.width = width;
    p->nvof.height = height;
    p->nvof.disable_temporal_hints_next = true;
    MP_INFO(f, "NVOF session initialized resolution=%dx%d grid=1 "
               "preset=slow direction=both cost=uint8 global-flow=no\n",
            width, height);
    return true;
}

static bool ensure_nvof_session(struct mp_filter *f, int width, int height)
{
    struct priv *p = f->priv;
    if (p->nvof.handle && p->nvof.width == width &&
        p->nvof.height == height)
        return true;
    return create_nvof_session(f, width, height);
}

static bool extract_luma(struct mp_filter *f, struct mp_image *frame,
                          int gray_index)
{
    struct priv *p = f->priv;
    ID3D11Texture2D *texture = (ID3D11Texture2D *)frame->planes[0];
    D3D11_TEXTURE2D_DESC texture_desc;
    ID3D11Texture2D_GetDesc(texture, &texture_desc);
    if (texture_desc.Format != DXGI_FORMAT_P010 ||
        texture_desc.ArraySize != 1 ||
        texture_desc.Width < (UINT)frame->w ||
        texture_desc.Height < (UINT)frame->h) {
        MP_ERR(f, "NVOF luma extraction requires a private single-layer P010 "
                  "texture received-format=%u array-size=%u size=%ux%u\n",
               (unsigned)texture_desc.Format, texture_desc.ArraySize,
               texture_desc.Width, texture_desc.Height);
        return false;
    }

    ID3D11Device3 *device3 = NULL;
    HRESULT hr = ID3D11Device_QueryInterface(
        p->device, &IID_ID3D11Device3, (void **)&device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF luma extraction requires ID3D11Device3 "
                  "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }
    D3D11_SHADER_RESOURCE_VIEW_DESC1 srv_desc = {
        .Format = DXGI_FORMAT_R16_UNORM,
        .ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D,
        .Texture2D = {
            .MostDetailedMip = 0,
            .MipLevels = 1,
            .PlaneSlice = 0,
        },
    };
    ID3D11ShaderResourceView1 *source_srv1 = NULL;
    hr = ID3D11Device3_CreateShaderResourceView1(
        device3, (ID3D11Resource *)texture, &srv_desc, &source_srv1);
    ID3D11Device3_Release(device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF could not create P010 Y SRV hr=0x%08lx\n",
               (unsigned long)hr);
        return false;
    }

    ID3D11ShaderResourceView *source_srv =
        (ID3D11ShaderResourceView *)source_srv1;
    ID3D11UnorderedAccessView *gray_uav = p->nvof.gray[gray_index].uav;
    lock_d3d11_context(p);
    struct gpu_profile_token profile = gpu_profile_begin_locked(
        p, GPU_PROFILE_LUMA_EXTRACT);
    ID3D11DeviceContext_CSSetShader(
        p->context, p->nvof.extract_luma_shader, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, 1, &source_srv);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, 1, &gray_uav, NULL);
    ID3D11DeviceContext_Dispatch(
        p->context, (frame->w + 15) / 16, (frame->h + 15) / 16, 1);

    ID3D11ShaderResourceView *null_srv = NULL;
    ID3D11UnorderedAccessView *null_uav = NULL;
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, 1, &null_srv);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, 1, &null_uav, NULL);
    ID3D11DeviceContext_CSSetShader(p->context, NULL, NULL, 0);
    gpu_profile_end_locked(p, profile);
    unlock_d3d11_context(p);
    ID3D11ShaderResourceView1_Release(source_srv1);
    return true;
}

static bool dispatch_scene_cut_analysis(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!p->nvof.scene_cut_shader || !p->nvof.scene_cut_buffer ||
        !p->nvof.scene_cut_uav || !p->nvof.scene_cut_srv ||
        !p->nvof.gray[0].srv || !p->nvof.gray[1].srv) {
        MP_ERR(f, "NVOF scene-cut detection resources are incomplete\n");
        return false;
    }

    uint32_t counters[SCENE_CUT_COUNTER_COUNT] = {0};
    lock_d3d11_context(p);
    struct gpu_profile_token profile = gpu_profile_begin_locked(
        p, GPU_PROFILE_SCENE_CUT);
    ID3D11DeviceContext_UpdateSubresource(
        p->context, (ID3D11Resource *)p->nvof.scene_cut_buffer,
        0, NULL, counters, 0, 0);
    ID3D11ShaderResourceView *srvs[] = {
        p->nvof.gray[0].srv,
        p->nvof.gray[1].srv,
    };
    ID3D11UnorderedAccessView *uav = p->nvof.scene_cut_uav;
    ID3D11DeviceContext_CSSetShader(
        p->context, p->nvof.scene_cut_shader, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(srvs), srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, 1, &uav, NULL);
    UINT sampled_width = (p->nvof.width +
        p->opts->scene_cut_sample_stride - 1) /
        p->opts->scene_cut_sample_stride;
    UINT sampled_height = (p->nvof.height +
        p->opts->scene_cut_sample_stride - 1) /
        p->opts->scene_cut_sample_stride;
    ID3D11DeviceContext_Dispatch(
        p->context, (sampled_width + 15) / 16,
        (sampled_height + 15) / 16, 1);

    ID3D11ShaderResourceView *null_srvs[MP_ARRAY_SIZE(srvs)] = {0};
    ID3D11UnorderedAccessView *null_uav = NULL;
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(null_srvs), null_srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, 1, &null_uav, NULL);
    ID3D11DeviceContext_CSSetShader(p->context, NULL, NULL, 0);
    gpu_profile_end_locked(p, profile);
    unlock_d3d11_context(p);
    return true;
}

static bool dispatch_robust_scene_analysis(struct mp_filter *f,
                                           struct mp_image *frame0,
                                           struct mp_image *frame1)
{
    struct priv *p = f->priv;
    if (!p->nvof.robust_scene_descriptor_shader ||
        !p->nvof.robust_scene_classify_shader ||
        !p->nvof.robust_scene_descriptor_buffer ||
        !p->nvof.robust_scene_descriptor_uav ||
        !p->nvof.robust_scene_descriptor_srv ||
        !p->nvof.robust_scene_state_buffer ||
        !p->nvof.robust_scene_state_uav ||
        !p->nvof.robust_scene_state_srv ||
        !p->nvof.robust_scene_summary_uav ||
        !p->nvof.gray[0].srv || !p->nvof.gray[1].srv) {
        MP_ERR(f, "NVOF robust scene resources are incomplete\n");
        return false;
    }

    ID3D11Texture2D *textures[] = {
        (ID3D11Texture2D *)frame0->planes[0],
        (ID3D11Texture2D *)frame1->planes[0],
    };
    ID3D11Device3 *device3 = NULL;
    HRESULT hr = ID3D11Device_QueryInterface(
        p->device, &IID_ID3D11Device3, (void **)&device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF robust scene analysis requires ID3D11Device3 "
                  "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }

    ID3D11ShaderResourceView1 *uv_views[2] = {0};
    D3D11_SHADER_RESOURCE_VIEW_DESC1 uv_desc = {
        .Format = DXGI_FORMAT_R16G16_UNORM,
        .ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D,
        .Texture2D = {
            .MostDetailedMip = 0,
            .MipLevels = 1,
            .PlaneSlice = 1,
        },
    };
    hr = ID3D11Device3_CreateShaderResourceView1(
        device3, (ID3D11Resource *)textures[0], &uv_desc, &uv_views[0]);
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateShaderResourceView1(
            device3, (ID3D11Resource *)textures[1], &uv_desc,
            &uv_views[1]);
    }
    ID3D11Device3_Release(device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF robust scene UV view creation failed "
                  "hr=0x%08lx\n", (unsigned long)hr);
        for (int n = 0; n < MP_ARRAY_SIZE(uv_views); n++) {
            if (uv_views[n])
                ID3D11ShaderResourceView1_Release(uv_views[n]);
        }
        return false;
    }

    const uint32_t zero_descriptor[ROBUST_SCENE_DESCRIPTOR_COUNT] = {0};
    const uint32_t zero_state[ROBUST_SCENE_STATE_COUNT] = {0};
    lock_d3d11_context(p);
    struct gpu_profile_token profile = gpu_profile_begin_locked(
        p, GPU_PROFILE_SCENE_CUT);
    ID3D11DeviceContext_UpdateSubresource(
        p->context,
        (ID3D11Resource *)p->nvof.robust_scene_descriptor_buffer,
        0, NULL, zero_descriptor, 0, 0);
    if (p->robust_scene_history_reset_pending) {
        ID3D11DeviceContext_UpdateSubresource(
            p->context,
            (ID3D11Resource *)p->nvof.robust_scene_state_buffer,
            0, NULL, zero_state, 0, 0);
        p->robust_scene_history_reset_pending = false;
    }

    ID3D11ShaderResourceView *descriptor_srvs[] = {
        p->nvof.gray[0].srv,
        p->nvof.gray[1].srv,
        (ID3D11ShaderResourceView *)uv_views[0],
        (ID3D11ShaderResourceView *)uv_views[1],
    };
    ID3D11UnorderedAccessView *descriptor_uav =
        p->nvof.robust_scene_descriptor_uav;
    ID3D11DeviceContext_CSSetShader(
        p->context, p->nvof.robust_scene_descriptor_shader, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(descriptor_srvs), descriptor_srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, 1, &descriptor_uav, NULL);
    UINT sampled_width = (p->nvof.width +
        p->opts->scene_cut_sample_stride - 1) /
        p->opts->scene_cut_sample_stride;
    UINT sampled_height = (p->nvof.height +
        p->opts->scene_cut_sample_stride - 1) /
        p->opts->scene_cut_sample_stride;
    ID3D11DeviceContext_Dispatch(
        p->context, (sampled_width + 15) / 16,
        (sampled_height + 15) / 16, 1);

    ID3D11ShaderResourceView *null_descriptor_srvs[
        MP_ARRAY_SIZE(descriptor_srvs)] = {0};
    ID3D11UnorderedAccessView *null_descriptor_uav = NULL;
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(null_descriptor_srvs),
        null_descriptor_srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, 1, &null_descriptor_uav, NULL);

    ID3D11ShaderResourceView *classify_srv =
        p->nvof.robust_scene_descriptor_srv;
    ID3D11UnorderedAccessView *classify_uavs[] = {
        p->nvof.robust_scene_state_uav,
        p->nvof.robust_scene_summary_uav,
    };
    ID3D11DeviceContext_CSSetShader(
        p->context, p->nvof.robust_scene_classify_shader, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, 1, &classify_srv);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(classify_uavs), classify_uavs, NULL);
    ID3D11DeviceContext_Dispatch(p->context, 1, 1, 1);

    ID3D11ShaderResourceView *null_classify_srv = NULL;
    ID3D11UnorderedAccessView *null_classify_uavs[
        MP_ARRAY_SIZE(classify_uavs)] = {0};
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, 1, &null_classify_srv);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(null_classify_uavs),
        null_classify_uavs, NULL);
    ID3D11DeviceContext_CSSetShader(p->context, NULL, NULL, 0);
    gpu_profile_end_locked(p, profile);
    unlock_d3d11_context(p);

    for (int n = 0; n < MP_ARRAY_SIZE(uv_views); n++)
        ID3D11ShaderResourceView1_Release(uv_views[n]);
    return true;
}

static bool read_robust_scene_summary(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!p->nvof.robust_scene_summary_buffer ||
        !p->nvof.robust_scene_summary_readback) {
        if (!p->synthesized_frames)
            return true;
        MP_ERR(f, "NVOF robust scene summary resources are incomplete\n");
        return false;
    }
    lock_d3d11_context(p);
    ID3D11DeviceContext_CopyResource(
        p->context,
        (ID3D11Resource *)p->nvof.robust_scene_summary_readback,
        (ID3D11Resource *)p->nvof.robust_scene_summary_buffer);
    D3D11_MAPPED_SUBRESOURCE mapped = {0};
    HRESULT hr = ID3D11DeviceContext_Map(
        p->context,
        (ID3D11Resource *)p->nvof.robust_scene_summary_readback,
        0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        unlock_d3d11_context(p);
        MP_ERR(f, "NVOF robust scene summary readback failed "
                  "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }
    const uint32_t *values = mapped.pData;
    p->nvof.scene_cut_pairs = values[0];
    for (int n = 0; n < ROBUST_SCENE_CLASS_COUNT; n++) {
        p->nvof.robust_scene_classes[n] = values[n + 1];
        int metric_base = 1 + ROBUST_SCENE_CLASS_COUNT +
                          n * ROBUST_SCENE_METRIC_COUNT;
        for (int metric = 0; metric < ROBUST_SCENE_METRIC_COUNT;
             metric++) {
            p->nvof.robust_scene_metric_totals[n][metric] =
                values[metric_base + metric];
        }
    }
    p->nvof.scene_cuts =
        p->nvof.robust_scene_classes[ROBUST_SCENE_HARD_CUT];
    p->scene_cut_midpoints = p->nvof.scene_cuts;
    ID3D11DeviceContext_Unmap(
        p->context,
        (ID3D11Resource *)p->nvof.robust_scene_summary_readback, 0);
    unlock_d3d11_context(p);
    if (p->nvof.scene_cut_pairs < p->synthesized_frames ||
        p->nvof.scene_cut_pairs > p->synthesized_frames + 1) {
        MP_ERR(f, "NVOF robust scene summary mismatch analyzed=%llu "
                  "synthesized=%llu lookahead-limit=1\n",
               (unsigned long long)p->nvof.scene_cut_pairs,
               (unsigned long long)p->synthesized_frames);
        return false;
    }
    return true;
}

static bool read_robust_synthesis_summary(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!p->nvof.robust_synthesis_summary_buffer ||
        !p->nvof.robust_synthesis_summary_readback) {
        if (!p->synthesized_frames)
            return true;
        MP_ERR(f, "NVOF robust synthesis summary resources are incomplete\n");
        return false;
    }
    lock_d3d11_context(p);
    ID3D11DeviceContext_CopyResource(
        p->context,
        (ID3D11Resource *)p->nvof.robust_synthesis_summary_readback,
        (ID3D11Resource *)p->nvof.robust_synthesis_summary_buffer);
    D3D11_MAPPED_SUBRESOURCE mapped = {0};
    HRESULT hr = ID3D11DeviceContext_Map(
        p->context,
        (ID3D11Resource *)p->nvof.robust_synthesis_summary_readback,
        0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        unlock_d3d11_context(p);
        MP_ERR(f, "NVOF robust synthesis summary readback failed "
                  "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }
    const uint32_t *values = mapped.pData;
    for (int n = 0; n < ROBUST_SYNTHESIS_COUNTER_COUNT; n++)
        p->nvof.robust_synthesis_totals[n] = values[n];
    ID3D11DeviceContext_Unmap(
        p->context,
        (ID3D11Resource *)p->nvof.robust_synthesis_summary_readback, 0);
    unlock_d3d11_context(p);

    uint64_t classified =
        p->nvof.robust_synthesis_totals[ROBUST_SYNTHESIS_SOURCE0] +
        p->nvof.robust_synthesis_totals[ROBUST_SYNTHESIS_SOURCE1] +
        p->nvof.robust_synthesis_totals[ROBUST_SYNTHESIS_BLENDED] +
        p->nvof.robust_synthesis_totals[ROBUST_SYNTHESIS_FALLBACK];
    if (classified != p->nvof.robust_synthesis_totals[
                          ROBUST_SYNTHESIS_PIXELS]) {
        MP_ERR(f, "NVOF robust synthesis summary mismatch pixels=%llu "
                  "classified=%llu\n",
               (unsigned long long)p->nvof.robust_synthesis_totals[
                   ROBUST_SYNTHESIS_PIXELS],
               (unsigned long long)classified);
        return false;
    }
    return true;
}

static bool read_scene_cut_summary(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!nvof_synthesis_enabled(p))
        return true;
    if (p->opts->stage6_robust_test)
        return read_robust_scene_summary(f) &&
               read_robust_synthesis_summary(f);
    if (!p->nvof.scene_cut_summary_buffer ||
        !p->nvof.scene_cut_summary_readback) {
        MP_ERR(f, "NVOF scene-cut summary resources are incomplete\n");
        return false;
    }
    lock_d3d11_context(p);
    ID3D11DeviceContext_CopyResource(
        p->context,
        (ID3D11Resource *)p->nvof.scene_cut_summary_readback,
        (ID3D11Resource *)p->nvof.scene_cut_summary_buffer);
    D3D11_MAPPED_SUBRESOURCE mapped = {0};
    HRESULT hr = ID3D11DeviceContext_Map(
        p->context,
        (ID3D11Resource *)p->nvof.scene_cut_summary_readback,
        0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        unlock_d3d11_context(p);
        MP_ERR(f, "NVOF scene-cut summary readback failed hr=0x%08lx\n",
               (unsigned long)hr);
        return false;
    }
    const uint32_t *values = mapped.pData;
    p->nvof.scene_cut_pairs = values[0];
    p->nvof.scene_cuts = values[1];
    p->scene_cut_midpoints = values[1];
    ID3D11DeviceContext_Unmap(
        p->context,
        (ID3D11Resource *)p->nvof.scene_cut_summary_readback, 0);
    unlock_d3d11_context(p);
    if (p->nvof.scene_cut_pairs != p->synthesized_frames ||
        p->nvof.scene_cuts > p->nvof.scene_cut_pairs) {
        MP_ERR(f, "NVOF scene-cut summary mismatch pairs=%llu cuts=%llu "
                  "synthesized=%llu\n",
               (unsigned long long)p->nvof.scene_cut_pairs,
               (unsigned long long)p->nvof.scene_cuts,
               (unsigned long long)p->synthesized_frames);
        return false;
    }
    return true;
}

static bool run_flow_diagnostics(struct mp_filter *f)
{
    struct priv *p = f->priv;
    bool final_state_ready = !p->opts->stage5_flow_infill_test ||
        (p->nvof.flow_state_forward[FLOW_INFILL_FINAL_INDEX].srv &&
         p->nvof.flow_state_backward[FLOW_INFILL_FINAL_INDEX].srv);
    bool raw_flow_ready = p->nvof.flow_forward.srv &&
                          p->nvof.cost_forward.srv &&
                          p->nvof.flow_backward.srv &&
                          p->nvof.cost_backward.srv;
    if (!p->nvof.flow_diagnostics_shader ||
        !p->nvof.flow_diagnostics_buffer ||
        !p->nvof.flow_diagnostics_readback ||
        !p->nvof.flow_diagnostics_uav ||
        !raw_flow_ready ||
        !p->nvof.gray[0].srv || !p->nvof.gray[1].srv ||
        !final_state_ready) {
        MP_ERR(f, "NVOF flow diagnostics resources are incomplete\n");
        return false;
    }

    LARGE_INTEGER frequency;
    LARGE_INTEGER start;
    LARGE_INTEGER end;
    QueryPerformanceFrequency(&frequency);
    QueryPerformanceCounter(&start);

    lock_d3d11_context(p);
    const uint32_t zero_counters[FLOW_DIAG_COUNTER_COUNT] = {0};
    ID3D11DeviceContext_UpdateSubresource(
        p->context,
        (ID3D11Resource *)p->nvof.flow_diagnostics_buffer,
        0, NULL, zero_counters, 0, 0);
    ID3D11ShaderResourceView *srvs[8] = {
        p->nvof.flow_forward.srv,
        p->nvof.flow_backward.srv,
        p->nvof.cost_forward.srv,
        p->nvof.cost_backward.srv,
        p->nvof.gray[0].srv,
        p->nvof.gray[1].srv,
    };
    if (p->opts->stage5_flow_infill_test) {
        srvs[6] = p->nvof.flow_state_forward[
            FLOW_INFILL_FINAL_INDEX].srv;
        srvs[7] = p->nvof.flow_state_backward[
            FLOW_INFILL_FINAL_INDEX].srv;
    }
    ID3D11UnorderedAccessView *uav = p->nvof.flow_diagnostics_uav;
    ID3D11DeviceContext_CSSetShader(
        p->context, p->nvof.flow_diagnostics_shader, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(srvs), srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, 1, &uav, NULL);
    ID3D11DeviceContext_Dispatch(
        p->context, (p->nvof.width + 15) / 16,
        (p->nvof.height + 15) / 16, 1);

    ID3D11ShaderResourceView *null_srvs[MP_ARRAY_SIZE(srvs)] = {0};
    ID3D11UnorderedAccessView *null_uav = NULL;
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(null_srvs), null_srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, 1, &null_uav, NULL);
    ID3D11DeviceContext_CSSetShader(p->context, NULL, NULL, 0);

    ID3D11DeviceContext_CopyResource(
        p->context, (ID3D11Resource *)p->nvof.flow_diagnostics_readback,
        (ID3D11Resource *)p->nvof.flow_diagnostics_buffer);
    D3D11_MAPPED_SUBRESOURCE mapped = {0};
    HRESULT hr = ID3D11DeviceContext_Map(
        p->context,
        (ID3D11Resource *)p->nvof.flow_diagnostics_readback,
        0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        unlock_d3d11_context(p);
        MP_ERR(f, "NVOF flow diagnostics readback failed hr=0x%08lx\n",
               (unsigned long)hr);
        return false;
    }
    uint32_t counters[FLOW_DIAG_COUNTER_COUNT];
    const uint32_t *mapped_values = mapped.pData;
    for (int n = 0; n < FLOW_DIAG_COUNTER_COUNT; n++)
        counters[n] = mapped_values[n];
    ID3D11DeviceContext_Unmap(
        p->context,
        (ID3D11Resource *)p->nvof.flow_diagnostics_readback, 0);
    unlock_d3d11_context(p);
    QueryPerformanceCounter(&end);

    uint32_t expected_pixels = p->nvof.width * p->nvof.height;
    uint64_t classified_pixels =
        (uint64_t)counters[FLOW_DIAG_BOTH_VALID] +
        counters[FLOW_DIAG_FORWARD_ONLY] +
        counters[FLOW_DIAG_BACKWARD_ONLY] +
        counters[FLOW_DIAG_HOLES];
    if (counters[FLOW_DIAG_PIXELS] != expected_pixels ||
        classified_pixels != expected_pixels) {
        MP_ERR(f, "NVOF flow diagnostics returned incomplete counters "
                  "analyzed=%u classified=%llu expected=%u\n",
               counters[FLOW_DIAG_PIXELS],
               (unsigned long long)classified_pixels, expected_pixels);
        return false;
    }
    if (p->opts->stage5_flow_infill_test) {
        uint64_t final_forward_pixels =
            (uint64_t)counters[FLOW_DIAG_FINAL_FORWARD_SEED] +
            counters[FLOW_DIAG_FINAL_FORWARD_PROPAGATED] +
            counters[FLOW_DIAG_FINAL_FORWARD_HOLE];
        uint64_t final_backward_pixels =
            (uint64_t)counters[FLOW_DIAG_FINAL_BACKWARD_SEED] +
            counters[FLOW_DIAG_FINAL_BACKWARD_PROPAGATED] +
            counters[FLOW_DIAG_FINAL_BACKWARD_HOLE];
        if (final_forward_pixels != expected_pixels ||
            final_backward_pixels != expected_pixels) {
            MP_ERR(f, "NVOF final flow-state diagnostics are incomplete "
                      "forward=%llu backward=%llu expected=%u\n",
                   (unsigned long long)final_forward_pixels,
                   (unsigned long long)final_backward_pixels,
                   expected_pixels);
            return false;
        }
    }
    double elapsed_ms = (end.QuadPart - start.QuadPart) * 1000.0 /
                        frequency.QuadPart;
    p->nvof.diagnostic_pairs++;
    p->nvof.diagnostic_total_ms += elapsed_ms;
    p->nvof.diagnostic_max_ms =
        MPMAX(p->nvof.diagnostic_max_ms, elapsed_ms);
    for (int n = 0; n < FLOW_DIAG_COUNTER_COUNT; n++)
        p->nvof.diagnostic_totals[n] += counters[n];

    if (p->nvof.diagnostic_pairs <= 4 ||
        p->nvof.diagnostic_pairs % 30 == 0) {
        double pixels = counters[FLOW_DIAG_PIXELS];
        MP_INFO(f, "NVOF flow diagnostics pair=%llu pixels=%u "
                   "valid=%.2f/%.2f/%.2f holes=%.2f "
                   "oob=%.2f/%.2f inconsistent=%.2f/%.2f "
                   "high-cost=%.2f/%.2f avg-cost=%.2f/%.2f "
                   "luma-diff=%.2f luma-large=%.2f large-flow=%.2f "
                   "residual=%.2f/%.2f "
                   "residual-le3/6/12=%.2f/%.2f/%.2f|%.2f/%.2f/%.2f "
                   "sync-ms=%.3f\n",
                (unsigned long long)p->nvof.diagnostic_pairs,
                counters[FLOW_DIAG_PIXELS],
                100.0 * counters[FLOW_DIAG_BOTH_VALID] / pixels,
                100.0 * counters[FLOW_DIAG_FORWARD_ONLY] / pixels,
                100.0 * counters[FLOW_DIAG_BACKWARD_ONLY] / pixels,
                100.0 * counters[FLOW_DIAG_HOLES] / pixels,
                100.0 * counters[FLOW_DIAG_FORWARD_OOB] / pixels,
                100.0 * counters[FLOW_DIAG_BACKWARD_OOB] / pixels,
                100.0 * counters[FLOW_DIAG_FORWARD_INCONSISTENT] / pixels,
                100.0 * counters[FLOW_DIAG_BACKWARD_INCONSISTENT] / pixels,
                100.0 * counters[FLOW_DIAG_FORWARD_HIGH_COST] / pixels,
                100.0 * counters[FLOW_DIAG_BACKWARD_HIGH_COST] / pixels,
                counters[FLOW_DIAG_FORWARD_COST_SUM] / pixels,
                counters[FLOW_DIAG_BACKWARD_COST_SUM] / pixels,
                counters[FLOW_DIAG_LUMA_ABS_SUM] / pixels,
                100.0 * counters[FLOW_DIAG_LUMA_LARGE_CHANGE] / pixels,
                100.0 * counters[FLOW_DIAG_LARGE_FLOW] / pixels,
                counters[FLOW_DIAG_FORWARD_RESIDUAL_SUM] / pixels,
                counters[FLOW_DIAG_BACKWARD_RESIDUAL_SUM] / pixels,
                100.0 * counters[FLOW_DIAG_FORWARD_RESIDUAL_LE_3] / pixels,
                100.0 * counters[FLOW_DIAG_FORWARD_RESIDUAL_LE_6] / pixels,
                100.0 * counters[FLOW_DIAG_FORWARD_RESIDUAL_LE_12] / pixels,
                100.0 * counters[FLOW_DIAG_BACKWARD_RESIDUAL_LE_3] / pixels,
                100.0 * counters[FLOW_DIAG_BACKWARD_RESIDUAL_LE_6] / pixels,
                100.0 * counters[FLOW_DIAG_BACKWARD_RESIDUAL_LE_12] / pixels,
                elapsed_ms);
        if (p->opts->stage5_flow_infill_test) {
            MP_INFO(f, "NVOF final flow-state pair=%llu "
                       "forward-seed/fill/hole=%.2f/%.2f/%.2f "
                       "backward-seed/fill/hole=%.2f/%.2f/%.2f "
                       "unresolved-reject="
                       "consistency/photometric/oob/cost="
                       "%.2f/%.2f/%.2f/%.2f\n",
                    (unsigned long long)p->nvof.diagnostic_pairs,
                    100.0 * counters[FLOW_DIAG_FINAL_FORWARD_SEED] / pixels,
                    100.0 * counters[
                        FLOW_DIAG_FINAL_FORWARD_PROPAGATED] / pixels,
                    100.0 * counters[FLOW_DIAG_FINAL_FORWARD_HOLE] / pixels,
                    100.0 * counters[FLOW_DIAG_FINAL_BACKWARD_SEED] / pixels,
                    100.0 * counters[
                        FLOW_DIAG_FINAL_BACKWARD_PROPAGATED] / pixels,
                    100.0 * counters[FLOW_DIAG_FINAL_BACKWARD_HOLE] / pixels,
                    100.0 * counters[
                        FLOW_DIAG_INVERSE_RESIDUAL_REJECT] / pixels,
                    100.0 * counters[FLOW_DIAG_PHOTOMETRIC_REJECT] / pixels,
                    100.0 * counters[FLOW_DIAG_INVERSE_OOB_REJECT] / pixels,
                    100.0 * counters[FLOW_DIAG_COST_REJECT] / pixels);
        }
    }
    return true;
}

static bool prepare_and_infill_flows(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!p->nvof.prepare_flow_shader || !p->nvof.infill_flow_shader ||
        !p->nvof.flow_forward.srv || !p->nvof.cost_forward.srv ||
        !p->nvof.flow_backward.srv || !p->nvof.cost_backward.srv ||
        !p->nvof.gray[0].srv || !p->nvof.gray[1].srv) {
        MP_ERR(f, "NVOF flow infill source resources are incomplete\n");
        return false;
    }
    for (int direction = 0; direction < 2; direction++) {
        struct nvof_resource *states = direction == 0
            ? p->nvof.flow_state_forward : p->nvof.flow_state_backward;
        for (int index = 0; index < 2; index++) {
            if (!states[index].srv || !states[index].uav) {
                MP_ERR(f, "NVOF flow infill state resource is incomplete "
                          "direction=%s index=%d\n",
                       direction == 0 ? "forward" : "backward", index);
                return false;
            }
        }
    }

    UINT groups_x = (p->nvof.width + 15) / 16;
    UINT groups_y = (p->nvof.height + 15) / 16;
    ID3D11ShaderResourceView *prepare_srvs[] = {
        p->nvof.flow_forward.srv,
        p->nvof.cost_forward.srv,
        p->nvof.flow_backward.srv,
        p->nvof.cost_backward.srv,
        p->nvof.gray[0].srv,
        p->nvof.gray[1].srv,
    };
    ID3D11UnorderedAccessView *prepare_uavs[] = {
        p->nvof.flow_state_forward[0].uav,
        p->nvof.flow_state_backward[0].uav,
    };
    lock_d3d11_context(p);
    struct gpu_profile_token profile = gpu_profile_begin_locked(
        p, GPU_PROFILE_FLOW_INFILL);
    ID3D11DeviceContext_CSSetShader(
        p->context, p->nvof.prepare_flow_shader, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(prepare_srvs), prepare_srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(prepare_uavs), prepare_uavs, NULL);
    ID3D11DeviceContext_Dispatch(p->context, groups_x, groups_y, 1);

    ID3D11ShaderResourceView *null_prepare_srvs[
        MP_ARRAY_SIZE(prepare_srvs)] = {0};
    ID3D11UnorderedAccessView *null_prepare_uavs[
        MP_ARRAY_SIZE(prepare_uavs)] = {0};
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(null_prepare_srvs), null_prepare_srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(null_prepare_uavs), null_prepare_uavs,
        NULL);

    int source_index = 0;
    for (int pass = 0; pass < FLOW_INFILL_PASS_COUNT; pass++) {
        int output_index = 1 - source_index;
        ID3D11ShaderResourceView *infill_srvs[] = {
            p->nvof.flow_state_forward[source_index].srv,
            p->nvof.flow_state_backward[source_index].srv,
            p->nvof.gray[0].srv,
            p->nvof.gray[1].srv,
        };
        ID3D11UnorderedAccessView *infill_uavs[] = {
            p->nvof.flow_state_forward[output_index].uav,
            p->nvof.flow_state_backward[output_index].uav,
        };
        ID3D11DeviceContext_CSSetShader(
            p->context, p->nvof.infill_flow_shader, NULL, 0);
        ID3D11DeviceContext_CSSetShaderResources(
            p->context, 0, MP_ARRAY_SIZE(infill_srvs), infill_srvs);
        ID3D11DeviceContext_CSSetUnorderedAccessViews(
            p->context, 0, MP_ARRAY_SIZE(infill_uavs), infill_uavs, NULL);
        ID3D11DeviceContext_Dispatch(p->context, groups_x, groups_y, 1);

        ID3D11ShaderResourceView *null_infill_srvs[
            MP_ARRAY_SIZE(infill_srvs)] = {0};
        ID3D11UnorderedAccessView *null_infill_uavs[
            MP_ARRAY_SIZE(infill_uavs)] = {0};
        ID3D11DeviceContext_CSSetShaderResources(
            p->context, 0, MP_ARRAY_SIZE(null_infill_srvs),
            null_infill_srvs);
        ID3D11DeviceContext_CSSetUnorderedAccessViews(
            p->context, 0, MP_ARRAY_SIZE(null_infill_uavs),
            null_infill_uavs, NULL);
        source_index = output_index;
    }
    ID3D11DeviceContext_CSSetShader(p->context, NULL, NULL, 0);
    gpu_profile_end_locked(p, profile);
    unlock_d3d11_context(p);

    if (source_index != FLOW_INFILL_FINAL_INDEX) {
        MP_ERR(f, "NVOF flow infill final state mismatch actual=%d expected=%d\n",
               source_index, FLOW_INFILL_FINAL_INDEX);
        return false;
    }
    p->nvof.flow_infill_pairs++;
    if (p->nvof.flow_infill_pairs == 1 ||
        p->nvof.flow_infill_pairs % 120 == 0) {
        MP_INFO(f, "NVOF flow infill dispatched pairs=%llu passes=%d "
                   "state-index=%d\n",
                (unsigned long long)p->nvof.flow_infill_pairs,
                FLOW_INFILL_PASS_COUNT, FLOW_INFILL_FINAL_INDEX);
    }
    return true;
}

static bool prepare_robust_occupancy_masks(
    struct mp_filter *f, ID3D11ShaderResourceView *flow_forward,
    ID3D11ShaderResourceView *flow_backward,
    ID3D11ShaderResourceView *cost_forward,
    ID3D11ShaderResourceView *cost_backward,
    ID3D11ShaderResourceView *luma0,
    ID3D11ShaderResourceView *luma1)
{
    struct priv *p = f->priv;
    if (!p->nvof.robust_occupancy_shader ||
        !flow_forward || !flow_backward || !cost_forward || !cost_backward ||
        !luma0 || !luma1 ||
        !p->nvof.occupancy[0].uav || !p->nvof.occupancy[0].srv ||
        !p->nvof.occupancy[1].uav || !p->nvof.occupancy[1].srv ||
        !p->nvof.projected_owner[0].uav ||
        !p->nvof.projected_owner[0].srv ||
        !p->nvof.projected_owner[1].uav ||
        !p->nvof.projected_owner[1].srv) {
        MP_ERR(f, "NVOF robust occupancy resources are incomplete\n");
        return false;
    }

    const UINT clear_value[4] = {0};
    ID3D11ShaderResourceView *srvs[] = {
        flow_forward,
        flow_backward,
        cost_forward,
        cost_backward,
        luma0,
        luma1,
    };
    ID3D11UnorderedAccessView *uavs[] = {
        p->nvof.occupancy[0].uav,
        p->nvof.occupancy[1].uav,
        p->nvof.projected_owner[0].uav,
        p->nvof.projected_owner[1].uav,
    };
    lock_d3d11_context(p);
    struct gpu_profile_token profile = gpu_profile_begin_locked(
        p, GPU_PROFILE_OCCUPANCY);
    ID3D11DeviceContext_ClearUnorderedAccessViewUint(
        p->context, uavs[0], clear_value);
    ID3D11DeviceContext_ClearUnorderedAccessViewUint(
        p->context, uavs[1], clear_value);
    ID3D11DeviceContext_ClearUnorderedAccessViewUint(
        p->context, uavs[2], clear_value);
    ID3D11DeviceContext_ClearUnorderedAccessViewUint(
        p->context, uavs[3], clear_value);
    ID3D11DeviceContext_CSSetShader(
        p->context, p->nvof.robust_occupancy_shader, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(srvs), srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(uavs), uavs, NULL);
    ID3D11DeviceContext_Dispatch(
        p->context, (p->nvof.width + 15) / 16,
        (p->nvof.height + 15) / 16, 1);
    ID3D11ShaderResourceView *null_srvs[MP_ARRAY_SIZE(srvs)] = {0};
    ID3D11UnorderedAccessView *null_uavs[MP_ARRAY_SIZE(uavs)] = {0};
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(null_srvs), null_srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(null_uavs), null_uavs, NULL);
    ID3D11DeviceContext_CSSetShader(p->context, NULL, NULL, 0);
    gpu_profile_end_locked(p, profile);
    unlock_d3d11_context(p);
    return true;
}

static bool execute_nvof_pair(struct mp_filter *f, struct mp_image *frame0,
                               struct mp_image *frame1)
{
    struct priv *p = f->priv;
    if (!ensure_nvof_session(f, frame0->w, frame0->h) ||
        !extract_luma(f, frame0, 0) || !extract_luma(f, frame1, 1))
        return false;

    if (nvof_synthesis_enabled(p)) {
        bool scene_ok = p->opts->stage6_robust_test
            ? dispatch_robust_scene_analysis(f, frame0, frame1)
            : dispatch_scene_cut_analysis(f);
        if (!scene_ok)
            return false;
    }

    bool temporal_hints_reset = p->nvof.disable_temporal_hints_next;
    bool temporal_hints_disabled = temporal_hints_reset ||
                                   nvof_synthesis_enabled(p);
    NV_OF_EXECUTE_INPUT_PARAMS input = {
        .inputFrame = p->nvof.gray[0].handle,
        .referenceFrame = p->nvof.gray[1].handle,
        .disableTemporalHints = temporal_hints_disabled
                              ? NV_OF_TRUE : NV_OF_FALSE,
    };
    NV_OF_EXECUTE_OUTPUT_PARAMS output = {
        .outputBuffer = p->nvof.flow_forward.handle,
        .outputCostBuffer = p->nvof.cost_forward.handle,
        .bwdOutputBuffer = p->nvof.flow_backward.handle,
        .bwdOutputCostBuffer = p->nvof.cost_backward.handle,
        .globalFlowBuffer = NULL,
    };
    LARGE_INTEGER frequency;
    LARGE_INTEGER start;
    LARGE_INTEGER submit_end;
    LARGE_INTEGER completion_end;
    QueryPerformanceFrequency(&frequency);
    QueryPerformanceCounter(&start);
    HRESULT completion_hr = S_OK;
    lock_d3d11_context(p);
    NV_OF_STATUS status = p->nvof.api.nvOFExecute(
        p->nvof.handle, &input, &output);
    QueryPerformanceCounter(&submit_end);
    if (status == NV_OF_SUCCESS &&
        p->opts->nvof_completion_diagnostics) {
        ID3D11DeviceContext_CopyResource(
            p->context,
            (ID3D11Resource *)p->nvof.completion_readback,
            (ID3D11Resource *)p->nvof.flow_forward.texture);
        ID3D11DeviceContext_CopyResource(
            p->context,
            (ID3D11Resource *)p->nvof.completion_readback,
            (ID3D11Resource *)p->nvof.flow_backward.texture);
        D3D11_MAPPED_SUBRESOURCE mapped = {0};
        completion_hr = ID3D11DeviceContext_Map(
            p->context,
            (ID3D11Resource *)p->nvof.completion_readback,
            0, D3D11_MAP_READ, 0, &mapped);
        if (SUCCEEDED(completion_hr)) {
            volatile uint32_t marker = *(const uint32_t *)mapped.pData;
            (void)marker;
            ID3D11DeviceContext_Unmap(
                p->context,
                (ID3D11Resource *)p->nvof.completion_readback, 0);
        }
    }
    unlock_d3d11_context(p);
    QueryPerformanceCounter(&completion_end);
    if (!check_nvof_status(f, "nvOFExecute(strict-memc)", status)) {
        p->nvof.disable_temporal_hints_next = true;
        return false;
    }
    if (FAILED(completion_hr)) {
        MP_ERR(f, "NVOF completion diagnostic full-flow readback failed "
                   "hr=0x%08lx\n",
               (unsigned long)completion_hr);
        p->nvof.disable_temporal_hints_next = true;
        return false;
    }

    double elapsed_ms = (submit_end.QuadPart - start.QuadPart) * 1000.0 /
                        frequency.QuadPart;
    p->nvof.execute_total_ms += elapsed_ms;
    p->nvof.execute_max_ms = MPMAX(p->nvof.execute_max_ms, elapsed_ms);
    p->nvof.executes++;
    if (p->opts->nvof_completion_diagnostics) {
        double completion_ms =
            (completion_end.QuadPart - start.QuadPart) * 1000.0 /
            frequency.QuadPart;
        p->nvof.completion_samples++;
        p->nvof.completion_total_ms += completion_ms;
        p->nvof.completion_max_ms =
            MPMAX(p->nvof.completion_max_ms, completion_ms);
        if (p->nvof.completion_samples <= 4 ||
            p->nvof.completion_samples % 30 == 0) {
            MP_INFO(f, "NVOF completion diagnostic sample=%llu "
                       "completion-ms=%.3f average-ms=%.3f max-ms=%.3f\n",
                    (unsigned long long)p->nvof.completion_samples,
                    completion_ms,
                    p->nvof.completion_total_ms /
                        p->nvof.completion_samples,
                    p->nvof.completion_max_ms);
        }
    }
    p->nvof.disable_temporal_hints_next = false;
    if (p->opts->stage5_flow_infill_test &&
        !prepare_and_infill_flows(f)) {
        p->nvof.disable_temporal_hints_next = true;
        return false;
    }
    if (p->opts->flow_diagnostics && !run_flow_diagnostics(f)) {
        p->nvof.disable_temporal_hints_next = true;
        return false;
    }
    if (p->nvof.executes == 1 || temporal_hints_reset ||
        p->nvof.executes % 120 == 0) {
        MP_INFO(f, "NVOF API execute count=%llu latest-ms=%.3f "
                   "average-ms=%.3f max-ms=%.3f temporal-hints=%s\n",
                (unsigned long long)p->nvof.executes, elapsed_ms,
                p->nvof.execute_total_ms / p->nvof.executes,
                p->nvof.execute_max_ms,
                temporal_hints_disabled ? "disabled" : "enabled");
    }
    return true;
}

static struct robust_pair_views active_robust_pair_views(struct priv *p)
{
    return (struct robust_pair_views) {
        .flow_forward = p->nvof.flow_forward.srv,
        .flow_backward = p->nvof.flow_backward.srv,
        .cost_forward = p->nvof.cost_forward.srv,
        .cost_backward = p->nvof.cost_backward.srv,
        .scene_state = p->nvof.robust_scene_state_srv,
    };
}

static struct robust_pair_views cached_robust_pair_views(
    struct priv *p, int slot)
{
    if (slot < 0 || slot >= MP_ARRAY_SIZE(p->nvof.pair_cache) ||
        !p->nvof.pair_cache[slot].valid)
        return (struct robust_pair_views) {0};
    struct robust_pair_cache *cache = &p->nvof.pair_cache[slot];
    return (struct robust_pair_views) {
        .flow_forward = cache->flow_forward.srv,
        .flow_backward = cache->flow_backward.srv,
        .cost_forward = cache->cost_forward.srv,
        .cost_backward = cache->cost_backward.srv,
        .scene_state = cache->scene_state_srv,
    };
}

static bool robust_pair_views_ready(const struct robust_pair_views *views)
{
    return views->flow_forward && views->flow_backward &&
           views->cost_forward && views->cost_backward &&
           views->scene_state;
}

static int select_free_robust_cache_slot(struct priv *p)
{
    for (int n = 0; n < MP_ARRAY_SIZE(p->nvof.pair_cache); n++) {
        if (n != p->previous_cache_slot && n != p->central_cache_slot)
            return n;
    }
    return -1;
}

static bool preserve_active_robust_pair(struct mp_filter *f, int slot)
{
    struct priv *p = f->priv;
    if (slot < 0 || slot >= MP_ARRAY_SIZE(p->nvof.pair_cache)) {
        MP_ERR(f, "NVOF robust temporal cache slot is invalid slot=%d\n",
               slot);
        return false;
    }
    struct robust_pair_cache *cache = &p->nvof.pair_cache[slot];
    struct robust_pair_views active = active_robust_pair_views(p);
    if (!robust_pair_views_ready(&active) ||
        !cache->flow_forward.texture || !cache->flow_backward.texture ||
        !cache->cost_forward.texture || !cache->cost_backward.texture ||
        !cache->scene_state_buffer) {
        MP_ERR(f, "NVOF robust temporal cache resources are incomplete "
                  "slot=%d\n", slot);
        return false;
    }

    lock_d3d11_context(p);
    ID3D11DeviceContext_CopyResource(
        p->context, (ID3D11Resource *)cache->flow_forward.texture,
        (ID3D11Resource *)p->nvof.flow_forward.texture);
    ID3D11DeviceContext_CopyResource(
        p->context, (ID3D11Resource *)cache->flow_backward.texture,
        (ID3D11Resource *)p->nvof.flow_backward.texture);
    ID3D11DeviceContext_CopyResource(
        p->context, (ID3D11Resource *)cache->cost_forward.texture,
        (ID3D11Resource *)p->nvof.cost_forward.texture);
    ID3D11DeviceContext_CopyResource(
        p->context, (ID3D11Resource *)cache->cost_backward.texture,
        (ID3D11Resource *)p->nvof.cost_backward.texture);
    ID3D11DeviceContext_CopyResource(
        p->context, (ID3D11Resource *)cache->scene_state_buffer,
        (ID3D11Resource *)p->nvof.robust_scene_state_buffer);
    unlock_d3d11_context(p);
    cache->valid = true;
    return true;
}

static bool has_dynamic_hdr10_plus(const struct mp_image *img)
{
    if (img->params.color.hdr.scene_avg > 0 ||
        img->params.color.hdr.ootf.num_anchors > 0)
        return true;
    for (int n = 0; n < img->num_ff_side_data; n++) {
        if (img->ff_side_data[n].type == AV_FRAME_DATA_DYNAMIC_HDR_PLUS)
            return true;
    }
    return false;
}

static bool validate_input(struct mp_filter *f, struct mp_image *img)
{
    struct priv *p = f->priv;
    if (!img || img->imgfmt != IMGFMT_D3D11 || !img->hwctx) {
        MP_ERR(f, "NVOF MEMC requires D3D11 hardware frames\n");
        return false;
    }
    if (img->params.hw_subfmt != IMGFMT_P010 &&
        img->params.hw_subfmt != IMGFMT_NV12) {
        MP_ERR(f, "NVOF MEMC requires P010 or NV12, received %s\n",
               mp_imgfmt_to_name(img->params.hw_subfmt));
        return false;
    }
    if (img->dovi || img->params.repr.sys == PL_COLOR_SYSTEM_DOLBYVISION) {
        MP_ERR(f, "NVOF MEMC does not support Dolby Vision\n");
        return false;
    }
    if (has_dynamic_hdr10_plus(img)) {
        MP_ERR(f, "NVOF MEMC does not support HDR10+ metadata\n");
        return false;
    }
    if (img->params.color.transfer == PL_COLOR_TRC_HLG) {
        MP_ERR(f, "NVOF MEMC does not support HLG before output "
                  "validation\n");
        return false;
    }
    if (img->params.color.transfer == PL_COLOR_TRC_PQ &&
        (img->params.hw_subfmt != IMGFMT_P010 ||
         img->params.color.primaries != PL_COLOR_PRIM_BT_2020)) {
        MP_ERR(f, "NVOF MEMC HDR10 requires P010 BT.2020/PQ input "
               "format=%s primaries=%d transfer=%d\n",
               mp_imgfmt_to_name(img->params.hw_subfmt),
               img->params.color.primaries, img->params.color.transfer);
        return false;
    }
    if (synthesis_enabled(p)) {
        bool chroma_supported = img->params.chroma_location == PL_CHROMA_LEFT ||
            (p->opts->rife &&
             img->params.chroma_location == PL_CHROMA_TOP_LEFT);
        if (!chroma_supported) {
            MP_ERR(f, "Frame interpolation chroma siting is unsupported "
                       "mode=%s received=%d expected=%s\n",
                   p->opts->rife ? "rife" :
                   p->opts->stage6_robust_test ? "stage6" :
                   p->opts->stage5_flow_infill_test ? "stage5" : "stage4",
                   img->params.chroma_location,
                   p->opts->rife ? "left-or-top-left" : "left");
            return false;
        }
    }
    if (img->pts == MP_NOPTS_VALUE || !isfinite(img->pts)) {
        MP_ERR(f, "NVOF MEMC requires finite timestamps\n");
        return false;
    }
    return true;
}

static void pool_ref(struct texture_pool *pool)
{
    InterlockedIncrement(&pool->refs);
}

static void pool_unref(struct texture_pool *pool)
{
    if (!pool || InterlockedDecrement(&pool->refs) != 0)
        return;
    for (int n = 0; n < OUTPUT_POOL_CAPACITY; n++) {
        if (pool->slots[n].texture)
            ID3D11Texture2D_Release(pool->slots[n].texture);
    }
    if (pool->device)
        ID3D11Device_Release(pool->device);
    talloc_free(pool);
}

static bool verify_p010_views(struct mp_filter *f, ID3D11Texture2D *texture)
{
    struct priv *p = f->priv;
    ID3D11Device3 *device3 = NULL;
    HRESULT hr = ID3D11Device_QueryInterface(
        p->device, &IID_ID3D11Device3, (void **)&device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF MEMC requires ID3D11Device3 for planar P010 views "
               "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }

    ID3D11ShaderResourceView1 *y_srv = NULL;
    ID3D11ShaderResourceView1 *uv_srv = NULL;
    ID3D11UnorderedAccessView1 *y_uav = NULL;
    ID3D11UnorderedAccessView1 *uv_uav = NULL;
    D3D11_SHADER_RESOURCE_VIEW_DESC1 srv_desc = {
        .Format = DXGI_FORMAT_R16_UNORM,
        .ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D,
        .Texture2D = {
            .MostDetailedMip = 0,
            .MipLevels = 1,
            .PlaneSlice = 0,
        },
    };
    hr = ID3D11Device3_CreateShaderResourceView1(
        device3, (ID3D11Resource *)texture, &srv_desc, &y_srv);
    if (SUCCEEDED(hr)) {
        srv_desc.Format = DXGI_FORMAT_R16G16_UNORM;
        srv_desc.Texture2D.PlaneSlice = 1;
        hr = ID3D11Device3_CreateShaderResourceView1(
            device3, (ID3D11Resource *)texture, &srv_desc, &uv_srv);
    }
    D3D11_UNORDERED_ACCESS_VIEW_DESC1 uav_desc = {
        .Format = DXGI_FORMAT_R16_UNORM,
        .ViewDimension = D3D11_UAV_DIMENSION_TEXTURE2D,
        .Texture2D = {
            .MipSlice = 0,
            .PlaneSlice = 0,
        },
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateUnorderedAccessView1(
            device3, (ID3D11Resource *)texture, &uav_desc, &y_uav);
    }
    if (SUCCEEDED(hr)) {
        uav_desc.Format = DXGI_FORMAT_R16G16_UNORM;
        uav_desc.Texture2D.PlaneSlice = 1;
        hr = ID3D11Device3_CreateUnorderedAccessView1(
            device3, (ID3D11Resource *)texture, &uav_desc, &uv_uav);
    }
    if (y_srv)
        ID3D11ShaderResourceView1_Release(y_srv);
    if (uv_srv)
        ID3D11ShaderResourceView1_Release(uv_srv);
    if (y_uav)
        ID3D11UnorderedAccessView1_Release(y_uav);
    if (uv_uav)
        ID3D11UnorderedAccessView1_Release(uv_uav);
    ID3D11Device3_Release(device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF MEMC could not create planar P010 SRV/UAV views "
               "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }
    return true;
}

static struct texture_pool *create_texture_pool(struct mp_filter *f,
                                                struct mp_image *in)
{
    struct priv *p = f->priv;
    struct texture_pool *pool = talloc_zero(NULL, struct texture_pool);
    if (!pool)
        return NULL;
    pool->refs = 1;
    InitializeSRWLock(&pool->lock);
    pool->device = p->device;
    ID3D11Device_AddRef(pool->device);
    pool->width = MP_ALIGN_UP(in->w, 2);
    pool->height = MP_ALIGN_UP(in->h, 2);
    const char *mode = p->opts->rife ? "rife-tensorrt-rtx" :
                       p->opts->stage6_robust_test
                     ? "robust-scene-test" :
                       p->opts->stage5_flow_infill_test
                     ? "flow-infill-test" :
                       p->opts->stage4_synthesis_test ? "synthesis-test" :
                       p->opts->stage3_nvof_test ? "nvof-test" :
                       p->opts->stage2_timing_test ? "timing-test" :
                       "passthrough";
    MP_INFO(f, "NVOF MEMC initialized resolution=%dx%d "
            "format=P010 private-pool=%d bind-flags=SRV|UAV|RT "
            "mode=%s\n",
            in->w, in->h, OUTPUT_POOL_CAPACITY, mode);
    return pool;
}

static bool create_pool_texture(struct mp_filter *f,
                                struct texture_pool *pool,
                                ID3D11Texture2D **texture)
{
    D3D11_TEXTURE2D_DESC desc = {
        .Width = pool->width,
        .Height = pool->height,
        .MipLevels = 1,
        .ArraySize = 1,
        .Format = DXGI_FORMAT_P010,
        .SampleDesc = { .Count = 1 },
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_SHADER_RESOURCE |
                     D3D11_BIND_UNORDERED_ACCESS |
                     D3D11_BIND_RENDER_TARGET,
    };
    HRESULT hr = ID3D11Device_CreateTexture2D(
        pool->device, &desc, NULL, texture);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF MEMC could not create a private P010 texture "
               "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }
    if (!pool->views_verified && !verify_p010_views(f, *texture)) {
        ID3D11Texture2D_Release(*texture);
        *texture = NULL;
        return false;
    }
    pool->views_verified = true;
    return true;
}

static int acquire_pool_slot(struct mp_filter *f, struct texture_pool *pool)
{
    int slot = -1;
    AcquireSRWLockExclusive(&pool->lock);
    for (int n = 0; n < OUTPUT_POOL_CAPACITY; n++) {
        if (!pool->slots[n].in_use) {
            if (!pool->slots[n].texture &&
                !create_pool_texture(f, pool, &pool->slots[n].texture))
                break;
            pool->slots[n].in_use = true;
            pool->created = MPMAX(pool->created, n + 1);
            slot = n;
            pool_ref(pool);
            break;
        }
    }
    ReleaseSRWLockExclusive(&pool->lock);
    if (slot < 0) {
        MP_ERR(f, "NVOF MEMC private P010 pool exhausted capacity=%d\n",
               OUTPUT_POOL_CAPACITY);
    }
    return slot;
}

static void release_pool_slot(struct texture_pool *pool, int slot)
{
    AcquireSRWLockExclusive(&pool->lock);
    mp_assert(slot >= 0 && slot < OUTPUT_POOL_CAPACITY);
    mp_assert(pool->slots[slot].in_use);
    pool->slots[slot].in_use = false;
    ReleaseSRWLockExclusive(&pool->lock);
    pool_unref(pool);
}

static void release_output(void *arg)
{
    struct output_ref *ref = arg;
    release_pool_slot(ref->pool, ref->slot);
    talloc_free(ref);
}

static struct mp_image_params p010_output_params(const struct mp_image *in)
{
    struct mp_image_params params = in->params;
    params.hw_subfmt = IMGFMT_P010;
    return params;
}

static struct mp_image *allocate_output(struct mp_filter *f,
                                        struct mp_image *in)
{
    struct priv *p = f->priv;
    struct mp_image_params output_params = p010_output_params(in);
    if (!p->pool ||
        !mp_image_params_static_equal(&p->input_params, &output_params)) {
        pool_unref(p->pool);
        p->pool = create_texture_pool(f, in);
        if (!p->pool)
            return NULL;
        p->input_params = output_params;
        p->pool_logged = false;
    }

    int slot = acquire_pool_slot(f, p->pool);
    if (slot < 0)
        return NULL;
    struct output_ref *ref = talloc(NULL, struct output_ref);
    if (!ref) {
        release_pool_slot(p->pool, slot);
        return NULL;
    }
    *ref = (struct output_ref){ .pool = p->pool, .slot = slot };
    struct mp_image *out = mp_image_new_custom_ref(in, ref, release_output);
    if (!out) {
        release_output(ref);
        return NULL;
    }
    mp_image_copy_attributes(out, in);
    out->params = output_params;
    out->hwctx = av_buffer_ref(in->hwctx);
    MP_HANDLE_OOM(out->hwctx);
    out->planes[0] = (uint8_t *)p->pool->slots[slot].texture;
    out->planes[1] = 0;
    for (int n = 2; n < MP_MAX_PLANES; n++)
        out->planes[n] = NULL;
    if (!p->pool_logged) {
        D3D11_TEXTURE2D_DESC desc;
        ID3D11Texture2D_GetDesc(p->pool->slots[slot].texture, &desc);
        MP_INFO(f, "NVOF MEMC output pool verified format=P010 "
                "texture=%ux%u array-size=%u bind-flags=0x%x "
                "views=R16_UNORM/R16G16_UNORM\n",
                desc.Width, desc.Height, desc.ArraySize, desc.BindFlags);
        p->pool_logged = true;
    }
    return out;
}

static bool promote_nv12_frame(struct mp_filter *f, struct mp_image *out,
                               struct mp_image *in)
{
    struct priv *p = f->priv;
    if (!load_promote_nv12_shader(f))
        return false;

    ID3D11Texture2D *source = (ID3D11Texture2D *)in->planes[0];
    ID3D11Texture2D *destination = (ID3D11Texture2D *)out->planes[0];
    D3D11_TEXTURE2D_DESC source_desc;
    D3D11_TEXTURE2D_DESC destination_desc;
    ID3D11Texture2D_GetDesc(source, &source_desc);
    ID3D11Texture2D_GetDesc(destination, &destination_desc);
    UINT required_width = MP_ALIGN_UP(in->w, 2);
    UINT required_height = MP_ALIGN_UP(in->h, 2);
    UINT source_slice = (UINT)(uintptr_t)in->planes[1];
    if (source_desc.Format != DXGI_FORMAT_NV12 ||
        destination_desc.Format != DXGI_FORMAT_P010 ||
        source_desc.Width < required_width ||
        source_desc.Height < required_height ||
        destination_desc.Width < required_width ||
        destination_desc.Height < required_height ||
        source_slice >= source_desc.ArraySize) {
        MP_ERR(f, "NVOF NV12 promotion dimensions or formats are invalid "
                  "source=%ux%u/%d array=%u slice=%u "
                  "destination=%ux%u/%d visible=%dx%d\n",
               source_desc.Width, source_desc.Height, source_desc.Format,
               source_desc.ArraySize, source_slice,
               destination_desc.Width, destination_desc.Height,
               destination_desc.Format, in->w, in->h);
        return false;
    }

    ID3D11Device3 *device3 = NULL;
    HRESULT hr = ID3D11Device_QueryInterface(
        p->device, &IID_ID3D11Device3, (void **)&device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF NV12 promotion requires ID3D11Device3 "
                  "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }

    ID3D11ShaderResourceView1 *source_views[2] = {0};
    ID3D11UnorderedAccessView1 *output_views[2] = {0};
    D3D11_SHADER_RESOURCE_VIEW_DESC1 srv_desc = {0};
    srv_desc.Format = DXGI_FORMAT_R8_UNORM;
    if (source_desc.ArraySize > 1) {
        srv_desc.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2DARRAY;
        srv_desc.Texture2DArray.MostDetailedMip = 0;
        srv_desc.Texture2DArray.MipLevels = 1;
        srv_desc.Texture2DArray.FirstArraySlice = source_slice;
        srv_desc.Texture2DArray.ArraySize = 1;
        srv_desc.Texture2DArray.PlaneSlice = 0;
    } else {
        srv_desc.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D;
        srv_desc.Texture2D.MostDetailedMip = 0;
        srv_desc.Texture2D.MipLevels = 1;
        srv_desc.Texture2D.PlaneSlice = 0;
    }
    hr = ID3D11Device3_CreateShaderResourceView1(
        device3, (ID3D11Resource *)source, &srv_desc, &source_views[0]);
    srv_desc.Format = DXGI_FORMAT_R8G8_UNORM;
    if (source_desc.ArraySize > 1)
        srv_desc.Texture2DArray.PlaneSlice = 1;
    else
        srv_desc.Texture2D.PlaneSlice = 1;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateShaderResourceView1(
            device3, (ID3D11Resource *)source, &srv_desc,
            &source_views[1]);
    }

    D3D11_UNORDERED_ACCESS_VIEW_DESC1 uav_desc = {
        .Format = DXGI_FORMAT_R16_UNORM,
        .ViewDimension = D3D11_UAV_DIMENSION_TEXTURE2D,
        .Texture2D = {
            .MipSlice = 0,
            .PlaneSlice = 0,
        },
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateUnorderedAccessView1(
            device3, (ID3D11Resource *)destination, &uav_desc,
            &output_views[0]);
    }
    uav_desc.Format = DXGI_FORMAT_R16G16_UNORM;
    uav_desc.Texture2D.PlaneSlice = 1;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateUnorderedAccessView1(
            device3, (ID3D11Resource *)destination, &uav_desc,
            &output_views[1]);
    }
    ID3D11Device3_Release(device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF NV12 promotion view creation failed "
                  "hr=0x%08lx bind-flags=0x%x\n",
               (unsigned long)hr, source_desc.BindFlags);
        for (int n = 0; n < MP_ARRAY_SIZE(source_views); n++) {
            if (source_views[n])
                ID3D11ShaderResourceView1_Release(source_views[n]);
        }
        for (int n = 0; n < MP_ARRAY_SIZE(output_views); n++) {
            if (output_views[n])
                ID3D11UnorderedAccessView1_Release(output_views[n]);
        }
        return false;
    }

    ID3D11ShaderResourceView *srvs[] = {
        (ID3D11ShaderResourceView *)source_views[0],
        (ID3D11ShaderResourceView *)source_views[1],
    };
    ID3D11UnorderedAccessView *uavs[] = {
        (ID3D11UnorderedAccessView *)output_views[0],
        (ID3D11UnorderedAccessView *)output_views[1],
    };
    lock_d3d11_context(p);
    struct gpu_profile_token profile = gpu_profile_begin_locked(
        p, GPU_PROFILE_P010_COPY);
    ID3D11DeviceContext_CSSetShader(
        p->context, p->nvof.promote_nv12_shader, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(srvs), srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(uavs), uavs, NULL);
    ID3D11DeviceContext_Dispatch(
        p->context, (required_width + 7) / 8,
        (required_height + 7) / 8, 1);
    ID3D11ShaderResourceView *null_srvs[MP_ARRAY_SIZE(srvs)] = {0};
    ID3D11UnorderedAccessView *null_uavs[MP_ARRAY_SIZE(uavs)] = {0};
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(null_srvs), null_srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(null_uavs), null_uavs, NULL);
    ID3D11DeviceContext_CSSetShader(p->context, NULL, NULL, 0);
    gpu_profile_end_locked(p, profile);
    unlock_d3d11_context(p);

    for (int n = 0; n < MP_ARRAY_SIZE(source_views); n++)
        ID3D11ShaderResourceView1_Release(source_views[n]);
    for (int n = 0; n < MP_ARRAY_SIZE(output_views); n++)
        ID3D11UnorderedAccessView1_Release(output_views[n]);
    p->promoted_frames++;
    return true;
}

static bool copy_frame(struct mp_filter *f, struct mp_image *out,
                       struct mp_image *in)
{
    struct priv *p = f->priv;
    ID3D11Texture2D *source = (ID3D11Texture2D *)in->planes[0];
    ID3D11Texture2D *destination = (ID3D11Texture2D *)out->planes[0];
    ID3D11Device *source_device = NULL;
    ID3D11Texture2D_GetDevice(source, &source_device);
    bool same_device = source_device == p->device;
    if (source_device)
        ID3D11Device_Release(source_device);
    if (!same_device) {
        MP_ERR(f, "NVOF MEMC cannot copy between different D3D11 devices\n");
        return false;
    }

    D3D11_TEXTURE2D_DESC source_desc;
    D3D11_TEXTURE2D_DESC destination_desc;
    ID3D11Texture2D_GetDesc(source, &source_desc);
    ID3D11Texture2D_GetDesc(destination, &destination_desc);
    if (source_desc.Format == DXGI_FORMAT_NV12)
        return promote_nv12_frame(f, out, in);
    UINT copy_width = MP_ALIGN_UP(in->w, 2);
    UINT copy_height = MP_ALIGN_UP(in->h, 2);
    if (source_desc.Format != DXGI_FORMAT_P010 ||
        destination_desc.Format != DXGI_FORMAT_P010 ||
        source_desc.Width < copy_width ||
        source_desc.Height < copy_height ||
        destination_desc.Width < copy_width ||
        destination_desc.Height < copy_height) {
        MP_ERR(f, "NVOF MEMC P010 copy dimensions or formats are invalid "
               "source=%ux%u/%d destination=%ux%u/%d visible=%dx%d\n",
               source_desc.Width, source_desc.Height, source_desc.Format,
               destination_desc.Width, destination_desc.Height,
               destination_desc.Format, in->w, in->h);
        return false;
    }

    D3D11_BOX box = {
        .left = 0,
        .top = 0,
        .front = 0,
        .right = copy_width,
        .bottom = copy_height,
        .back = 1,
    };
    lock_d3d11_context(p);
    struct gpu_profile_token profile = gpu_profile_begin_locked(
        p, GPU_PROFILE_P010_COPY);
    ID3D11DeviceContext_CopySubresourceRegion(
        p->context, (ID3D11Resource *)destination,
        (UINT)(uintptr_t)out->planes[1], 0, 0, 0,
        (ID3D11Resource *)source, (UINT)(uintptr_t)in->planes[1], &box);
    gpu_profile_end_locked(p, profile);
    unlock_d3d11_context(p);
    return true;
}

static bool synthesize_p010_frame(struct mp_filter *f, struct mp_image *out,
                                  struct mp_image *frame0,
                                  struct mp_image *frame1,
                                  const struct robust_synthesis_context *context)
{
    struct priv *p = f->priv;
    struct robust_pair_views active = active_robust_pair_views(p);
    const struct robust_pair_views *central = p->opts->stage6_robust_test
        ? context ? &context->central : NULL
        : &active;
    bool flow_resources_ready = p->opts->stage5_flow_infill_test
        ? p->nvof.flow_state_forward[FLOW_INFILL_FINAL_INDEX].srv &&
          p->nvof.flow_state_backward[FLOW_INFILL_FINAL_INDEX].srv
        : central && robust_pair_views_ready(central);
    bool scene_resources_ready = p->opts->stage6_robust_test
        ? context && central && central->scene_state &&
          p->nvof.occupancy[0].srv && p->nvof.occupancy[1].srv &&
          p->nvof.projected_owner[0].srv &&
          p->nvof.projected_owner[1].srv &&
          p->nvof.robust_synthesis_summary_uav &&
          p->nvof.robust_temporal_constants_buffer
        : p->nvof.scene_cut_srv && p->nvof.scene_cut_summary_uav;
    if (!p->nvof.synthesize_p010_shader || !flow_resources_ready ||
        !scene_resources_ready) {
        MP_ERR(f, "NVOF P010 synthesis resources are incomplete\n");
        return false;
    }
    ID3D11Texture2D *textures[] = {
        (ID3D11Texture2D *)frame0->planes[0],
        (ID3D11Texture2D *)frame1->planes[0],
        (ID3D11Texture2D *)out->planes[0],
    };
    UINT required_width = MP_ALIGN_UP(frame0->w, 2);
    UINT required_height = MP_ALIGN_UP(frame0->h, 2);
    for (int n = 0; n < MP_ARRAY_SIZE(textures); n++) {
        D3D11_TEXTURE2D_DESC desc;
        ID3D11Texture2D_GetDesc(textures[n], &desc);
        if (desc.Format != DXGI_FORMAT_P010 || desc.ArraySize != 1 ||
            desc.Width < required_width || desc.Height < required_height) {
            MP_ERR(f, "NVOF P010 synthesis requires private single-layer "
                      "textures index=%d format=%u array-size=%u size=%ux%u "
                      "required=%ux%u\n",
                   n, (unsigned)desc.Format, desc.ArraySize,
                   desc.Width, desc.Height, required_width, required_height);
            return false;
        }
    }

    ID3D11Device3 *device3 = NULL;
    HRESULT hr = ID3D11Device_QueryInterface(
        p->device, &IID_ID3D11Device3, (void **)&device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF P010 synthesis requires ID3D11Device3 "
                  "hr=0x%08lx\n", (unsigned long)hr);
        return false;
    }

    ID3D11ShaderResourceView1 *input_views[4] = {0};
    ID3D11UnorderedAccessView1 *output_views[2] = {0};
    D3D11_SHADER_RESOURCE_VIEW_DESC1 srv_desc = {
        .Format = DXGI_FORMAT_R16_UNORM,
        .ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D,
        .Texture2D = {
            .MostDetailedMip = 0,
            .MipLevels = 1,
            .PlaneSlice = 0,
        },
    };
    hr = ID3D11Device3_CreateShaderResourceView1(
        device3, (ID3D11Resource *)textures[0], &srv_desc, &input_views[0]);
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateShaderResourceView1(
            device3, (ID3D11Resource *)textures[1], &srv_desc,
            &input_views[1]);
    }
    srv_desc.Format = DXGI_FORMAT_R16G16_UNORM;
    srv_desc.Texture2D.PlaneSlice = 1;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateShaderResourceView1(
            device3, (ID3D11Resource *)textures[0], &srv_desc,
            &input_views[2]);
    }
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateShaderResourceView1(
            device3, (ID3D11Resource *)textures[1], &srv_desc,
            &input_views[3]);
    }

    D3D11_UNORDERED_ACCESS_VIEW_DESC1 uav_desc = {
        .Format = DXGI_FORMAT_R16_UNORM,
        .ViewDimension = D3D11_UAV_DIMENSION_TEXTURE2D,
        .Texture2D = {
            .MipSlice = 0,
            .PlaneSlice = 0,
        },
    };
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateUnorderedAccessView1(
            device3, (ID3D11Resource *)textures[2], &uav_desc,
            &output_views[0]);
    }
    uav_desc.Format = DXGI_FORMAT_R16G16_UNORM;
    uav_desc.Texture2D.PlaneSlice = 1;
    if (SUCCEEDED(hr)) {
        hr = ID3D11Device3_CreateUnorderedAccessView1(
            device3, (ID3D11Resource *)textures[2], &uav_desc,
            &output_views[1]);
    }
    ID3D11Device3_Release(device3);
    if (FAILED(hr)) {
        MP_ERR(f, "NVOF P010 synthesis view creation failed "
                  "hr=0x%08lx\n", (unsigned long)hr);
        for (int n = 0; n < MP_ARRAY_SIZE(input_views); n++) {
            if (input_views[n])
                ID3D11ShaderResourceView1_Release(input_views[n]);
        }
        for (int n = 0; n < MP_ARRAY_SIZE(output_views); n++) {
            if (output_views[n])
                ID3D11UnorderedAccessView1_Release(output_views[n]);
        }
        return false;
    }

    if (p->opts->stage6_robust_test &&
        !prepare_robust_occupancy_masks(
            f, central->flow_forward, central->flow_backward,
            central->cost_forward, central->cost_backward,
            (ID3D11ShaderResourceView *)input_views[0],
            (ID3D11ShaderResourceView *)input_views[1])) {
        for (int n = 0; n < MP_ARRAY_SIZE(input_views); n++)
            ID3D11ShaderResourceView1_Release(input_views[n]);
        for (int n = 0; n < MP_ARRAY_SIZE(output_views); n++)
            ID3D11UnorderedAccessView1_Release(output_views[n]);
        return false;
    }

    ID3D11ShaderResourceView *srvs[17] = {
        (ID3D11ShaderResourceView *)input_views[0],
        (ID3D11ShaderResourceView *)input_views[1],
        (ID3D11ShaderResourceView *)input_views[2],
        (ID3D11ShaderResourceView *)input_views[3],
    };
    if (p->opts->stage5_flow_infill_test) {
        srvs[4] = p->nvof.flow_state_forward[
            FLOW_INFILL_FINAL_INDEX].srv;
        srvs[5] = p->nvof.flow_state_backward[
            FLOW_INFILL_FINAL_INDEX].srv;
    } else {
        srvs[4] = central->flow_forward;
        srvs[5] = central->flow_backward;
        srvs[6] = central->cost_forward;
        srvs[7] = central->cost_backward;
    }
    srvs[8] = p->opts->stage6_robust_test
        ? central->scene_state
        : p->nvof.scene_cut_srv;
    if (p->opts->stage6_robust_test) {
        srvs[9] = p->nvof.occupancy[0].srv;
        srvs[10] = p->nvof.occupancy[1].srv;
        const struct robust_pair_views *previous =
            context->previous_available ? &context->previous : central;
        const struct robust_pair_views *next =
            context->next_available ? &context->next : central;
        srvs[11] = previous->flow_backward;
        srvs[12] = next->flow_forward;
        srvs[13] = previous->scene_state;
        srvs[14] = next->scene_state;
        srvs[15] = p->nvof.projected_owner[0].srv;
        srvs[16] = p->nvof.projected_owner[1].srv;
    }
    ID3D11UnorderedAccessView *uavs[] = {
        (ID3D11UnorderedAccessView *)output_views[0],
        (ID3D11UnorderedAccessView *)output_views[1],
        p->opts->stage6_robust_test
            ? p->nvof.robust_synthesis_summary_uav
            : p->nvof.scene_cut_summary_uav,
    };
    lock_d3d11_context(p);
    ID3D11Buffer *temporal_constants_buffer =
        p->nvof.robust_temporal_constants_buffer;
    if (p->opts->stage6_robust_test) {
        const struct robust_temporal_constants temporal_constants = {
            .previous_available = context->previous_available ? 1 : 0,
            .next_available = context->next_available ? 1 : 0,
        };
        ID3D11DeviceContext_UpdateSubresource(
            p->context,
            (ID3D11Resource *)temporal_constants_buffer,
            0, NULL, &temporal_constants, 0, 0);
        ID3D11DeviceContext_CSSetConstantBuffers(
            p->context, 0, 1, &temporal_constants_buffer);
    }
    struct gpu_profile_token profile = gpu_profile_begin_locked(
        p, GPU_PROFILE_SYNTH);
    ID3D11DeviceContext_CSSetShader(
        p->context, p->nvof.synthesize_p010_shader, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(srvs), srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(uavs), uavs, NULL);
    ID3D11DeviceContext_Dispatch(
        p->context, (required_width + 7) / 8, (required_height + 7) / 8, 1);

    ID3D11ShaderResourceView *null_srvs[MP_ARRAY_SIZE(srvs)] = {0};
    ID3D11UnorderedAccessView *null_uavs[MP_ARRAY_SIZE(uavs)] = {0};
    ID3D11DeviceContext_CSSetShaderResources(
        p->context, 0, MP_ARRAY_SIZE(null_srvs), null_srvs);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(
        p->context, 0, MP_ARRAY_SIZE(null_uavs), null_uavs, NULL);
    if (p->opts->stage6_robust_test) {
        ID3D11Buffer *null_constant_buffer = NULL;
        ID3D11DeviceContext_CSSetConstantBuffers(
            p->context, 0, 1, &null_constant_buffer);
    }
    ID3D11DeviceContext_CSSetShader(p->context, NULL, NULL, 0);
    gpu_profile_end_locked(p, profile);
    unlock_d3d11_context(p);

    for (int n = 0; n < MP_ARRAY_SIZE(input_views); n++)
        ID3D11ShaderResourceView1_Release(input_views[n]);
    for (int n = 0; n < MP_ARRAY_SIZE(output_views); n++)
        ID3D11UnorderedAccessView1_Release(output_views[n]);
    return true;
}

static struct mp_image *copy_to_private_texture(struct mp_filter *f,
                                                struct mp_image *in)
{
    struct mp_image *copy = allocate_output(f, in);
    if (!copy || !copy_frame(f, copy, in)) {
        talloc_free(copy);
        return NULL;
    }
    return copy;
}

static void fail_filter(struct mp_filter *f)
{
    mp_filter_internal_mark_failed(f);
}

static bool write_copied_frame(struct mp_filter *f, struct mp_image *source,
                               double pts, double duration, bool stage2)
{
    struct priv *p = f->priv;
    struct mp_image *out = allocate_output(f, source);
    if (!out || !copy_frame(f, out, source)) {
        talloc_free(out);
        fail_filter(f);
        return false;
    }
    out->pts = pts;
    out->dts = MP_NOPTS_VALUE;
    out->pkt_duration = duration;
    if (stage2 && out->nominal_fps > 0)
        out->nominal_fps *= 2;
    p->copied_frames++;
    mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_VIDEO, out));
    return true;
}

static bool write_synthesized_frame(struct mp_filter *f,
                                    struct mp_image *frame0,
                                    struct mp_image *frame1,
                                    double pts, double duration,
                                    const struct robust_synthesis_context *context)
{
    struct priv *p = f->priv;
    struct mp_image *out = allocate_output(f, frame0);
    if (!out || !synthesize_p010_frame(
                    f, out, frame0, frame1, context)) {
        talloc_free(out);
        fail_filter(f);
        return false;
    }
    out->pts = pts;
    out->dts = MP_NOPTS_VALUE;
    out->pkt_duration = duration;
    if (out->nominal_fps > 0)
        out->nominal_fps *= 2;
    p->synthesized_frames++;
    mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_VIDEO, out));
    return true;
}

static bool write_rife_frame(struct mp_filter *f,
                             struct mp_image *frame0,
                             struct mp_image *frame1,
                             double pts, double duration)
{
    struct priv *p = f->priv;
    MP_VERBOSE(f, "RIFE midpoint begin pts=%.6f duration=%.6f\n",
               pts, duration);
    if (!ensure_rife_session(f, frame0)) {
        fail_filter(f);
        return false;
    }
    struct mp_image *out = allocate_output(f, frame0);
    if (!out) {
        MP_ERR(f, "RIFE could not allocate a private P010 output frame\n");
        fail_filter(f);
        return false;
    }
    char error[1024] = {0};
    struct rife_frame_diagnostics diagnostics = {0};
    lock_d3d11_context(p);
    int status = p->rife.process(
        p->rife.runtime,
        (ID3D11Texture2D *)frame0->planes[0],
        (uint32_t)(uintptr_t)frame0->planes[1],
        (ID3D11Texture2D *)frame1->planes[0],
        (uint32_t)(uintptr_t)frame1->planes[1],
        (ID3D11Texture2D *)out->planes[0],
        (uint32_t)(uintptr_t)out->planes[1],
        frame0->pts, frame1->pts, &diagnostics, error, sizeof(error));
    unlock_d3d11_context(p);
    if (status != RIFE_RUNTIME_OK) {
        MP_ERR(f, "RIFE midpoint inference failed status=%d detail=%s\n",
               status, error[0] ? error : "unknown");
        talloc_free(out);
        fail_filter(f);
        return false;
    }
    out->pts = pts;
    out->dts = MP_NOPTS_VALUE;
    out->pkt_duration = duration;
    if (out->nominal_fps > 0)
        out->nominal_fps *= 2;
    p->synthesized_frames++;
    if (diagnostics.scene_cut) {
        p->scene_cut_midpoints++;
    }
    bool classification_changed = !p->rife_scene_class_initialized ||
        p->rife_last_scene_class != diagnostics.classification;
    p->rife_scene_class_initialized = true;
    p->rife_last_scene_class = diagnostics.classification;
    bool scene_event = diagnostics.scene_cut || classification_changed ||
        (diagnostics.classification != RIFE_SCENE_NORMAL &&
         p->synthesized_frames % 30 == 0);
    double pair_interval_ms =
        isfinite(frame0->pts) && isfinite(frame1->pts)
            ? fabs(frame1->pts - frame0->pts) * 1000.0 : 0;
    bool timing_over_budget = pair_interval_ms > 0 &&
        diagnostics.inference_ms > pair_interval_ms;
    bool timing_warning = timing_over_budget &&
        (p->rife_last_timing_warning_frame == 0 ||
         p->synthesized_frames - p->rife_last_timing_warning_frame >= 240);
    if (timing_warning)
        p->rife_last_timing_warning_frame = p->synthesized_frames;
    if (scene_event || timing_warning || p->synthesized_frames % 240 == 0) {
        int level = diagnostics.scene_cut || timing_warning ? MSGL_WARN
                                                          : MSGL_INFO;
        MP_MSG(f, level, "RIFE frame evidence source0-pts=%.6f "
               "source1-pts=%.6f midpoint-pts=%.6f class=%s policy=%s "
               "average-delta=%.3f changed-ratio=%.4f average-kl=%.5f "
               "regional-kl-max=%.5f chroma-delta=%.3f "
               "edge-delta=%.3f exposure-delta=%.3f "
               "exposure-spread=%.3f scene-ms=%.3f inference-ms=%.3f\n",
               diagnostics.source0_pts, diagnostics.source1_pts,
               diagnostics.midpoint_pts,
               robust_scene_class_name(diagnostics.classification),
               diagnostics.scene_cut ? "copy-f0-hard-cut" : "rife-midpoint",
               diagnostics.average_delta, diagnostics.changed_ratio,
               diagnostics.average_kl, diagnostics.regional_kl_max,
               diagnostics.chroma_delta, diagnostics.edge_delta,
               diagnostics.exposure_delta, diagnostics.exposure_spread,
               diagnostics.scene_ms, diagnostics.inference_ms);
    }
    mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_VIDEO, out));
    return true;
}

static void process_stage1(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!mp_pin_in_needs_data(f->ppins[1]))
        return;
    if (!mp_pin_out_request_data(f->ppins[0]))
        return;

    struct mp_frame frame = mp_pin_out_read(f->ppins[0]);
    if (frame.type == MP_FRAME_NONE)
        return;
    if (frame.type == MP_FRAME_EOF) {
        mp_pin_in_write(f->ppins[1], frame);
        return;
    }
    if (frame.type != MP_FRAME_VIDEO) {
        MP_ERR(f, "NVOF MEMC stage 1 received unsupported frame type=%d\n",
               frame.type);
        mp_frame_unref(&frame);
        fail_filter(f);
        return;
    }

    struct mp_image *in = frame.data;
    if (!validate_input(f, in)) {
        talloc_free(in);
        fail_filter(f);
        return;
    }
    p->input_frames++;
    if (!write_copied_frame(f, in, in->pts, in->pkt_duration, false)) {
        talloc_free(in);
        return;
    }
    p->original_frames++;
    talloc_free(in);
}

static void clear_timing_state(struct priv *p)
{
    mp_image_unrefp(&p->frame0);
    mp_image_unrefp(&p->frame1);
    mp_image_unrefp(&p->frame2);
    mp_image_unrefp(&p->pending_frame);
    p->output_phase = OUTPUT_NONE;
    p->pair_duration = 0;
    p->next_pair_duration = 0;
    p->previous_cache_slot = -1;
    p->central_cache_slot = -1;
    p->flush_pair_after_midpoint = false;
    p->input_eof = false;
    p->output_eof_sent = false;
    p->nvof.disable_temporal_hints_next = true;
    p->robust_scene_history_reset_pending = true;
    for (int n = 0; n < MP_ARRAY_SIZE(p->nvof.pair_cache); n++)
        p->nvof.pair_cache[n].valid = false;
}

static bool valid_pair(struct mp_filter *f, struct mp_image *frame0,
                       struct mp_image *frame1, double *duration)
{
    struct priv *p = f->priv;
    if (!!frame0->hwctx != !!frame1->hwctx ||
        (frame0->hwctx && frame0->hwctx->data != frame1->hwctx->data) ||
        !mp_image_params_static_equal(&frame0->params, &frame1->params)) {
        MP_WARN(f, "NVOF MEMC stage 2 reset history because input format "
                   "or D3D11 context changed\n");
        p->discontinuities++;
        p->robust_scene_history_reset_pending = true;
        return false;
    }

    double delta = frame1->pts - frame0->pts;
    double nominal_duration = frame0->nominal_fps > 0
                            ? 1.0 / frame0->nominal_fps : 0;
    double maximum_duration = nominal_duration > 0
                            ? nominal_duration * 4.0 : 1.0;
    if (!isfinite(delta) || delta <= 0 || delta > maximum_duration) {
        MP_WARN(f, "NVOF MEMC stage 2 reset history because PTS interval "
                   "is invalid delta=%.6f maximum=%.6f\n",
                delta, maximum_duration);
        p->discontinuities++;
        p->robust_scene_history_reset_pending = true;
        return false;
    }
    *duration = delta;
    return true;
}

static bool emit_stage2_frame(struct mp_filter *f)
{
    struct priv *p = f->priv;
    struct mp_image *source = p->frame0;
    mp_assert(source);

    double pts = source->pts;
    double duration = source->pkt_duration;
    enum output_phase phase = p->output_phase;
    if (phase == OUTPUT_ORIGINAL || phase == OUTPUT_INTERMEDIATE_TEST) {
        duration = p->pair_duration / 2.0;
        if (phase == OUTPUT_INTERMEDIATE_TEST)
            pts += duration;
    }
    bool wrote;
    if (phase == OUTPUT_INTERMEDIATE_TEST && p->opts->rife) {
        wrote = write_rife_frame(f, p->frame0, p->frame1, pts, duration);
    } else if (phase == OUTPUT_INTERMEDIATE_TEST &&
               nvof_synthesis_enabled(p)) {
        wrote = write_synthesized_frame(f, p->frame0, p->frame1,
                                        pts, duration, NULL);
    } else {
        wrote = write_copied_frame(f, source, pts, duration, true);
    }
    if (!wrote)
        return false;

    if (phase == OUTPUT_ORIGINAL) {
        p->original_frames++;
        p->output_phase = OUTPUT_INTERMEDIATE_TEST;
    } else if (phase == OUTPUT_INTERMEDIATE_TEST) {
        p->intermediate_test_frames++;
        mp_image_unrefp(&p->frame0);
        p->frame0 = p->frame1;
        p->frame1 = NULL;
        p->output_phase = OUTPUT_NONE;
        p->pair_duration = 0;
    } else if (phase == OUTPUT_FINAL) {
        p->original_frames++;
        mp_image_unrefp(&p->frame0);
        p->frame0 = p->pending_frame;
        p->pending_frame = NULL;
        p->output_phase = OUTPUT_NONE;
    } else {
        MP_ASSERT_UNREACHABLE();
    }
    return true;
}

static void process_stage2(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!mp_pin_in_needs_data(f->ppins[1]))
        return;

    if (p->output_phase != OUTPUT_NONE) {
        emit_stage2_frame(f);
        return;
    }
    if (p->input_eof) {
        if (!p->output_eof_sent) {
            p->output_eof_sent = true;
            mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_EOF, NULL));
        }
        return;
    }
    if (!mp_pin_out_request_data(f->ppins[0]))
        return;

    struct mp_frame frame = mp_pin_out_read(f->ppins[0]);
    if (frame.type == MP_FRAME_NONE)
        return;
    if (frame.type == MP_FRAME_EOF) {
        p->input_eof = true;
        if (p->frame0) {
            p->output_phase = OUTPUT_FINAL;
            emit_stage2_frame(f);
        } else {
            p->output_eof_sent = true;
            mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_EOF, NULL));
        }
        return;
    }
    if (frame.type != MP_FRAME_VIDEO) {
        MP_ERR(f, "NVOF MEMC stage 2 received unsupported frame type=%d\n",
               frame.type);
        mp_frame_unref(&frame);
        fail_filter(f);
        return;
    }

    struct mp_image *in = frame.data;
    if (!validate_input(f, in)) {
        talloc_free(in);
        fail_filter(f);
        return;
    }
    p->input_frames++;
    if (nvof_analysis_enabled(p) || p->opts->rife) {
        struct mp_image *private_copy = copy_to_private_texture(f, in);
        talloc_free(in);
        if (!private_copy) {
            MP_ERR(f, "Frame interpolation could not copy input to a "
                      "private P010 texture mode=%s\n",
                   p->opts->rife ? "rife" :
                   p->opts->stage6_robust_test ? "stage6" :
                   p->opts->stage5_flow_infill_test ? "stage5" :
                   p->opts->stage4_synthesis_test ? "stage4" : "stage3");
            fail_filter(f);
            return;
        }
        in = private_copy;
    }
    if (!p->frame0) {
        p->frame0 = in;
        MP_VERBOSE(f, "RIFE buffered first private P010 frame pts=%.6f\n",
                   in->pts);
        mp_pin_out_request_data(f->ppins[0]);
        return;
    }

    double duration = 0;
    if (!valid_pair(f, p->frame0, in, &duration)) {
        p->nvof.disable_temporal_hints_next = true;
        if (p->opts->rife)
            destroy_rife_session(f);
        p->pending_frame = in;
        p->output_phase = OUTPUT_FINAL;
        emit_stage2_frame(f);
        return;
    }
    if (nvof_analysis_enabled(p) &&
        !execute_nvof_pair(f, p->frame0, in)) {
        talloc_free(in);
        fail_filter(f);
        return;
    }
    p->frame1 = in;
    p->pair_duration = duration;
    p->output_phase = OUTPUT_ORIGINAL;
    if (p->opts->rife)
        MP_VERBOSE(f, "RIFE pair ready delta=%.6f; emitting original\n",
                   duration);
    emit_stage2_frame(f);
}

static void invalidate_robust_temporal_context(struct priv *p)
{
    p->previous_cache_slot = -1;
    p->central_cache_slot = -1;
    for (int n = 0; n < MP_ARRAY_SIZE(p->nvof.pair_cache); n++)
        p->nvof.pair_cache[n].valid = false;
    p->nvof.disable_temporal_hints_next = true;
    p->robust_scene_history_reset_pending = true;
}

static bool build_robust_synthesis_context(
    struct mp_filter *f, struct robust_synthesis_context *context)
{
    struct priv *p = f->priv;
    *context = (struct robust_synthesis_context) {0};
    context->previous = cached_robust_pair_views(
        p, p->previous_cache_slot);
    context->previous_available = robust_pair_views_ready(
        &context->previous);
    if (p->central_cache_slot >= 0) {
        context->central = cached_robust_pair_views(
            p, p->central_cache_slot);
        context->next = active_robust_pair_views(p);
        context->next_available = p->frame2 &&
            robust_pair_views_ready(&context->next);
    } else {
        context->central = active_robust_pair_views(p);
    }
    if (!robust_pair_views_ready(&context->central)) {
        MP_ERR(f, "NVOF robust temporal central pair is unavailable "
                  "previous-slot=%d central-slot=%d lookahead=%s\n",
               p->previous_cache_slot, p->central_cache_slot,
               p->frame2 ? "yes" : "no");
        return false;
    }
    return true;
}

static bool emit_stage6_frame(struct mp_filter *f)
{
    struct priv *p = f->priv;
    struct mp_image *source = p->frame0;
    mp_assert(source);

    enum output_phase phase = p->output_phase;
    double pts = source->pts;
    double duration = source->pkt_duration;
    if (phase == OUTPUT_ORIGINAL || phase == OUTPUT_INTERMEDIATE_TEST) {
        duration = p->pair_duration / 2.0;
        if (phase == OUTPUT_INTERMEDIATE_TEST)
            pts += duration;
    }

    bool wrote = false;
    if (phase == OUTPUT_INTERMEDIATE_TEST) {
        struct robust_synthesis_context context;
        if (!p->frame1 || !build_robust_synthesis_context(f, &context)) {
            fail_filter(f);
            return false;
        }
        wrote = write_synthesized_frame(
            f, p->frame0, p->frame1, pts, duration, &context);
    } else {
        wrote = write_copied_frame(f, source, pts, duration, true);
    }
    if (!wrote)
        return false;

    if (phase == OUTPUT_ORIGINAL) {
        p->original_frames++;
        p->output_phase = OUTPUT_INTERMEDIATE_TEST;
    } else if (phase == OUTPUT_INTERMEDIATE_TEST) {
        p->intermediate_test_frames++;
        mp_image_unrefp(&p->frame0);
        p->frame0 = p->frame1;
        p->frame1 = NULL;
        if (p->flush_pair_after_midpoint) {
            mp_assert(!p->frame2);
            p->output_phase = OUTPUT_FINAL;
            p->pair_duration = 0;
        } else {
            mp_assert(p->frame2);
            mp_assert(p->central_cache_slot >= 0);
            p->frame1 = p->frame2;
            p->frame2 = NULL;
            p->previous_cache_slot = p->central_cache_slot;
            p->central_cache_slot = -1;
            p->pair_duration = p->next_pair_duration;
            p->next_pair_duration = 0;
            p->output_phase = OUTPUT_NONE;
        }
        p->flush_pair_after_midpoint = false;
    } else if (phase == OUTPUT_FINAL) {
        p->original_frames++;
        mp_image_unrefp(&p->frame0);
        p->output_phase = OUTPUT_NONE;
        p->pair_duration = 0;
        p->next_pair_duration = 0;
        invalidate_robust_temporal_context(p);
        if (p->pending_frame) {
            p->frame0 = p->pending_frame;
            p->pending_frame = NULL;
        }
    } else {
        MP_ASSERT_UNREACHABLE();
    }
    return true;
}

static bool copy_stage6_input(struct mp_filter *f, struct mp_image **in)
{
    struct mp_image *private_copy = copy_to_private_texture(f, *in);
    talloc_free(*in);
    *in = private_copy;
    if (private_copy)
        return true;
    MP_ERR(f, "NVOF could not copy input to a private P010 analysis "
              "texture mode=stage6-temporal\n");
    fail_filter(f);
    return false;
}

static void start_stage6_flush(struct mp_filter *f,
                               struct mp_image *pending_frame)
{
    struct priv *p = f->priv;
    p->pending_frame = pending_frame;
    p->flush_pair_after_midpoint = p->frame1 != NULL;
    p->output_phase = p->frame1 ? OUTPUT_ORIGINAL : OUTPUT_FINAL;
    emit_stage6_frame(f);
}

static void process_stage6(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!mp_pin_in_needs_data(f->ppins[1]))
        return;
    if (p->output_phase != OUTPUT_NONE) {
        emit_stage6_frame(f);
        return;
    }
    if (p->input_eof) {
        if (p->frame0) {
            start_stage6_flush(f, NULL);
        } else if (!p->output_eof_sent) {
            p->output_eof_sent = true;
            mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_EOF, NULL));
        }
        return;
    }
    if (!mp_pin_out_request_data(f->ppins[0]))
        return;

    struct mp_frame frame = mp_pin_out_read(f->ppins[0]);
    if (frame.type == MP_FRAME_NONE)
        return;
    if (frame.type == MP_FRAME_EOF) {
        p->input_eof = true;
        if (p->frame0) {
            start_stage6_flush(f, NULL);
        } else {
            p->output_eof_sent = true;
            mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_EOF, NULL));
        }
        return;
    }
    if (frame.type != MP_FRAME_VIDEO) {
        MP_ERR(f, "NVOF MEMC stage 6 received unsupported frame type=%d\n",
               frame.type);
        mp_frame_unref(&frame);
        fail_filter(f);
        return;
    }

    struct mp_image *in = frame.data;
    if (!validate_input(f, in)) {
        talloc_free(in);
        fail_filter(f);
        return;
    }
    p->input_frames++;
    if (!copy_stage6_input(f, &in))
        return;
    if (!p->frame0) {
        p->frame0 = in;
        mp_pin_out_request_data(f->ppins[0]);
        return;
    }

    if (!p->frame1) {
        double duration = 0;
        if (!valid_pair(f, p->frame0, in, &duration)) {
            start_stage6_flush(f, in);
            return;
        }
        if (!execute_nvof_pair(f, p->frame0, in)) {
            talloc_free(in);
            fail_filter(f);
            return;
        }
        p->frame1 = in;
        p->pair_duration = duration;
        mp_pin_out_request_data(f->ppins[0]);
        return;
    }

    double next_duration = 0;
    if (!valid_pair(f, p->frame1, in, &next_duration)) {
        start_stage6_flush(f, in);
        return;
    }
    int cache_slot = select_free_robust_cache_slot(p);
    if (cache_slot < 0 || !preserve_active_robust_pair(f, cache_slot)) {
        talloc_free(in);
        fail_filter(f);
        return;
    }
    p->central_cache_slot = cache_slot;
    if (!execute_nvof_pair(f, p->frame1, in)) {
        talloc_free(in);
        fail_filter(f);
        return;
    }
    p->frame2 = in;
    p->next_pair_duration = next_duration;
    p->flush_pair_after_midpoint = false;
    p->output_phase = OUTPUT_ORIGINAL;
    emit_stage6_frame(f);
}

static void process(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->opts->stage6_robust_test)
        process_stage6(f);
    else if (p->opts->rife || p->opts->stage2_timing_test ||
             nvof_analysis_enabled(p))
        process_stage2(f);
    else
        process_stage1(f);
}

static void reset_filter(struct mp_filter *f)
{
    struct priv *p = f->priv;
    clear_timing_state(p);
    if (p->rife.runtime)
        p->rife.reset(p->rife.runtime);
    p->rife_scene_class_initialized = false;
    p->resets++;
    MP_VERBOSE(f, "Frame interpolation reset backend=%s count=%llu\n",
               p->opts->rife ? "rife" : "nvof",
               (unsigned long long)p->resets);
}

static bool command_filter(struct mp_filter *f,
                           struct mp_filter_command *cmd)
{
    struct priv *p = f->priv;
    if (cmd->type != MP_FILTER_COMMAND_GET_META || !p->opts->rife)
        return false;

    struct mp_tags *tags = talloc_zero(NULL, struct mp_tags);
    mp_tags_set_str(tags, "status",
                    p->rife.runtime ? "active" : "initializing");
    mp_tags_set_str(tags, "model", p->opts->rife_model);
    mp_tags_set_str(tags, "input-frames",
                    mp_tprintf(80, "%llu",
                               (unsigned long long)p->input_frames));
    mp_tags_set_str(tags, "synthesized-frames",
                    mp_tprintf(80, "%llu",
                               (unsigned long long)p->synthesized_frames));
    *(struct mp_tags **)cmd->res = tags;
    return true;
}

static void destroy(struct mp_filter *f)
{
    struct priv *p = f->priv;
    int pending_gpu_queries = drain_gpu_profile_queries(p);
    if (pending_gpu_queries) {
        MP_WARN(f, "Frame interpolation GPU timing drain left unresolved "
                   "queries=%d\n",
                pending_gpu_queries);
    }
    read_scene_cut_summary(f);
    MP_INFO(f, "Frame interpolation shutdown backend=%s "
            "input-frames=%llu copied-frames=%llu "
            "promoted-frames=%llu "
            "original-frames=%llu intermediate-test-frames=%llu "
            "synthesized-frames=%llu scene-cut-midpoints=%llu "
            "discontinuities=%llu resets=%llu\n",
            p->opts->rife ? "rife" : "nvof",
            (unsigned long long)p->input_frames,
            (unsigned long long)p->copied_frames,
            (unsigned long long)p->promoted_frames,
            (unsigned long long)p->original_frames,
            (unsigned long long)p->intermediate_test_frames,
            (unsigned long long)p->synthesized_frames,
            (unsigned long long)p->scene_cut_midpoints,
            (unsigned long long)p->discontinuities,
            (unsigned long long)p->resets);
    if (p->nvof.executes) {
        MP_INFO(f, "NVOF API timing executes=%llu average-ms=%.3f "
                   "max-ms=%.3f note=fixed-function-engine-not-covered-by-"
                   "d3d11-timestamps\n",
                (unsigned long long)p->nvof.executes,
                p->nvof.execute_total_ms / p->nvof.executes,
                p->nvof.execute_max_ms);
    }
    if (p->nvof.completion_samples) {
        MP_INFO(f, "NVOF completion diagnostic samples=%llu "
                   "average-ms=%.3f max-ms=%.3f "
                   "sync=full-%s-flow-copy-map\n",
                (unsigned long long)p->nvof.completion_samples,
                p->nvof.completion_total_ms / p->nvof.completion_samples,
                p->nvof.completion_max_ms,
                "bidirectional");
    }
    if (p->nvof.diagnostic_pairs &&
        p->nvof.diagnostic_totals[FLOW_DIAG_PIXELS]) {
        double pixels = p->nvof.diagnostic_totals[FLOW_DIAG_PIXELS];
        MP_INFO(f, "NVOF flow diagnostics summary pairs=%llu pixels=%llu "
                   "valid=%.2f/%.2f/%.2f holes=%.2f "
                   "oob=%.2f/%.2f inconsistent=%.2f/%.2f "
                   "high-cost=%.2f/%.2f avg-cost=%.2f/%.2f "
                   "luma-diff=%.2f luma-large=%.2f large-flow=%.2f "
                   "residual=%.2f/%.2f "
                   "residual-le3/6/12=%.2f/%.2f/%.2f|%.2f/%.2f/%.2f "
                   "sync-average-ms=%.3f sync-max-ms=%.3f\n",
                (unsigned long long)p->nvof.diagnostic_pairs,
                (unsigned long long)p->nvof.diagnostic_totals[
                    FLOW_DIAG_PIXELS],
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_BOTH_VALID] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_FORWARD_ONLY] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_BACKWARD_ONLY] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_HOLES] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_FORWARD_OOB] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_BACKWARD_OOB] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_FORWARD_INCONSISTENT] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_BACKWARD_INCONSISTENT] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_FORWARD_HIGH_COST] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_BACKWARD_HIGH_COST] / pixels,
                p->nvof.diagnostic_totals[
                    FLOW_DIAG_FORWARD_COST_SUM] / pixels,
                p->nvof.diagnostic_totals[
                    FLOW_DIAG_BACKWARD_COST_SUM] / pixels,
                p->nvof.diagnostic_totals[
                    FLOW_DIAG_LUMA_ABS_SUM] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_LUMA_LARGE_CHANGE] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_LARGE_FLOW] / pixels,
                p->nvof.diagnostic_totals[
                    FLOW_DIAG_FORWARD_RESIDUAL_SUM] / pixels,
                p->nvof.diagnostic_totals[
                    FLOW_DIAG_BACKWARD_RESIDUAL_SUM] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_FORWARD_RESIDUAL_LE_3] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_FORWARD_RESIDUAL_LE_6] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_FORWARD_RESIDUAL_LE_12] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_BACKWARD_RESIDUAL_LE_3] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_BACKWARD_RESIDUAL_LE_6] / pixels,
                100.0 * p->nvof.diagnostic_totals[
                    FLOW_DIAG_BACKWARD_RESIDUAL_LE_12] / pixels,
                p->nvof.diagnostic_total_ms / p->nvof.diagnostic_pairs,
                p->nvof.diagnostic_max_ms);
        if (p->opts->stage5_flow_infill_test) {
            MP_INFO(f, "NVOF final flow-state summary pairs=%llu "
                       "forward-seed/fill/hole=%.2f/%.2f/%.2f "
                       "backward-seed/fill/hole=%.2f/%.2f/%.2f "
                       "unresolved-reject="
                       "consistency/photometric/oob/cost="
                       "%.2f/%.2f/%.2f/%.2f\n",
                    (unsigned long long)p->nvof.diagnostic_pairs,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_FINAL_FORWARD_SEED] / pixels,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_FINAL_FORWARD_PROPAGATED] / pixels,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_FINAL_FORWARD_HOLE] / pixels,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_FINAL_BACKWARD_SEED] / pixels,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_FINAL_BACKWARD_PROPAGATED] / pixels,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_FINAL_BACKWARD_HOLE] / pixels,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_INVERSE_RESIDUAL_REJECT] / pixels,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_PHOTOMETRIC_REJECT] / pixels,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_INVERSE_OOB_REJECT] / pixels,
                    100.0 * p->nvof.diagnostic_totals[
                        FLOW_DIAG_COST_REJECT] / pixels);
        }
    }
    if (p->nvof.scene_cut_pairs) {
        if (p->opts->stage6_robust_test) {
            MP_INFO(f, "NVOF robust scene summary pairs=%llu "
                       "normal=%llu flash=%llu fade-dissolve=%llu "
                       "hard-cut=%llu uncertain=%llu "
                       "policies=normal-warp,fade-crossfade,"
                       "flash-hard-cut-uncertain-hold-f0\n",
                    (unsigned long long)p->nvof.scene_cut_pairs,
                    (unsigned long long)p->nvof.robust_scene_classes[
                        ROBUST_SCENE_NORMAL],
                    (unsigned long long)p->nvof.robust_scene_classes[
                        ROBUST_SCENE_FLASH],
                    (unsigned long long)p->nvof.robust_scene_classes[
                        ROBUST_SCENE_FADE_DISSOLVE],
                    (unsigned long long)p->nvof.robust_scene_classes[
                        ROBUST_SCENE_HARD_CUT],
                    (unsigned long long)p->nvof.robust_scene_classes[
                        ROBUST_SCENE_UNCERTAIN]);
            for (int scene = 0; scene < ROBUST_SCENE_CLASS_COUNT; scene++) {
                uint64_t count = p->nvof.robust_scene_classes[scene];
                if (!count)
                    continue;
                uint64_t *metrics =
                    p->nvof.robust_scene_metric_totals[scene];
                MP_INFO(f, "NVOF robust scene metrics class=%s count=%llu "
                           "kl=%.4f average-delta=%.2f "
                           "changed-ratio=%.3f chroma-delta=%.2f "
                           "edge-delta=%.2f exposure-delta=%.2f "
                           "regional-kl-max=%.4f\n",
                        robust_scene_class_name(scene),
                        (unsigned long long)count,
                        metrics[ROBUST_SCENE_METRIC_KL_MILLI] /
                            (1000.0 * count),
                        metrics[ROBUST_SCENE_METRIC_AVERAGE_DELTA] /
                            (double)count,
                        metrics[ROBUST_SCENE_METRIC_CHANGED_RATIO_MILLI] /
                            (1000.0 * count),
                        metrics[ROBUST_SCENE_METRIC_CHROMA_DELTA] /
                            (double)count,
                        metrics[ROBUST_SCENE_METRIC_EDGE_DELTA] /
                            (double)count,
                        metrics[ROBUST_SCENE_METRIC_EXPOSURE_DELTA] /
                            (double)count,
                        metrics[ROBUST_SCENE_METRIC_REGIONAL_KL_MAX_MILLI] /
                            (1000.0 * count));
            }
            double synthesis_samples = p->nvof.robust_synthesis_totals[
                ROBUST_SYNTHESIS_PIXELS];
            uint64_t candidate_samples =
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_RAW_CANDIDATE] +
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_MEDIAN_CANDIDATE] +
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_BLOCK_CANDIDATE] +
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_TEMPORAL_CANDIDATE] +
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_EDGE_VECTOR_CANDIDATE] +
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_PROJECTED_OWNER_CANDIDATE];
            if (synthesis_samples > 0) {
                MP_INFO(f, "NVOF robust synthesis summary samples=%llu "
                           "source0/source1/blend/fallback="
                           "%.2f/%.2f/%.2f/%.2f%% "
                           "occupancy-empty=%.2f/%.2f%% "
                           "occupancy-collision=%.2f/%.2f%%\n",
                        (unsigned long long)synthesis_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_SOURCE0] / synthesis_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_SOURCE1] / synthesis_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_BLENDED] / synthesis_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_FALLBACK] / synthesis_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_OCCUPANCY0_EMPTY] /
                            synthesis_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_OCCUPANCY1_EMPTY] /
                            synthesis_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_OCCUPANCY0_COLLISION] /
                            synthesis_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_OCCUPANCY1_COLLISION] /
                            synthesis_samples);
            }
            uint64_t owner0_samples =
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_OWNER0_MATCH] +
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_OWNER0_REJECT];
            uint64_t owner1_samples =
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_OWNER1_MATCH] +
                p->nvof.robust_synthesis_totals[
                    ROBUST_SYNTHESIS_OWNER1_REJECT];
            if (owner0_samples || owner1_samples) {
                MP_INFO(f, "NVOF projected ownership collision "
                           "match/reject=%.2f/%.2f%% %.2f/%.2f%%\n",
                        owner0_samples ? 100.0 *
                            p->nvof.robust_synthesis_totals[
                                ROBUST_SYNTHESIS_OWNER0_MATCH] /
                            owner0_samples : 0.0,
                        owner0_samples ? 100.0 *
                            p->nvof.robust_synthesis_totals[
                                ROBUST_SYNTHESIS_OWNER0_REJECT] /
                            owner0_samples : 0.0,
                        owner1_samples ? 100.0 *
                            p->nvof.robust_synthesis_totals[
                                ROBUST_SYNTHESIS_OWNER1_MATCH] /
                            owner1_samples : 0.0,
                        owner1_samples ? 100.0 *
                            p->nvof.robust_synthesis_totals[
                                ROBUST_SYNTHESIS_OWNER1_REJECT] /
                            owner1_samples : 0.0);
            }
            if (candidate_samples) {
                MP_INFO(f, "NVOF robust candidate summary selected=%llu "
                           "raw/median/block/temporal/edge-vector/owner="
                           "%.2f/%.2f/%.2f/%.2f/%.2f/%.2f%%\n",
                        (unsigned long long)candidate_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_RAW_CANDIDATE] /
                            candidate_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_MEDIAN_CANDIDATE] /
                            candidate_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_BLOCK_CANDIDATE] /
                            candidate_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_TEMPORAL_CANDIDATE] /
                            candidate_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_EDGE_VECTOR_CANDIDATE] /
                            candidate_samples,
                        100.0 * p->nvof.robust_synthesis_totals[
                            ROBUST_SYNTHESIS_PROJECTED_OWNER_CANDIDATE] /
                            candidate_samples);
            }
        } else {
            MP_INFO(f, "NVOF GPU scene-cut summary pairs=%llu cuts=%llu "
                       "midpoint-policy=copy-f0\n",
                    (unsigned long long)p->nvof.scene_cut_pairs,
                    (unsigned long long)p->nvof.scene_cuts);
        }
    }
    if (p->opts->gpu_timing &&
        (nvof_analysis_enabled(p) || p->opts->rife)) {
        for (int stage = 0; stage < GPU_PROFILE_STAGE_COUNT; stage++) {
            const struct gpu_profile_stats *stats =
                &p->nvof.gpu_profile[stage];
            MP_INFO(f, "Frame interpolation GPU timing stage=%s "
                       "samples=%llu skipped=%llu "
                       "invalid=%llu average-ms=%.3f p95-ms=%.3f "
                       "p99-ms=%.3f max-ms=%.3f\n",
                    gpu_profile_stage_name(stage),
                    (unsigned long long)stats->samples,
                    (unsigned long long)stats->skipped,
                    (unsigned long long)stats->invalid,
                    stats->samples ? stats->total_ms / stats->samples : 0,
                    gpu_profile_percentile(stats, 0.95),
                    gpu_profile_percentile(stats, 0.99), stats->max_ms);
        }
    }
    clear_timing_state(p);
    destroy_rife_bridge(f);
    pool_unref(p->pool);
    p->pool = NULL;
    destroy_nvof(f);
    av_buffer_unref(&p->av_device_ref);
    mp_assert(!p->caller_context_state);
    if (p->rife_context_state)
        ID3DDeviceContextState_Release(p->rife_context_state);
    p->rife_context_state = NULL;
    if (p->context1)
        ID3D11DeviceContext1_Release(p->context1);
    p->context1 = NULL;
    if (p->multithread)
        ID3D10Multithread_Release(p->multithread);
    p->multithread = NULL;
    if (p->context)
        ID3D11DeviceContext_Release(p->context);
    if (p->device)
        ID3D11Device_Release(p->device);
}

static const struct mp_filter_info nvofmemc_filter = {
    .name = "nvofmemc",
    .process = process,
    .command = command_filter,
    .reset = reset_filter,
    .destroy = destroy,
    .priv_size = sizeof(struct priv),
};

static struct mp_filter *create(struct mp_filter *parent, void *options)
{
    struct mp_filter *f = mp_filter_create(parent, &nvofmemc_filter);
    if (!f) {
        talloc_free(options);
        return NULL;
    }
    mp_filter_add_pin(f, MP_PIN_IN, "in");
    mp_filter_add_pin(f, MP_PIN_OUT, "out");

    struct priv *p = f->priv;
    p->opts = talloc_steal(p, options);
    p->previous_cache_slot = -1;
    p->central_cache_slot = -1;
    int mode_count = p->opts->rife +
                     p->opts->stage1_passthrough +
                     p->opts->stage2_timing_test +
                     p->opts->stage3_nvof_test +
                     p->opts->stage4_synthesis_test +
                     p->opts->stage5_flow_infill_test +
                     p->opts->stage6_robust_test;
    if (mode_count != 1) {
        MP_ERR(f, "Frame interpolation requires exactly one mode: "
                "rife=yes, stage1-passthrough=yes, "
                "stage2-timing-test=yes, or "
                "stage3-nvof-test=yes, stage4-synthesis-test=yes, or "
                "stage5-flow-infill-test=yes, or "
                "stage6-robust-test=yes\n");
        goto fail;
    }
    if (p->opts->flow_diagnostics &&
        !nvof_analysis_enabled(p)) {
        MP_ERR(f, "NVOF flow diagnostics require stage 3, 4, 5, or 6 "
                  "mode\n");
        goto fail;
    }
    if (p->opts->stage2_timing_test) {
        MP_WARN(f, "NVOF MEMC stage 2 timing test repeats F0 for midpoint "
                   "frames; motion compensation is not active\n");
    }
    if (p->opts->rife) {
        if (!p->opts->rife_model || !p->opts->rife_model[0]) {
            MP_ERR(f, "RIFE requires an explicit rife-model identifier\n");
            goto fail;
        }
        MP_WARN(f, "RIFE enables strict x2 P010 midpoint inference with "
                   "TensorRT-RTX FP16; unsupported formats, missing runtime "
                   "components, incompatible engines, and inference failures "
                   "are fatal and never fall back to NVOF or passthrough\n");
    }
    if (p->opts->stage3_nvof_test) {
        MP_WARN(f, "NVOF MEMC stage 3 generates bidirectional flow and cost "
                   "but still repeats F0 for midpoint frames; motion "
                   "compensation is not active\n");
    }
    if (p->opts->stage4_synthesis_test) {
        MP_WARN(f, "NVOF MEMC stage 4 enables strict x2 P010 midpoint "
                   "synthesis; unsupported formats or synthesis failures "
                   "are fatal\n");
    }
    if (p->opts->stage5_flow_infill_test) {
        MP_WARN(f, "NVOF MEMC stage 5 enables strict x2 P010 midpoint "
                   "synthesis with native bidirectional OFA, "
                   "forward-backward consistency rejection, and two-pass "
                   "luma-guided flow infill; "
                   "unsupported formats or synthesis failures are fatal\n");
    }
    if (p->opts->stage6_robust_test) {
        MP_WARN(f, "NVOF MEMC stage 6 enables strict x2 P010 midpoint "
                   "synthesis with native bidirectional OFA and GPU-resident "
                   "3x3 histogram, chroma, edge, exposure, and hysteresis "
                   "scene classification, luma- and consistency-scored "
                   "projected ownership, "
                   "raw/median/block and luma-guided vector-median "
                   "candidates, one-frame source lookahead, four-frame "
                   "Hermite motion candidates, and "
                   "near-binary source arbitration; flow diffusion is "
                   "disabled and "
                   "unsupported formats or synthesis failures are fatal\n");
    }
    if (p->opts->flow_diagnostics) {
        MP_WARN(f, "NVOF flow diagnostics are enabled for development "
                   "measurement and add a synchronous GPU counter readback "
                   "for every analyzed pair\n");
    }
    if (p->opts->nvof_completion_diagnostics) {
        MP_WARN(f, "NVOF completion diagnostics are enabled and force a "
                   "full OFA flow staging copy and Map after "
                   "every pair; this mode is "
                   "measurement-only and intentionally synchronous\n");
    }

    struct mp_stream_info *info = mp_filter_find_stream_info(f);
    if (!info || !info->hwdec_devs)
        goto fail;
    struct hwdec_imgfmt_request request = {
        .imgfmt = IMGFMT_D3D11,
        .probing = false,
    };
    hwdec_devices_request_for_img_fmt(info->hwdec_devs, &request);
    struct mp_hwdec_ctx *hwctx = hwdec_devices_get_by_imgfmt_and_type(
        info->hwdec_devs, IMGFMT_D3D11, AV_HWDEVICE_TYPE_D3D11VA);
    if (!hwctx || !hwctx->av_device_ref) {
        MP_ERR(f, "Frame interpolation could not obtain mpv's D3D11 device\n");
        goto fail;
    }

    p->av_device_ref = av_buffer_ref(hwctx->av_device_ref);
    MP_HANDLE_OOM(p->av_device_ref);
    AVHWDeviceContext *device = (AVHWDeviceContext *)p->av_device_ref->data;
    AVD3D11VADeviceContext *d3d = device->hwctx;
    p->device = d3d->device;
    ID3D11Device_AddRef(p->device);
    ID3D11Device_GetImmediateContext(p->device, &p->context);
    if (!p->context) {
        MP_ERR(f, "Frame interpolation could not obtain the D3D11 "
                  "immediate context\n");
        goto fail;
    }
    HRESULT hr = ID3D11Device_QueryInterface(
        p->device, &IID_ID3D10Multithread, (void **)&p->multithread);
    if (FAILED(hr) || !p->multithread) {
        MP_ERR(f, "Frame interpolation requires ID3D10Multithread for "
                  "atomic D3D11 "
                  "context command blocks hr=0x%08lx\n",
               (unsigned long)hr);
        goto fail;
    }
    ID3D10Multithread_SetMultithreadProtected(p->multithread, TRUE);
    if (!create_d3d11_context_state_isolation(f))
        goto fail;
    MP_INFO(f, "Frame interpolation D3D11 context block locking and state "
               "isolation enabled\n");
    if (p->opts->rife && !load_rife_bridge(f))
        goto fail;
    if (!queue_rife_prewarm(f))
        goto fail;
    if ((nvof_analysis_enabled(p) || p->opts->rife) &&
        !create_gpu_profile_queries(f))
        goto fail;
    if (nvof_analysis_enabled(p) &&
        (!load_nvof_api(f) || !load_extract_luma_shader(f)))
        goto fail;
    return f;

fail:
    talloc_free(f);
    return NULL;
}

#define OPT_BASE_STRUCT struct opts
static const m_option_t option_fields[] = {
    {"rife", OPT_BOOL(rife)},
    {"rife-runtime-dll", OPT_STRING(rife_runtime_dll), .flags = M_OPT_FILE},
    {"rife-engine", OPT_STRING(rife_engine), .flags = M_OPT_FILE},
    {"rife-model", OPT_STRING(rife_model)},
    {"rife-cudart", OPT_STRING(rife_cudart), .flags = M_OPT_FILE},
    {"rife-source-width", OPT_INT(rife_source_width), M_RANGE(0, 16384)},
    {"rife-source-height", OPT_INT(rife_source_height), M_RANGE(0, 16384)},
    {"rife-shape-alignment", OPT_INT(rife_shape_alignment),
        M_RANGE(64, 128)},
    {"rife-scene-sample-stride", OPT_INT(rife_scene_sample_stride),
        M_RANGE(2, 64)},
    {"rife-scene-pixel-threshold", OPT_INT(rife_scene_pixel_threshold),
        M_RANGE(1, 255)},
    {"rife-scene-average-threshold", OPT_DOUBLE(rife_scene_average_threshold),
        M_RANGE(0.001, 1023.0)},
    {"rife-scene-changed-ratio", OPT_DOUBLE(rife_scene_changed_ratio),
        M_RANGE(0.001, 1.0)},
    {"memc", OPT_BOOL(stage6_robust_test)},
    {"stage1-passthrough", OPT_BOOL(stage1_passthrough)},
    {"stage2-timing-test", OPT_BOOL(stage2_timing_test)},
    {"stage3-nvof-test", OPT_BOOL(stage3_nvof_test)},
    {"stage4-synthesis-test", OPT_BOOL(stage4_synthesis_test)},
    {"stage5-flow-infill-test", OPT_BOOL(stage5_flow_infill_test)},
    {"stage6-robust-test", OPT_BOOL(stage6_robust_test)},
    {"flow-diagnostics", OPT_BOOL(flow_diagnostics)},
    {"flow-fb-abs", OPT_DOUBLE(flow_fb_abs), M_RANGE(0.001, 255.0)},
    {"flow-fb-rel", OPT_DOUBLE(flow_fb_rel), M_RANGE(0.0, 10.0)},
    {"flow-cost-max", OPT_DOUBLE(flow_cost_max), M_RANGE(0.0, 1.0)},
    {"flow-confidence-min", OPT_DOUBLE(flow_confidence_min),
        M_RANGE(0.001, 0.5)},
    {"infill-luma-threshold", OPT_DOUBLE(infill_luma_threshold),
        M_RANGE(0.0, 255.0)},
    {"scene-cut-sample-stride", OPT_INT(scene_cut_sample_stride),
        M_RANGE(2, 64)},
    {"scene-cut-pixel-threshold", OPT_INT(scene_cut_pixel_threshold),
        M_RANGE(1, 127)},
    {"scene-cut-average-threshold", OPT_DOUBLE(scene_cut_average_threshold),
        M_RANGE(0.0, 255.0)},
    {"scene-cut-changed-ratio", OPT_DOUBLE(scene_cut_changed_ratio),
        M_RANGE(0.0, 1.0)},
    {"gpu-timing", OPT_BOOL(gpu_timing)},
    {"nvof-completion-diagnostics", OPT_BOOL(nvof_completion_diagnostics)},
    {0}
};

const struct mp_user_filter_entry vf_nvofmemc = {
    .desc = {
        .description = "NVIDIA D3D11 P010 frame interpolation",
        .name = "nvofmemc",
        .priv_size = sizeof(OPT_BASE_STRUCT),
        .priv_defaults = &(const OPT_BASE_STRUCT) {
            .rife = false,
            .rife_source_width = 0,
            .rife_source_height = 0,
            .rife_shape_alignment = 0,
            .rife_scene_sample_stride = 8,
            .rife_scene_pixel_threshold = 32,
            .rife_scene_average_threshold = 24.0,
            .rife_scene_changed_ratio = 0.42,
            .stage1_passthrough = false,
            .stage2_timing_test = false,
            .stage3_nvof_test = false,
            .stage4_synthesis_test = false,
            .stage5_flow_infill_test = false,
            .stage6_robust_test = false,
            .flow_diagnostics = false,
            .flow_fb_abs = 6.0,
            .flow_fb_rel = 0.05,
            .flow_cost_max = 0.95,
            .flow_confidence_min = 0.05,
            .infill_luma_threshold = 12.0,
            .scene_cut_sample_stride = 8,
            .scene_cut_pixel_threshold = 32,
            .scene_cut_average_threshold = 24.0,
            .scene_cut_changed_ratio = 0.35,
            .gpu_timing = true,
            .nvof_completion_diagnostics = false,
        },
        .options = option_fields,
    },
    .create = create,
};
