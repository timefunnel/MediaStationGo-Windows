/*
 * MediaStationGo NVIDIA NvOFFRUC frame interpolation filter for mpv.
 *
 * Decoded NV12 frames stay on mpv's D3D11 device. The D3D11 video processor
 * converts them to shared RGBA8 surfaces, NVIDIA's official NvOFFRUC runtime
 * synthesizes intermediate frames, and a small copy shader writes BGRA output
 * surfaces understood by mpv's D3D11 mapper.
 */

#include <windows.h>
#include <d3d11.h>
#include <d3d11_1.h>
#include <d3d11_4.h>
#include <d3dcompiler.h>
#include <dxgi1_4.h>

#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>

#include <libavutil/buffer.h>
#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>
#include <libavutil/rational.h>

#include "common/common.h"
#include "filters/filter.h"
#include "filters/filter_internal.h"
#include "filters/user_filters.h"
#include "options/m_option.h"
#include "video/hwdec.h"
#include "video/mp_image.h"
#include "video/mp_image_pool.h"
#include "video/out/gpu/d3d11_helpers.h"

#define FRUC_LANE_COUNT 3
#define FRUC_MAX_RESOURCE 10

#define RELEASE_COM(value, release_fn) do { \
    if (value) { \
        release_fn(value); \
        value = NULL; \
    } \
} while (0)

/*
 * Minimal ABI declarations from Optical Flow SDK 5.0.7. NvOFFRUC is loaded
 * dynamically so the source tree does not redistribute NVIDIA's SDK headers.
 */
typedef struct NvOFFRUCHandleImpl *NvOFFRUCHandle;

typedef enum NvOFFRUCStatus {
    NvOFFRUC_SUCCESS = 0,
    NvOFFRUC_ERR_NOT_SUPPORTED,
    NvOFFRUC_ERR_INVALID_PTR,
    NvOFFRUC_ERR_INVALID_PARAM,
    NvOFFRUC_ERR_INVALID_HANDLE,
    NvOFFRUC_ERR_OUT_OF_SYSTEM_MEMORY,
    NvOFFRUC_ERR_OUT_OF_VIDEO_MEMORY,
    NvOFFRUC_ERR_OPENCV_NOT_AVAILABLE,
    NvOFFRUC_ERR_UNIMPLEMENTED,
    NvOFFRUC_ERR_OF_FAILURE,
    NvOFFRUC_ERR_DUPLICATE_RESOURCE,
    NvOFFRUC_ERR_UNREGISTERED_RESOURCE,
    NvOFFRUC_ERR_INCORRECT_API_SEQUENCE,
    NvOFFRUC_ERR_WRITE_TO_DISK_FAILED,
    NvOFFRUC_ERR_PIPELINE_EXECUTION_FAILURE,
    NvOFFRUC_ERR_SYNC_WRITE_FAILED,
    NvOFFRUC_ERR_GENERIC,
    NvOFFRUC_ERR_MAX_ERROR,
} NvOFFRUCStatus;

typedef enum NvOFFRUCCudaResourceType {
    NvOFFRUC_CUDA_UNDEFINED = -1,
    NvOFFRUC_CUDA_DEVICE_PTR,
    NvOFFRUC_CUDA_ARRAY,
} NvOFFRUCCudaResourceType;

typedef enum NvOFFRUCResourceType {
    NvOFFRUC_RESOURCE_UNDEFINED = -1,
    NvOFFRUC_RESOURCE_CUDA = 0,
    NvOFFRUC_RESOURCE_D3D11 = 1,
} NvOFFRUCResourceType;

typedef enum NvOFFRUCSurfaceFormat {
    NvOFFRUC_SURFACE_UNDEFINED = -1,
    NvOFFRUC_SURFACE_NV12 = 0,
    NvOFFRUC_SURFACE_ARGB = 1,
} NvOFFRUCSurfaceFormat;

typedef union NvOFFRUCSyncWait {
    struct {
        uint64_t value;
    } fence;
    struct {
        uint64_t render_acquire;
        uint64_t output_acquire;
    } mutex;
} NvOFFRUCSyncWait;

typedef union NvOFFRUCSyncSignal {
    struct {
        uint64_t value;
    } fence;
    struct {
        uint64_t render_release;
        uint64_t output_release;
    } mutex;
} NvOFFRUCSyncSignal;

typedef struct NvOFFRUCCreateParams {
    uint32_t width;
    uint32_t height;
    void *device;
    NvOFFRUCResourceType resource_type;
    NvOFFRUCSurfaceFormat surface_format;
    NvOFFRUCCudaResourceType cuda_resource_type;
    uint32_t reserved[32];
} NvOFFRUCCreateParams;

typedef struct NvOFFRUCFrameData {
    void *frame;
    double timestamp;
    size_t cuda_pitch;
    bool *frame_repeated;
    uint32_t reserved[32];
} NvOFFRUCFrameData;

typedef struct NvOFFRUCProcessInput {
    NvOFFRUCFrameData input;
    uint32_t skip_warp : 1;
    NvOFFRUCSyncWait wait;
    uint32_t reserved[32];
} NvOFFRUCProcessInput;

typedef struct NvOFFRUCProcessOutput {
    NvOFFRUCFrameData output;
    NvOFFRUCSyncSignal signal;
    uint32_t reserved[32];
} NvOFFRUCProcessOutput;

typedef struct NvOFFRUCRegisterParams {
    void *resources[FRUC_MAX_RESOURCE];
    void *d3d11_fence;
    uint32_t count;
} NvOFFRUCRegisterParams;

typedef struct NvOFFRUCUnregisterParams {
    void *resources[FRUC_MAX_RESOURCE];
    uint32_t count;
} NvOFFRUCUnregisterParams;

_Static_assert(sizeof(NvOFFRUCCreateParams) == 160,
               "NvOFFRUC create ABI mismatch");
_Static_assert(sizeof(NvOFFRUCFrameData) == 160,
               "NvOFFRUC frame ABI mismatch");
_Static_assert(sizeof(NvOFFRUCProcessInput) == 312,
               "NvOFFRUC input ABI mismatch");
_Static_assert(sizeof(NvOFFRUCProcessOutput) == 304,
               "NvOFFRUC output ABI mismatch");
_Static_assert(sizeof(NvOFFRUCRegisterParams) == 96,
               "NvOFFRUC register ABI mismatch");
_Static_assert(sizeof(NvOFFRUCUnregisterParams) == 88,
               "NvOFFRUC unregister ABI mismatch");

typedef NvOFFRUCStatus (CALLBACK *fruc_create_fn)(
    const NvOFFRUCCreateParams *, NvOFFRUCHandle *);
typedef NvOFFRUCStatus (CALLBACK *fruc_register_fn)(
    NvOFFRUCHandle, const NvOFFRUCRegisterParams *);
typedef NvOFFRUCStatus (CALLBACK *fruc_unregister_fn)(
    NvOFFRUCHandle, const NvOFFRUCUnregisterParams *);
typedef NvOFFRUCStatus (CALLBACK *fruc_process_fn)(
    NvOFFRUCHandle, const NvOFFRUCProcessInput *,
    const NvOFFRUCProcessOutput *);
typedef NvOFFRUCStatus (CALLBACK *fruc_destroy_fn)(NvOFFRUCHandle);

typedef HRESULT (WINAPI *d3d_compile_fn)(
    const void *source, SIZE_T source_size, const char *source_name,
    const D3D_SHADER_MACRO *defines, ID3DInclude *include,
    const char *entrypoint, const char *target, UINT flags1, UINT flags2,
    ID3DBlob **code, ID3DBlob **errors);

struct opts {
    double target_fps;
};

struct fruc_lane {
    NvOFFRUCHandle handle;
    bool registered;
    ID3D11Texture2D *output;
    ID3D11Texture2D *display_output;
    ID3D11ShaderResourceView *output_srv;
    ID3D11Fence *fence;
    uint64_t next_fence_value;
    uint64_t completion_value;
    bool frame_repeated;
    bool pending_process;
    bool pending_result;
    bool has_output;
    double output_pts;
};

struct priv {
    struct opts *opts;
    struct mp_image *current;
    struct mp_image *future;
    struct mp_image *pending_reset;
    bool eof;
    bool pair_prepared;
    bool resources_ready;
    bool output_clock_valid;
    bool input_primed;
    int current_input;
    int future_input;
    double output_pts;
    double api_input_timestamp;

    struct mp_image_params input_params;
    enum mp_imgfmt input_subfmt;
    DXGI_COLOR_SPACE_TYPE input_csp;
    DXGI_COLOR_SPACE_TYPE output_csp;
    UINT width;
    UINT height;

    uint64_t source_pairs;
    uint64_t output_frames;
    uint64_t timeline_resets;
    uint64_t fruc_process_calls;
    uint64_t fruc_output_calls;
    uint64_t fruc_skip_calls;
    uint64_t fruc_repeated_frames;
    uint64_t fruc_failures;
    double fruc_submit_total_ms;
    double fruc_submit_max_ms;
    LARGE_INTEGER performance_frequency;

    AVBufferRef *av_device_ref;
    AVBufferRef *output_pool;
    ID3D11Device *device;
    ID3D11Device5 *device5;
    ID3D11DeviceContext *context;
    ID3D11DeviceContext4 *context4;
    ID3D11VideoDevice *video_device;
    ID3D11VideoContext *video_context;
    ID3D11VideoContext1 *video_context1;
    ID3D11VideoProcessorEnumerator *vp_enumerator;
    ID3D11VideoProcessor *video_processor;

    HMODULE fruc_module;
    fruc_create_fn fruc_create;
    fruc_register_fn fruc_register;
    fruc_unregister_fn fruc_unregister;
    fruc_process_fn fruc_process;
    fruc_destroy_fn fruc_destroy;

    HMODULE compiler_module;
    d3d_compile_fn compile;
    ID3D11VertexShader *copy_vertex_shader;
    ID3D11PixelShader *copy_pixel_shader;

    ID3D11Texture2D *inputs[2];
    ID3D11Texture2D *fruc_inputs[2];
    ID3D11ShaderResourceView *input_srvs[2];
    ID3D11VideoProcessorOutputView *input_vp_views[2];
    struct fruc_lane lanes[FRUC_LANE_COUNT];
    HANDLE fence_event;
};

static const char copy_shader_source[] =
    "Texture2D<float4> source : register(t0);"
    "struct VSOut { float4 position : SV_Position; };"
    "VSOut vertex_main(uint id : SV_VertexID) {"
    " VSOut o; float2 p=float2((id<<1)&2,id&2);"
    " o.position=float4(p*float2(2,-2)+float2(-1,1),0,1); return o; }"
    "float4 pixel_copy(VSOut i) : SV_Target {"
    " return source.Load(int3(int2(i.position.xy),0)); }";

static const char *fruc_status_name(NvOFFRUCStatus status)
{
    switch (status) {
    case NvOFFRUC_SUCCESS: return "success";
    case NvOFFRUC_ERR_NOT_SUPPORTED: return "not_supported";
    case NvOFFRUC_ERR_INVALID_PTR: return "invalid_pointer";
    case NvOFFRUC_ERR_INVALID_PARAM: return "invalid_parameter";
    case NvOFFRUC_ERR_INVALID_HANDLE: return "invalid_handle";
    case NvOFFRUC_ERR_OUT_OF_SYSTEM_MEMORY: return "out_of_system_memory";
    case NvOFFRUC_ERR_OUT_OF_VIDEO_MEMORY: return "out_of_video_memory";
    case NvOFFRUC_ERR_OPENCV_NOT_AVAILABLE: return "opencv_unavailable";
    case NvOFFRUC_ERR_UNIMPLEMENTED: return "unimplemented";
    case NvOFFRUC_ERR_OF_FAILURE: return "optical_flow_failure";
    case NvOFFRUC_ERR_DUPLICATE_RESOURCE: return "duplicate_resource";
    case NvOFFRUC_ERR_UNREGISTERED_RESOURCE: return "unregistered_resource";
    case NvOFFRUC_ERR_INCORRECT_API_SEQUENCE: return "incorrect_api_sequence";
    case NvOFFRUC_ERR_WRITE_TO_DISK_FAILED: return "write_failed";
    case NvOFFRUC_ERR_PIPELINE_EXECUTION_FAILURE: return "pipeline_failure";
    case NvOFFRUC_ERR_SYNC_WRITE_FAILED: return "sync_failure";
    case NvOFFRUC_ERR_GENERIC: return "generic_error";
    default: return "unknown";
    }
}

static bool check_hr(struct mp_filter *f, const char *operation, HRESULT hr)
{
    if (SUCCEEDED(hr))
        return true;
    MP_ERR(f, "NvOFFRUC %s failed hr=0x%08lx\n", operation,
           (unsigned long)hr);
    return false;
}

static bool check_fruc(struct mp_filter *f, const char *operation,
                       NvOFFRUCStatus status)
{
    if (status == NvOFFRUC_SUCCESS)
        return true;
    struct priv *p = f->priv;
    p->fruc_failures++;
    MP_ERR(f, "NvOFFRUC %s failed status=%d (%s)\n", operation, status,
           fruc_status_name(status));
    return false;
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
    if (img->imgfmt != IMGFMT_D3D11 || !img->hwctx) {
        MP_ERR(f, "NvOFFRUC requires D3D11 hardware frames; received %s\n",
               mp_imgfmt_to_name(img->imgfmt));
        return false;
    }
    if (img->params.hw_subfmt != IMGFMT_NV12) {
        MP_ERR(f, "NvOFFRUC ARGB backend supports SDR NV12 only; received %s\n",
               mp_imgfmt_to_name(img->params.hw_subfmt));
        return false;
    }
    if (img->dovi || img->params.repr.sys == PL_COLOR_SYSTEM_DOLBYVISION) {
        MP_ERR(f, "NvOFFRUC does not support Dolby Vision input\n");
        return false;
    }
    if (has_dynamic_hdr10_plus(img)) {
        MP_ERR(f, "NvOFFRUC does not support HDR10+ dynamic metadata\n");
        return false;
    }
    if (img->params.color.transfer == PL_COLOR_TRC_PQ ||
        img->params.color.transfer == PL_COLOR_TRC_HLG) {
        MP_ERR(f, "NvOFFRUC ARGB backend refuses HDR/HLG input\n");
        return false;
    }
    if (img->pts == MP_NOPTS_VALUE || !isfinite(img->pts)) {
        MP_ERR(f, "NvOFFRUC requires finite source timestamps\n");
        return false;
    }
    return true;
}

static bool create_rgba_texture(struct mp_filter *f, UINT bind_flags,
                                UINT misc_flags, const char *operation,
                                ID3D11Texture2D **texture)
{
    struct priv *p = f->priv;
    D3D11_TEXTURE2D_DESC desc = {
        .Width = p->width,
        .Height = p->height,
        .MipLevels = 1,
        .ArraySize = 1,
        .Format = DXGI_FORMAT_R8G8B8A8_UNORM,
        .SampleDesc = { .Count = 1 },
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = bind_flags,
        .MiscFlags = misc_flags,
    };
    return check_hr(f, operation,
                    ID3D11Device_CreateTexture2D(p->device, &desc, NULL,
                                                 texture));
}

static bool compile_shader(struct mp_filter *f, const char *entrypoint,
                           const char *target, ID3DBlob **bytecode)
{
    struct priv *p = f->priv;
    ID3DBlob *errors = NULL;
    HRESULT hr = p->compile(copy_shader_source,
                            sizeof(copy_shader_source) - 1,
                            "vf_nvofa_copy.hlsl", NULL, NULL, entrypoint,
                            target, D3DCOMPILE_ENABLE_STRICTNESS |
                                        D3DCOMPILE_OPTIMIZATION_LEVEL3,
                            0, bytecode, &errors);
    if (FAILED(hr) && errors) {
        MP_ERR(f, "NvOFFRUC shader %s failed: %s\n", entrypoint,
               (const char *)ID3D10Blob_GetBufferPointer(errors));
    }
    RELEASE_COM(errors, ID3D10Blob_Release);
    return check_hr(f, entrypoint, hr);
}

static bool create_copy_shaders(struct mp_filter *f)
{
    struct priv *p = f->priv;
    ID3DBlob *vertex = NULL;
    ID3DBlob *pixel = NULL;
    bool ok = compile_shader(f, "vertex_main", "vs_5_0", &vertex) &&
              compile_shader(f, "pixel_copy", "ps_5_0", &pixel);
    if (ok) {
        ok = check_hr(f, "CreateVertexShader(copy)",
                      ID3D11Device_CreateVertexShader(
                          p->device, ID3D10Blob_GetBufferPointer(vertex),
                          ID3D10Blob_GetBufferSize(vertex), NULL,
                          &p->copy_vertex_shader)) &&
             check_hr(f, "CreatePixelShader(copy)",
                      ID3D11Device_CreatePixelShader(
                          p->device, ID3D10Blob_GetBufferPointer(pixel),
                          ID3D10Blob_GetBufferSize(pixel), NULL,
                          &p->copy_pixel_shader));
    }
    RELEASE_COM(vertex, ID3D10Blob_Release);
    RELEASE_COM(pixel, ID3D10Blob_Release);
    return ok;
}

static bool init_video_processor(struct mp_filter *f, struct mp_image *img)
{
    struct priv *p = f->priv;
    p->input_csp = mp_params_to_dxgi_colorspace(f->log, &img->params);
    struct mp_image_params output_params = img->params;
    output_params.imgfmt = IMGFMT_D3D11;
    output_params.hw_subfmt = IMGFMT_RGBA;
    output_params.repr.sys = PL_COLOR_SYSTEM_RGB;
    output_params.repr.levels = PL_COLOR_LEVELS_FULL;
    p->output_csp = mp_params_to_dxgi_colorspace(f->log, &output_params);

    double source_fps = img->nominal_fps > 1 && img->nominal_fps < 240
        ? img->nominal_fps : 24.0;
    AVRational input_rate = av_d2q(source_fps, 100000);
    AVRational output_rate = av_d2q(p->opts->target_fps, 100000);
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC content = {
        .InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        .InputFrameRate = { input_rate.num, input_rate.den },
        .InputWidth = p->width,
        .InputHeight = p->height,
        .OutputFrameRate = { output_rate.num, output_rate.den },
        .OutputWidth = p->width,
        .OutputHeight = p->height,
        .Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    };
    if (!check_hr(f, "CreateVideoProcessorEnumerator",
                  ID3D11VideoDevice_CreateVideoProcessorEnumerator(
                      p->video_device, &content, &p->vp_enumerator)) ||
        !check_hr(f, "CreateVideoProcessor",
                  ID3D11VideoDevice_CreateVideoProcessor(
                      p->video_device, p->vp_enumerator, 0,
                      &p->video_processor)))
        return false;

    ID3D11VideoProcessorEnumerator1 *enumerator1 = NULL;
    if (!check_hr(f, "QueryInterface(VideoProcessorEnumerator1)",
                  ID3D11VideoProcessorEnumerator_QueryInterface(
                      p->vp_enumerator, &IID_ID3D11VideoProcessorEnumerator1,
                      (void **)&enumerator1)))
        return false;
    BOOL conversion_supported = FALSE;
    HRESULT hr =
        ID3D11VideoProcessorEnumerator1_CheckVideoProcessorFormatConversion(
            enumerator1, DXGI_FORMAT_NV12, p->input_csp,
            DXGI_FORMAT_R8G8B8A8_UNORM, p->output_csp,
            &conversion_supported);
    ID3D11VideoProcessorEnumerator1_Release(enumerator1);
    if (!check_hr(f, "CheckVideoProcessorFormatConversion", hr) ||
        !conversion_supported) {
        MP_ERR(f, "NvOFFRUC video processor cannot convert NV12 csp=%d to RGBA8 csp=%d\n",
               p->input_csp, p->output_csp);
        return false;
    }

    RECT rect = { 0, 0, p->width, p->height };
    ID3D11VideoContext_VideoProcessorSetStreamSourceRect(
        p->video_context, p->video_processor, 0, TRUE, &rect);
    ID3D11VideoContext_VideoProcessorSetStreamDestRect(
        p->video_context, p->video_processor, 0, TRUE, &rect);
    ID3D11VideoContext_VideoProcessorSetOutputTargetRect(
        p->video_context, p->video_processor, TRUE, &rect);
    ID3D11VideoContext_VideoProcessorSetStreamAutoProcessingMode(
        p->video_context, p->video_processor, 0, FALSE);
    ID3D11VideoContext1_VideoProcessorSetStreamColorSpace1(
        p->video_context1, p->video_processor, 0, p->input_csp);
    ID3D11VideoContext1_VideoProcessorSetOutputColorSpace1(
        p->video_context1, p->video_processor, p->output_csp);
    return true;
}

static bool create_processing_textures(struct mp_filter *f)
{
    struct priv *p = f->priv;
    UINT input_bind = D3D11_BIND_SHADER_RESOURCE |
                      D3D11_BIND_RENDER_TARGET;
    UINT shared_flags = D3D11_RESOURCE_MISC_SHARED |
                        D3D11_RESOURCE_MISC_SHARED_NTHANDLE;
    for (int n = 0; n < 2; n++) {
        if (!create_rgba_texture(f, input_bind, 0,
                                 "CreateTexture2D(conversion RGBA)",
                                 &p->inputs[n]) ||
            !create_rgba_texture(f, 0, shared_flags,
                                 "CreateTexture2D(FRUC input)",
                                 &p->fruc_inputs[n]) ||
            !check_hr(f, "CreateShaderResourceView(input)",
                      ID3D11Device_CreateShaderResourceView(
                          p->device, (ID3D11Resource *)p->inputs[n], NULL,
                          &p->input_srvs[n])))
            return false;
    }

    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC view = {
        .ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D,
        .Texture2D = { .MipSlice = 0 },
    };
    for (int n = 0; n < 2; n++) {
        if (!check_hr(f, "CreateVideoProcessorOutputView(input)",
                      ID3D11VideoDevice_CreateVideoProcessorOutputView(
                          p->video_device, (ID3D11Resource *)p->inputs[n],
                          p->vp_enumerator, &view, &p->input_vp_views[n])))
            return false;
    }

    for (int n = 0; n < FRUC_LANE_COUNT; n++) {
        struct fruc_lane *lane = &p->lanes[n];
        if (!create_rgba_texture(f, 0, shared_flags,
                                 "CreateTexture2D(FRUC output)",
                                 &lane->output) ||
            !create_rgba_texture(f, D3D11_BIND_SHADER_RESOURCE, 0,
                                 "CreateTexture2D(display RGBA)",
                                 &lane->display_output) ||
            !check_hr(f, "CreateShaderResourceView(FRUC output)",
                      ID3D11Device_CreateShaderResourceView(
                          p->device,
                          (ID3D11Resource *)lane->display_output, NULL,
                          &lane->output_srv)) ||
            !check_hr(f, "CreateFence(FRUC lane)",
                      ID3D11Device5_CreateFence(
                          p->device5, 0, D3D11_FENCE_FLAG_SHARED,
                          &IID_ID3D11Fence, (void **)&lane->fence)))
            return false;
    }
    return true;
}

static bool init_fruc_lanes(struct mp_filter *f)
{
    struct priv *p = f->priv;
    NvOFFRUCCreateParams create = {
        .width = p->width,
        .height = p->height,
        .device = p->device5,
        .resource_type = NvOFFRUC_RESOURCE_D3D11,
        .surface_format = NvOFFRUC_SURFACE_ARGB,
        .cuda_resource_type = NvOFFRUC_CUDA_UNDEFINED,
    };
    for (int n = 0; n < FRUC_LANE_COUNT; n++) {
        struct fruc_lane *lane = &p->lanes[n];
        if (!check_fruc(f, "NvOFFRUCCreate",
                        p->fruc_create(&create, &lane->handle)))
            return false;
        NvOFFRUCRegisterParams resources = {
            .resources = {
                lane->output, p->fruc_inputs[0], p->fruc_inputs[1],
            },
            .d3d11_fence = lane->fence,
            .count = 3,
        };
        if (!check_fruc(f, "NvOFFRUCRegisterResource",
                        p->fruc_register(lane->handle, &resources)))
            return false;
        lane->registered = true;
    }
    return true;
}

static bool wait_for_fence(struct mp_filter *f, struct fruc_lane *lane,
                           uint64_t value)
{
    struct priv *p = f->priv;
    if (!value || ID3D11Fence_GetCompletedValue(lane->fence) >= value)
        return true;
    ResetEvent(p->fence_event);
    if (!check_hr(f, "SetEventOnCompletion",
                  ID3D11Fence_SetEventOnCompletion(
                      lane->fence, value, p->fence_event)))
        return false;
    DWORD result = WaitForSingleObject(p->fence_event, INFINITE);
    if (result != WAIT_OBJECT_0) {
        MP_ERR(f, "NvOFFRUC fence wait failed result=%lu error=%lu\n",
               (unsigned long)result, GetLastError());
        return false;
    }
    return true;
}

static bool collect_lane_result(struct mp_filter *f, struct fruc_lane *lane,
                                bool wait)
{
    struct priv *p = f->priv;
    if (!lane->pending_process)
        return true;
    if (wait && !wait_for_fence(f, lane, lane->completion_value))
        return false;
    if (ID3D11Fence_GetCompletedValue(lane->fence) < lane->completion_value)
        return true;
    if (lane->pending_result && lane->frame_repeated) {
        p->fruc_repeated_frames++;
        MP_VERBOSE(f, "NvOFFRUC repeated an interpolated frame pts=%.6f lane=%td\n",
                   lane->output_pts, lane - p->lanes);
    }
    lane->pending_process = false;
    lane->pending_result = false;
    return true;
}

static bool wait_lane_idle(struct mp_filter *f, struct fruc_lane *lane)
{
    struct priv *p = f->priv;
    if (!lane->fence)
        return true;
    uint64_t value = ++lane->next_fence_value;
    if (!check_hr(f, "Signal(FRUC lane idle)",
                  ID3D11DeviceContext4_Signal(
                      p->context4, lane->fence, value)))
        return false;
    ID3D11DeviceContext_Flush(p->context);
    return wait_for_fence(f, lane, value) &&
           collect_lane_result(f, lane, false);
}

static void release_resources(struct mp_filter *f)
{
    struct priv *p = f->priv;
    for (int n = 0; n < FRUC_LANE_COUNT; n++) {
        if (p->lanes[n].fence)
            wait_lane_idle(f, &p->lanes[n]);
    }
    for (int n = 0; n < FRUC_LANE_COUNT; n++) {
        struct fruc_lane *lane = &p->lanes[n];
        if (lane->registered && lane->handle && p->fruc_unregister) {
            NvOFFRUCUnregisterParams resources = {
                .resources = {
                    lane->output, p->fruc_inputs[0], p->fruc_inputs[1],
                },
                .count = 3,
            };
            check_fruc(f, "NvOFFRUCUnregisterResource",
                       p->fruc_unregister(lane->handle, &resources));
        }
        lane->registered = false;
        if (lane->handle && p->fruc_destroy)
            check_fruc(f, "NvOFFRUCDestroy",
                       p->fruc_destroy(lane->handle));
        lane->handle = NULL;
        RELEASE_COM(lane->output_srv, ID3D11ShaderResourceView_Release);
        RELEASE_COM(lane->display_output, ID3D11Texture2D_Release);
        RELEASE_COM(lane->output, ID3D11Texture2D_Release);
        RELEASE_COM(lane->fence, ID3D11Fence_Release);
        memset(lane, 0, sizeof(*lane));
    }

    for (int n = 0; n < 2; n++) {
        RELEASE_COM(p->input_vp_views[n],
                    ID3D11VideoProcessorOutputView_Release);
        RELEASE_COM(p->input_srvs[n], ID3D11ShaderResourceView_Release);
        RELEASE_COM(p->fruc_inputs[n], ID3D11Texture2D_Release);
        RELEASE_COM(p->inputs[n], ID3D11Texture2D_Release);
    }
    RELEASE_COM(p->copy_pixel_shader, ID3D11PixelShader_Release);
    RELEASE_COM(p->copy_vertex_shader, ID3D11VertexShader_Release);
    RELEASE_COM(p->video_processor, ID3D11VideoProcessor_Release);
    RELEASE_COM(p->vp_enumerator,
                ID3D11VideoProcessorEnumerator_Release);
    av_buffer_unref(&p->output_pool);
    p->resources_ready = false;
    p->pair_prepared = false;
    p->input_primed = false;
}

static bool init_resources(struct mp_filter *f, struct mp_image *img)
{
    struct priv *p = f->priv;
    release_resources(f);
    p->width = img->w;
    p->height = img->h;
    p->input_params = img->params;
    p->input_subfmt = img->params.hw_subfmt;
    if (p->width < 320 || p->height < 240) {
        MP_ERR(f, "NvOFFRUC refuses dimensions below 320x240\n");
        return false;
    }
    if (p->width > 3840 || p->height > 2160) {
        MP_ERR(f, "NvOFFRUC supports at most 3840x2160; received %ux%u\n",
               p->width, p->height);
        return false;
    }
    if (img->nominal_fps > 0 && img->nominal_fps < 20.0) {
        MP_ERR(f, "NvOFFRUC three-lane scheduler requires source fps >= 20; received %.3f\n",
               img->nominal_fps);
        return false;
    }
    if (img->nominal_fps > 0 &&
        p->opts->target_fps <= img->nominal_fps + 0.01) {
        MP_ERR(f, "NvOFFRUC target %.3f must exceed source %.3f fps\n",
               p->opts->target_fps, img->nominal_fps);
        return false;
    }
    if (!init_video_processor(f, img) ||
        !create_copy_shaders(f) ||
        !create_processing_textures(f) ||
        !init_fruc_lanes(f)) {
        release_resources(f);
        return false;
    }
    p->resources_ready = true;
    MP_INFO(f, "NvOFFRUC initialized backend=D3D11-ARGB lanes=%d source=%s resolution=%ux%u target-fps=%.3f input-csp=%d output-csp=%d\n",
            FRUC_LANE_COUNT, mp_imgfmt_to_name(p->input_subfmt),
            p->width, p->height, p->opts->target_fps,
            p->input_csp, p->output_csp);
    return true;
}

static ID3D11VideoProcessorInputView *create_input_view(
    struct mp_filter *f, struct mp_image *img)
{
    struct priv *p = f->priv;
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC desc = {
        .ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D,
        .Texture2D = { .ArraySlice = (uintptr_t)img->planes[1] },
    };
    ID3D11VideoProcessorInputView *view = NULL;
    if (!check_hr(f, "CreateVideoProcessorInputView",
                  ID3D11VideoDevice_CreateVideoProcessorInputView(
                      p->video_device, (ID3D11Resource *)img->planes[0],
                      p->vp_enumerator, &desc, &view)))
        return NULL;
    return view;
}

static bool convert_frame(struct mp_filter *f, struct mp_image *img,
                          int output_index)
{
    struct priv *p = f->priv;
    ID3D11VideoProcessorInputView *input = create_input_view(f, img);
    if (!input)
        return false;
    D3D11_VIDEO_PROCESSOR_STREAM stream = {
        .Enable = TRUE,
        .OutputIndex = 0,
        .InputFrameOrField = 0,
        .pInputSurface = input,
    };
    HRESULT hr = ID3D11VideoContext_VideoProcessorBlt(
        p->video_context, p->video_processor,
        p->input_vp_views[output_index], 0, 1, &stream);
    ID3D11VideoProcessorInputView_Release(input);
    if (!check_hr(f, "VideoProcessorBlt(NV12 to RGBA)", hr))
        return false;
    ID3D11DeviceContext_CopyResource(
        p->context, (ID3D11Resource *)p->fruc_inputs[output_index],
        (ID3D11Resource *)p->inputs[output_index]);
    return true;
}

static double elapsed_ms(struct priv *p, LARGE_INTEGER start,
                         LARGE_INTEGER end)
{
    if (!p->performance_frequency.QuadPart)
        return 0;
    return (double)(end.QuadPart - start.QuadPart) * 1000.0 /
           (double)p->performance_frequency.QuadPart;
}

static bool process_lane(struct mp_filter *f, struct fruc_lane *lane,
                         ID3D11Texture2D *input, double api_input_timestamp,
                         double api_output_timestamp, double output_pts,
                         bool skip_warp)
{
    struct priv *p = f->priv;
    if (!collect_lane_result(f, lane, true))
        return false;

    uint64_t wait_value = ++lane->next_fence_value;
    if (!check_hr(f, "Signal(FRUC input ready)",
                  ID3D11DeviceContext4_Signal(
                      p->context4, lane->fence, wait_value)))
        return false;
    ID3D11DeviceContext_Flush(p->context);
    if (!wait_for_fence(f, lane, wait_value))
        return false;
    uint64_t signal_value = ++lane->next_fence_value;
    lane->frame_repeated = false;

    NvOFFRUCProcessInput in = {
        .input = {
            .frame = input,
            .timestamp = api_input_timestamp,
        },
        .skip_warp = skip_warp ? 1 : 0,
        .wait = { .fence = { .value = wait_value } },
    };
    NvOFFRUCProcessOutput out = {
        .output = {
            .frame = lane->output,
            .timestamp = api_output_timestamp,
            .frame_repeated = &lane->frame_repeated,
        },
        .signal = { .fence = { .value = signal_value } },
    };

    LARGE_INTEGER start = {0}, end = {0};
    QueryPerformanceCounter(&start);
    NvOFFRUCStatus status = p->fruc_process(lane->handle, &in, &out);
    QueryPerformanceCounter(&end);
    double duration = elapsed_ms(p, start, end);
    p->fruc_submit_total_ms += duration;
    p->fruc_submit_max_ms = MPMAX(p->fruc_submit_max_ms, duration);
    p->fruc_process_calls++;
    if (!check_fruc(f, "NvOFFRUCProcess", status))
        return false;

    lane->completion_value = signal_value;
    lane->has_output = !skip_warp;
    lane->output_pts = output_pts;
    lane->pending_process = true;
    lane->pending_result = !skip_warp;
    if (skip_warp)
        p->fruc_skip_calls++;
    else
        p->fruc_output_calls++;
    return true;
}

static double source_duration(struct mp_image *img);

static bool ensure_current_input(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->input_primed)
        return true;
    p->current_input = 0;
    if (!convert_frame(f, p->current, p->current_input))
        return false;
    p->api_input_timestamp = 1.0;
    for (int n = 0; n < FRUC_LANE_COUNT; n++) {
        double warmup_pts =
            p->current->pts - source_duration(p->current) * 0.5;
        if (!process_lane(f, &p->lanes[n],
                          p->fruc_inputs[p->current_input],
                          p->api_input_timestamp, 0.5, warmup_pts, false))
            return false;
        p->lanes[n].has_output = false;
        p->lanes[n].pending_result = false;
    }
    p->input_primed = true;
    return true;
}

static bool prepare_pair(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->pair_prepared)
        return true;
    if (!ensure_current_input(f))
        return false;

    p->future_input = 1 - p->current_input;
    if (!convert_frame(f, p->future, p->future_input))
        return false;

    double step = 1.0 / p->opts->target_fps;
    double epsilon = step * 0.001;
    double targets[FRUC_LANE_COUNT] = {0};
    int target_count = 0;
    double target = p->output_pts;
    while (target <= p->current->pts + epsilon)
        target += step;
    while (target < p->future->pts - epsilon) {
        if (target_count >= FRUC_LANE_COUNT) {
            MP_ERR(f, "NvOFFRUC interval %.6f..%.6f requires more than %d interpolation lanes\n",
                   p->current->pts, p->future->pts, FRUC_LANE_COUNT);
            return false;
        }
        targets[target_count++] = target;
        target += step;
    }

    for (int n = 0; n < FRUC_LANE_COUNT; n++) {
        bool skip = n >= target_count;
        double output_pts = skip
            ? p->current->pts + (p->future->pts - p->current->pts) * 0.5
            : targets[n];
        double alpha = (output_pts - p->current->pts) /
                       (p->future->pts - p->current->pts);
        double api_output_timestamp = p->api_input_timestamp +
                                      MPCLAMP(alpha, 0.0, 1.0);
        if (!process_lane(f, &p->lanes[n],
                          p->fruc_inputs[p->future_input],
                          p->api_input_timestamp + 1.0,
                          api_output_timestamp, output_pts, skip))
            return false;
    }
    p->pair_prepared = true;
    p->source_pairs++;
    MP_TRACE(f, "NvOFFRUC prepared pair current=%.6f future=%.6f interpolated=%d\n",
             p->current->pts, p->future->pts, target_count);
    return true;
}

static struct mp_image *alloc_output(struct mp_filter *f,
                                     struct mp_image *attributes,
                                     double pts)
{
    struct priv *p = f->priv;
    if (!mp_update_av_hw_frames_pool(&p->output_pool, p->av_device_ref,
                                     IMGFMT_D3D11, IMGFMT_BGRA,
                                     p->width, p->height, false)) {
        MP_ERR(f, "NvOFFRUC failed to allocate BGRA hardware pool\n");
        return NULL;
    }
    AVFrame *frame = av_frame_alloc();
    MP_HANDLE_OOM(frame);
    if (av_hwframe_get_buffer(p->output_pool, frame, 0) < 0) {
        MP_ERR(f, "NvOFFRUC failed to allocate BGRA output frame\n");
        av_frame_free(&frame);
        return NULL;
    }
    struct mp_image *out = mp_image_from_av_frame(frame);
    av_frame_free(&frame);
    if (!out)
        return NULL;
    mp_image_copy_attributes(out, attributes);
    out->pts = pts;
    out->dts = MP_NOPTS_VALUE;
    out->pkt_duration = 1.0 / p->opts->target_fps;
    out->nominal_fps = p->opts->target_fps;
    out->params.repr.sys = PL_COLOR_SYSTEM_RGB;
    out->params.repr.levels = PL_COLOR_LEVELS_FULL;
    return out;
}

static ID3D11RenderTargetView *create_output_rtv(struct mp_filter *f,
                                                 struct mp_image *out)
{
    struct priv *p = f->priv;
    ID3D11Texture2D *texture = (ID3D11Texture2D *)out->planes[0];
    D3D11_TEXTURE2D_DESC texture_desc;
    ID3D11Texture2D_GetDesc(texture, &texture_desc);
    D3D11_RENDER_TARGET_VIEW_DESC view = {
        .Format = texture_desc.Format,
    };
    if (texture_desc.ArraySize > 1) {
        view.ViewDimension = D3D11_RTV_DIMENSION_TEXTURE2DARRAY;
        view.Texture2DArray.MipSlice = 0;
        view.Texture2DArray.FirstArraySlice =
            (uintptr_t)out->planes[1];
        view.Texture2DArray.ArraySize = 1;
    } else {
        view.ViewDimension = D3D11_RTV_DIMENSION_TEXTURE2D;
        view.Texture2D.MipSlice = 0;
    }
    ID3D11RenderTargetView *rtv = NULL;
    if (!check_hr(f, "CreateRenderTargetView(output)",
                  ID3D11Device_CreateRenderTargetView(
                      p->device, (ID3D11Resource *)texture, &view, &rtv)))
        return NULL;
    return rtv;
}

static bool draw_copy(struct mp_filter *f, ID3D11ShaderResourceView *source,
                      ID3D11RenderTargetView *target)
{
    struct priv *p = f->priv;
    D3D11_VIEWPORT viewport = {
        .Width = p->width,
        .Height = p->height,
        .MaxDepth = 1.0f,
    };
    float blend_factor[4] = {0};
    ID3D11DeviceContext_IASetPrimitiveTopology(
        p->context, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_IASetInputLayout(p->context, NULL);
    ID3D11DeviceContext_VSSetShader(
        p->context, p->copy_vertex_shader, NULL, 0);
    ID3D11DeviceContext_PSSetShader(
        p->context, p->copy_pixel_shader, NULL, 0);
    ID3D11DeviceContext_OMSetBlendState(
        p->context, NULL, blend_factor, 0xffffffff);
    ID3D11DeviceContext_OMSetDepthStencilState(p->context, NULL, 0);
    ID3D11DeviceContext_RSSetState(p->context, NULL);
    ID3D11DeviceContext_RSSetViewports(p->context, 1, &viewport);
    ID3D11DeviceContext_PSSetShaderResources(
        p->context, 0, 1, &source);
    ID3D11DeviceContext_OMSetRenderTargets(
        p->context, 1, &target, NULL);
    ID3D11DeviceContext_Draw(p->context, 3, 0);
    ID3D11ShaderResourceView *empty_source = NULL;
    ID3D11RenderTargetView *empty_target = NULL;
    ID3D11DeviceContext_PSSetShaderResources(
        p->context, 0, 1, &empty_source);
    ID3D11DeviceContext_OMSetRenderTargets(
        p->context, 1, &empty_target, NULL);
    return true;
}

static struct fruc_lane *find_output_lane(struct priv *p, double pts)
{
    double tolerance = 0.1 / p->opts->target_fps;
    for (int n = 0; n < FRUC_LANE_COUNT; n++) {
        if (p->lanes[n].has_output &&
            fabs(p->lanes[n].output_pts - pts) <= tolerance)
            return &p->lanes[n];
    }
    return NULL;
}

static struct mp_image *render_output(struct mp_filter *f,
                                      bool force_source, double pts)
{
    struct priv *p = f->priv;
    struct mp_image *out = alloc_output(f, p->current, pts);
    if (!out)
        return NULL;
    ID3D11RenderTargetView *rtv = create_output_rtv(f, out);
    if (!rtv) {
        talloc_free(out);
        return NULL;
    }

    double epsilon = 0.001 / p->opts->target_fps;
    ID3D11ShaderResourceView *source = NULL;
    if (force_source || pts <= p->current->pts + epsilon) {
        source = p->input_srvs[p->current_input];
    } else {
        struct fruc_lane *lane = find_output_lane(p, pts);
        if (!lane) {
            MP_ERR(f, "NvOFFRUC has no synthesized frame for pts=%.6f\n",
                   pts);
            ID3D11RenderTargetView_Release(rtv);
            talloc_free(out);
            return NULL;
        }
        if (!wait_for_fence(f, lane, lane->completion_value)) {
            ID3D11RenderTargetView_Release(rtv);
            talloc_free(out);
            return NULL;
        }
        ID3D11DeviceContext_CopyResource(
            p->context, (ID3D11Resource *)lane->display_output,
            (ID3D11Resource *)lane->output);
        source = lane->output_srv;
    }
    draw_copy(f, source, rtv);
    ID3D11RenderTargetView_Release(rtv);
    p->output_frames++;
    return out;
}

static bool same_static_format(struct mp_image *a, struct mp_image *b)
{
    return a->hwctx && b->hwctx && a->hwctx->data == b->hwctx->data &&
           a->params.hw_subfmt == b->params.hw_subfmt &&
           mp_image_params_static_equal(&a->params, &b->params);
}

static double source_duration(struct mp_image *img)
{
    if (img->pkt_duration > 0 && img->pkt_duration < 1)
        return img->pkt_duration;
    if (img->nominal_fps > 1 && img->nominal_fps < 240)
        return 1.0 / img->nominal_fps;
    return 1.0 / 24.0;
}

static void clear_frames(struct priv *p)
{
    mp_image_unrefp(&p->current);
    mp_image_unrefp(&p->future);
    mp_image_unrefp(&p->pending_reset);
    p->eof = false;
    p->pair_prepared = false;
    p->output_clock_valid = false;
    p->output_pts = MP_NOPTS_VALUE;
}

static void reset_filter(struct mp_filter *f)
{
    struct priv *p = f->priv;
    clear_frames(p);
    release_resources(f);
    p->timeline_resets++;
    MP_VERBOSE(f, "NvOFFRUC timeline and lane state reset\n");
}

static bool prepare_single(struct mp_filter *f)
{
    return ensure_current_input(f);
}

static void fail_filter(struct mp_filter *f)
{
    mp_filter_internal_mark_failed(f);
}

static void process(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!mp_pin_in_needs_data(f->ppins[1]))
        return;

    if (p->future) {
        double delta = p->future->pts - p->current->pts;
        double expected = source_duration(p->current);
        if (delta <= 0 || delta > MPMAX(0.25, expected * 4.0)) {
            MP_WARN(f, "NvOFFRUC discontinuity current=%.6f future=%.6f delta=%.6f; resetting lanes\n",
                    p->current->pts, p->future->pts, delta);
            p->pending_reset = p->future;
            p->future = NULL;
            p->pair_prepared = false;
            p->timeline_resets++;
            mp_filter_internal_mark_progress(f);
            return;
        }
        if (!p->output_clock_valid) {
            p->output_pts = p->current->pts;
            p->output_clock_valid = true;
        }
        if (p->output_pts < p->future->pts - 1e-9) {
            if (!prepare_pair(f)) {
                fail_filter(f);
                return;
            }
            bool source = p->output_pts <= p->current->pts +
                          0.001 / p->opts->target_fps;
            struct mp_image *out =
                render_output(f, source, p->output_pts);
            if (!out) {
                fail_filter(f);
                return;
            }
            p->output_pts += 1.0 / p->opts->target_fps;
            mp_pin_in_write(f->ppins[1],
                            MAKE_FRAME(MP_FRAME_VIDEO, out));
            return;
        }
        mp_image_unrefp(&p->current);
        p->current = p->future;
        p->future = NULL;
        p->current_input = p->future_input;
        p->api_input_timestamp += 1.0;
        p->pair_prepared = false;
        mp_filter_internal_mark_progress(f);
        return;
    }

    if (p->pending_reset) {
        if (p->current) {
            if (!prepare_single(f)) {
                fail_filter(f);
                return;
            }
            double pts = p->output_clock_valid
                ? MPMAX(p->output_pts, p->current->pts)
                : p->current->pts;
            struct mp_image *out = render_output(f, true, pts);
            if (!out) {
                fail_filter(f);
                return;
            }
            mp_image_unrefp(&p->current);
            p->output_pts = pts + 1.0 / p->opts->target_fps;
            p->output_clock_valid = true;
            mp_pin_in_write(f->ppins[1],
                            MAKE_FRAME(MP_FRAME_VIDEO, out));
            return;
        }
        struct mp_image *next = p->pending_reset;
        p->pending_reset = NULL;
        if (!validate_input(f, next) || !init_resources(f, next)) {
            talloc_free(next);
            fail_filter(f);
            return;
        }
        p->current = next;
        p->output_pts = next->pts;
        p->output_clock_valid = true;
        mp_filter_internal_mark_progress(f);
        return;
    }

    if (p->eof) {
        if (p->current) {
            if (!p->output_clock_valid) {
                p->output_pts = p->current->pts;
                p->output_clock_valid = true;
            }
            double end = p->current->pts + source_duration(p->current);
            if (p->output_pts < end - 1e-9) {
                if (!prepare_single(f)) {
                    fail_filter(f);
                    return;
                }
                struct mp_image *out = render_output(
                    f, true, MPMAX(p->output_pts, p->current->pts));
                if (!out) {
                    fail_filter(f);
                    return;
                }
                p->output_pts += 1.0 / p->opts->target_fps;
                mp_pin_in_write(f->ppins[1],
                                MAKE_FRAME(MP_FRAME_VIDEO, out));
                return;
            }
            mp_image_unrefp(&p->current);
        }
        p->eof = false;
        p->output_clock_valid = false;
        mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_EOF, NULL));
        return;
    }

    if (!mp_pin_out_request_data(f->ppins[0]))
        return;
    struct mp_frame frame = mp_pin_out_read(f->ppins[0]);
    if (frame.type == MP_FRAME_EOF) {
        p->eof = true;
        mp_filter_internal_mark_progress(f);
        return;
    }
    if (frame.type != MP_FRAME_VIDEO) {
        MP_ERR(f, "NvOFFRUC received unsupported frame type=%d\n",
               frame.type);
        mp_frame_unref(&frame);
        fail_filter(f);
        return;
    }
    struct mp_image *img = frame.data;
    if (!validate_input(f, img)) {
        talloc_free(img);
        fail_filter(f);
        return;
    }
    if (!p->current) {
        if (!p->resources_ready ||
            !mp_image_params_static_equal(&p->input_params, &img->params) ||
            p->input_subfmt != img->params.hw_subfmt) {
            if (!init_resources(f, img)) {
                talloc_free(img);
                fail_filter(f);
                return;
            }
        }
        p->current = img;
        p->output_pts = img->pts;
        p->output_clock_valid = true;
    } else if (!same_static_format(p->current, img)) {
        MP_INFO(f, "NvOFFRUC input format changed; draining and reinitializing\n");
        p->pending_reset = img;
        p->timeline_resets++;
    } else {
        p->future = img;
    }
    mp_filter_internal_mark_progress(f);
}

static void destroy(struct mp_filter *f)
{
    struct priv *p = f->priv;
    clear_frames(p);
    release_resources(f);
    double average = p->fruc_process_calls
        ? p->fruc_submit_total_ms / p->fruc_process_calls : 0;
    MP_INFO(f, "NvOFFRUC shutdown backend=D3D11-ARGB lanes=%d source-pairs=%llu output-frames=%llu process-calls=%llu synthesized=%llu skip-warp=%llu repeated=%llu failures=%llu timeline-resets=%llu submit-ms-avg=%.3f submit-ms-max=%.3f\n",
            FRUC_LANE_COUNT,
            (unsigned long long)p->source_pairs,
            (unsigned long long)p->output_frames,
            (unsigned long long)p->fruc_process_calls,
            (unsigned long long)p->fruc_output_calls,
            (unsigned long long)p->fruc_skip_calls,
            (unsigned long long)p->fruc_repeated_frames,
            (unsigned long long)p->fruc_failures,
            (unsigned long long)p->timeline_resets,
            average, p->fruc_submit_max_ms);
    if (p->fence_event)
        CloseHandle(p->fence_event);
    p->fence_event = NULL;
    if (p->fruc_module)
        FreeLibrary(p->fruc_module);
    p->fruc_module = NULL;
    if (p->compiler_module)
        FreeLibrary(p->compiler_module);
    p->compiler_module = NULL;
    RELEASE_COM(p->video_context1, ID3D11VideoContext1_Release);
    RELEASE_COM(p->video_context, ID3D11VideoContext_Release);
    RELEASE_COM(p->video_device, ID3D11VideoDevice_Release);
    RELEASE_COM(p->context4, ID3D11DeviceContext4_Release);
    RELEASE_COM(p->context, ID3D11DeviceContext_Release);
    RELEASE_COM(p->device5, ID3D11Device5_Release);
    RELEASE_COM(p->device, ID3D11Device_Release);
    av_buffer_unref(&p->av_device_ref);
}

static const struct mp_filter_info nvofa_filter = {
    .name = "nvofa",
    .process = process,
    .reset = reset_filter,
    .destroy = destroy,
    .priv_size = sizeof(struct priv),
};

static bool load_fruc_runtime(struct mp_filter *f)
{
    struct priv *p = f->priv;
    DWORD flags = LOAD_LIBRARY_SEARCH_APPLICATION_DIR |
                  LOAD_LIBRARY_SEARCH_SYSTEM32;
    p->fruc_module = LoadLibraryExW(L"NvOFFRUC.dll", NULL, flags);
    if (!p->fruc_module) {
        MP_ERR(f, "NvOFFRUC.dll could not be loaded error=%lu\n",
               GetLastError());
        return false;
    }
    p->fruc_create = (fruc_create_fn)GetProcAddress(
        p->fruc_module, "NvOFFRUCCreate");
    p->fruc_register = (fruc_register_fn)GetProcAddress(
        p->fruc_module, "NvOFFRUCRegisterResource");
    p->fruc_unregister = (fruc_unregister_fn)GetProcAddress(
        p->fruc_module, "NvOFFRUCUnregisterResource");
    p->fruc_process = (fruc_process_fn)GetProcAddress(
        p->fruc_module, "NvOFFRUCProcess");
    p->fruc_destroy = (fruc_destroy_fn)GetProcAddress(
        p->fruc_module, "NvOFFRUCDestroy");
    if (!p->fruc_create || !p->fruc_register || !p->fruc_unregister ||
        !p->fruc_process || !p->fruc_destroy) {
        MP_ERR(f, "NvOFFRUC.dll is missing required SDK 5.0.7 exports\n");
        return false;
    }
    return true;
}

static struct mp_filter *create(struct mp_filter *parent, void *options)
{
    struct mp_filter *f = mp_filter_create(parent, &nvofa_filter);
    if (!f) {
        talloc_free(options);
        return NULL;
    }
    mp_filter_add_pin(f, MP_PIN_IN, "in");
    mp_filter_add_pin(f, MP_PIN_OUT, "out");
    struct priv *p = f->priv;
    p->opts = talloc_steal(p, options);
    p->output_pts = MP_NOPTS_VALUE;
    QueryPerformanceFrequency(&p->performance_frequency);

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
        MP_ERR(f, "NvOFFRUC could not obtain mpv's D3D11 device\n");
        goto fail;
    }
    p->av_device_ref = av_buffer_ref(hwctx->av_device_ref);
    AVHWDeviceContext *av_device = (AVHWDeviceContext *)p->av_device_ref->data;
    AVD3D11VADeviceContext *d3d = av_device->hwctx;
    p->device = d3d->device;
    ID3D11Device_AddRef(p->device);
    ID3D11Device_GetImmediateContext(p->device, &p->context);
    if (!p->context ||
        !check_hr(f, "QueryInterface(ID3D11Device5)",
                  ID3D11Device_QueryInterface(
                      p->device, &IID_ID3D11Device5,
                      (void **)&p->device5)) ||
        !check_hr(f, "QueryInterface(ID3D11DeviceContext4)",
                  ID3D11DeviceContext_QueryInterface(
                      p->context, &IID_ID3D11DeviceContext4,
                      (void **)&p->context4)) ||
        !check_hr(f, "QueryInterface(ID3D11VideoDevice)",
                  ID3D11Device_QueryInterface(
                      p->device, &IID_ID3D11VideoDevice,
                      (void **)&p->video_device)) ||
        !check_hr(f, "QueryInterface(ID3D11VideoContext)",
                  ID3D11DeviceContext_QueryInterface(
                      p->context, &IID_ID3D11VideoContext,
                      (void **)&p->video_context)) ||
        !check_hr(f, "QueryInterface(ID3D11VideoContext1)",
                  ID3D11VideoContext_QueryInterface(
                      p->video_context, &IID_ID3D11VideoContext1,
                      (void **)&p->video_context1))) {
        MP_ERR(f, "NvOFFRUC requires Windows 10 1703+ D3D11 fence support\n");
        goto fail;
    }

    p->fence_event = CreateEventW(NULL, FALSE, FALSE, NULL);
    if (!p->fence_event) {
        MP_ERR(f, "NvOFFRUC could not create fence event error=%lu\n",
               GetLastError());
        goto fail;
    }

    p->compiler_module = LoadLibraryExW(
        L"d3dcompiler_47.dll", NULL, LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!p->compiler_module) {
        MP_ERR(f, "NvOFFRUC could not load d3dcompiler_47.dll error=%lu\n",
               GetLastError());
        goto fail;
    }
    p->compile = (d3d_compile_fn)GetProcAddress(
        p->compiler_module, "D3DCompile");
    if (!p->compile) {
        MP_ERR(f, "NvOFFRUC D3DCompile entry point is unavailable\n");
        goto fail;
    }
    if (!load_fruc_runtime(f))
        goto fail;
    return f;

fail:
    talloc_free(f);
    return NULL;
}

#define OPT_BASE_STRUCT struct opts
static const m_option_t option_fields[] = {
    {"target-fps", OPT_DOUBLE(target_fps), M_RANGE(50.0, 61.0)},
    {0}
};

const struct mp_user_filter_entry vf_nvofa = {
    .desc = {
        .description = "NVIDIA NvOFFRUC D3D11 ARGB frame interpolation",
        .name = "nvofa",
        .priv_size = sizeof(OPT_BASE_STRUCT),
        .priv_defaults = &(const OPT_BASE_STRUCT) {
            .target_fps = 60.0,
        },
        .options = option_fields,
    },
    .create = create,
};
