/*
 * MediaStationGo NVIDIA Optical Flow frame interpolation filter for mpv.
 *
 * The filter keeps decoded video on mpv's D3D11 device. NV12/P010 input is
 * converted to RGB10 for synthesis and to R8 luma for NVOFA motion analysis.
 */

#include <windows.h>
#include <d3d11.h>
#include <d3d11_1.h>
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

#include "../../../nvofapi/include/nvOpticalFlowD3D11.h"

#define RELEASE_COM(value, release_fn) do { \
    if (value) { \
        release_fn(value); \
        value = NULL; \
    } \
} while (0)

struct opts {
    double target_fps;
    double scene_threshold;
};

struct of_resource {
    ID3D11Texture2D *texture;
    NvOFGPUBufferHandle handle;
};

typedef NV_OF_STATUS (NVOFAPI *get_max_version_fn)(uint32_t *version);
typedef NV_OF_STATUS (NVOFAPI *create_instance_d3d11_fn)(
    uint32_t version, NV_OF_D3D11_API_FUNCTION_LIST *functions);
typedef HRESULT (WINAPI *d3d_compile_fn)(
    const void *source, SIZE_T source_size, const char *source_name,
    const D3D_SHADER_MACRO *defines, ID3DInclude *include,
    const char *entrypoint, const char *target, UINT flags1, UINT flags2,
    ID3DBlob **code, ID3DBlob **errors);

struct shader_constants {
    float alpha;
    float flow_grid;
    float frame_width;
    float frame_height;
    float scene_cut;
    float reserved[3];
};

struct priv {
    struct opts *opts;
    struct mp_image *current;
    struct mp_image *future;
    struct mp_image *pending_reset;
    bool eof;
    bool pair_prepared;
    bool pair_scene_cut;
    bool resources_ready;
    bool output_clock_valid;
    double output_pts;
    double last_scene_score;
    uint64_t source_pairs;
    uint64_t output_frames;
    uint64_t scene_cuts;
    uint64_t timeline_resets;

    struct mp_image_params input_params;
    enum mp_imgfmt input_subfmt;
    DXGI_FORMAT input_dxgi;
    DXGI_COLOR_SPACE_TYPE input_csp;
    DXGI_COLOR_SPACE_TYPE output_csp;
    UINT width;
    UINT height;
    UINT flow_grid;

    AVBufferRef *av_device_ref;
    AVBufferRef *output_pool;
    ID3D11Device *device;
    ID3D11DeviceContext *context;
    ID3D11VideoDevice *video_device;
    ID3D11VideoContext *video_context;
    ID3D11VideoContext1 *video_context1;
    ID3D11VideoProcessorEnumerator *vp_enumerator;
    ID3D11VideoProcessor *video_processor;

    HMODULE nvofa_module;
    NV_OF_D3D11_API_FUNCTION_LIST nvofa;
    NvOFHandle nvofa_handle;
    uint32_t nvofa_api_version;
    struct of_resource luma_a;
    struct of_resource luma_b;
    struct of_resource flow_forward;
    struct of_resource flow_backward;

    HMODULE compiler_module;
    d3d_compile_fn compile;
    ID3D11VertexShader *vertex_shader;
    ID3D11PixelShader *luma_shader;
    ID3D11PixelShader *scene_shader;
    ID3D11PixelShader *interpolation_shader;
    ID3D11SamplerState *sampler;
    ID3D11Buffer *constants;

    ID3D11Texture2D *rgb_a;
    ID3D11Texture2D *rgb_b;
    ID3D11ShaderResourceView *rgb_a_srv;
    ID3D11ShaderResourceView *rgb_b_srv;
    ID3D11VideoProcessorOutputView *rgb_a_vp_view;
    ID3D11VideoProcessorOutputView *rgb_b_vp_view;
    ID3D11RenderTargetView *luma_a_rtv;
    ID3D11RenderTargetView *luma_b_rtv;
    ID3D11ShaderResourceView *flow_forward_srv;
    ID3D11ShaderResourceView *flow_backward_srv;
    ID3D11Texture2D *scene_texture;
    ID3D11RenderTargetView *scene_rtv;
    ID3D11Texture2D *scene_staging;
};

static const char shader_source[] =
    "cbuffer Params : register(b0) {"
    " float alpha; float flow_grid; float frame_width; float frame_height;"
    " float scene_cut; float3 reserved; };"
    "Texture2D<float4> source_a : register(t0);"
    "Texture2D<float4> source_b : register(t1);"
    "Texture2D<int2> flow_forward : register(t2);"
    "Texture2D<int2> flow_backward : register(t3);"
    "SamplerState source_sampler : register(s0);"
    "struct VSOut { float4 position : SV_Position; float2 uv : TEXCOORD0; };"
    "VSOut vertex_main(uint id : SV_VertexID) {"
    " VSOut o; float2 p=float2((id<<1)&2,id&2); o.uv=p;"
    " o.position=float4(p*float2(2,-2)+float2(-1,1),0,1); return o; }"
    "float luma(float3 v) { return dot(v,float3(0.2627,0.6780,0.0593)); }"
    "float pixel_luma(VSOut i) : SV_Target {"
    " return luma(source_a.SampleLevel(source_sampler,i.uv,0).rgb); }"
    "float2 pixel_scene(VSOut i) : SV_Target {"
    " return float2(luma(source_a.SampleLevel(source_sampler,i.uv,0).rgb),"
    "               luma(source_b.SampleLevel(source_sampler,i.uv,0).rgb)); }"
    "float2 load_flow(Texture2D<int2> tex,float2 pixel) {"
    " uint w,h; tex.GetDimensions(w,h); float2 c=pixel/flow_grid-0.5;"
    " float2 b=floor(c),q=c-b; int2 m=int2(w-1,h-1);"
    " int2 p00=clamp(int2(b),int2(0,0),m);"
    " int2 p10=clamp(p00+int2(1,0),int2(0,0),m);"
    " int2 p01=clamp(p00+int2(0,1),int2(0,0),m);"
    " int2 p11=clamp(p00+int2(1,1),int2(0,0),m);"
    " float2 f00=float2(tex.Load(int3(p00,0)))/32.0;"
    " float2 f10=float2(tex.Load(int3(p10,0)))/32.0;"
    " float2 f01=float2(tex.Load(int3(p01,0)))/32.0;"
    " float2 f11=float2(tex.Load(int3(p11,0)))/32.0;"
    " return lerp(lerp(f00,f10,q.x),lerp(f01,f11,q.x),q.y); }"
    "float4 pixel_interpolate(VSOut i) : SV_Target {"
    " if(scene_cut>0.5) return alpha<0.5"
    "   ? source_a.SampleLevel(source_sampler,i.uv,0)"
    "   : source_b.SampleLevel(source_sampler,i.uv,0);"
    " float2 size=float2(frame_width,frame_height),pixel=i.uv*size;"
    " float2 f=load_flow(flow_forward,pixel);"
    " float2 b=load_flow(flow_backward,pixel);"
    " float2 ua=clamp((pixel-alpha*f)/size,0.0,1.0);"
    " float2 ub=clamp((pixel-(1.0-alpha)*b)/size,0.0,1.0);"
    " float4 a=source_a.SampleLevel(source_sampler,ua,0);"
    " float4 z=source_b.SampleLevel(source_sampler,ub,0);"
    " float2 reverse=load_flow(flow_backward,pixel+f);"
    " float consistency=length(f+reverse);"
    " float difference=max(max(abs(a.r-z.r),abs(a.g-z.g)),abs(a.b-z.b));"
    " if(consistency>6.0||difference>0.45) return alpha<0.5?a:z;"
    " return lerp(a,z,alpha); }";

static const char *of_status_name(NV_OF_STATUS status)
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

static bool check_hr(struct mp_filter *f, const char *operation, HRESULT hr)
{
    if (SUCCEEDED(hr))
        return true;
    MP_ERR(f, "NVOFA operation=%s failed hresult=%#lx\n", operation, hr);
    return false;
}

static bool check_of(struct mp_filter *f, const char *operation,
                     NV_OF_STATUS status)
{
    if (status == NV_OF_SUCCESS)
        return true;
    MP_ERR(f, "NVOFA operation=%s failed status=%u name=%s\n", operation,
           (unsigned)status, of_status_name(status));
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
        MP_ERR(f, "NVOFA requires D3D11 hardware frames; received %s\n",
               mp_imgfmt_to_name(img->imgfmt));
        return false;
    }
    if (img->params.hw_subfmt != IMGFMT_NV12 &&
        img->params.hw_subfmt != IMGFMT_P010) {
        MP_ERR(f, "NVOFA supports only NV12/P010 input; received %s\n",
               mp_imgfmt_to_name(img->params.hw_subfmt));
        return false;
    }
    if (img->dovi || img->params.repr.sys == PL_COLOR_SYSTEM_DOLBYVISION) {
        MP_ERR(f, "NVOFA does not support Dolby Vision input\n");
        return false;
    }
    if (has_dynamic_hdr10_plus(img)) {
        MP_ERR(f, "NVOFA does not support HDR10+ dynamic metadata\n");
        return false;
    }
    if (img->params.color.transfer == PL_COLOR_TRC_HLG) {
        MP_ERR(f, "NVOFA HLG output has not been validated\n");
        return false;
    }
    if (img->params.color.transfer == PL_COLOR_TRC_PQ &&
        img->params.hw_subfmt != IMGFMT_P010) {
        MP_ERR(f, "NVOFA refuses HDR10 carried in an 8-bit surface\n");
        return false;
    }
    if (img->pts == MP_NOPTS_VALUE || !isfinite(img->pts)) {
        MP_ERR(f, "NVOFA requires finite source timestamps\n");
        return false;
    }
    return true;
}

static bool create_texture(struct mp_filter *f, UINT width, UINT height,
                           DXGI_FORMAT format, UINT bind_flags,
                           D3D11_USAGE usage, UINT cpu_flags,
                           ID3D11Texture2D **texture)
{
    struct priv *p = f->priv;
    D3D11_TEXTURE2D_DESC desc = {
        .Width = width,
        .Height = height,
        .MipLevels = 1,
        .ArraySize = 1,
        .Format = format,
        .SampleDesc = { .Count = 1 },
        .Usage = usage,
        .BindFlags = bind_flags,
        .CPUAccessFlags = cpu_flags,
    };
    return check_hr(f, "CreateTexture2D",
                    ID3D11Device_CreateTexture2D(p->device, &desc, NULL,
                                                 texture));
}

static bool query_cap_values(struct mp_filter *f, NV_OF_CAPS cap,
                             uint32_t **values, uint32_t *count)
{
    struct priv *p = f->priv;
    *count = 0;
    if (!check_of(f, "nvOFGetCaps(count)",
                  p->nvofa.nvOFGetCaps(p->nvofa_handle, cap, NULL, count)))
        return false;
    *values = talloc_zero_array(NULL, uint32_t, *count);
    if (*count && !*values)
        return false;
    if (*count && !check_of(f, "nvOFGetCaps(values)",
                            p->nvofa.nvOFGetCaps(p->nvofa_handle, cap,
                                                *values, count))) {
        talloc_free(*values);
        *values = NULL;
        return false;
    }
    return true;
}

static bool nvofa_has_format(struct mp_filter *f, NV_OF_BUFFER_USAGE usage,
                             DXGI_FORMAT expected)
{
    struct priv *p = f->priv;
    uint32_t count = 0;
    if (!check_of(f, "nvOFGetSurfaceFormatCountD3D11",
                  p->nvofa.nvOFGetSurfaceFormatCountD3D11(
                      p->nvofa_handle, usage, NV_OF_MODE_OPTICALFLOW, &count)))
        return false;
    DXGI_FORMAT *formats = talloc_zero_array(NULL, DXGI_FORMAT, count);
    if (count && !formats)
        return false;
    bool ok = check_of(f, "nvOFGetSurfaceFormatD3D11",
                       p->nvofa.nvOFGetSurfaceFormatD3D11(
                           p->nvofa_handle, usage, NV_OF_MODE_OPTICALFLOW,
                           formats));
    bool found = false;
    for (uint32_t n = 0; ok && n < count; n++)
        found |= formats[n] == expected;
    talloc_free(formats);
    return ok && found;
}

static bool register_of_resource(struct mp_filter *f,
                                 struct of_resource *resource)
{
    struct priv *p = f->priv;
    return check_of(f, "nvOFRegisterResourceD3D11",
                    p->nvofa.nvOFRegisterResourceD3D11(
                        p->nvofa_handle,
                        (ID3D11Resource *)resource->texture,
                        &resource->handle));
}

static void unregister_of_resource(struct priv *p,
                                   struct of_resource *resource)
{
    if (resource->handle && p->nvofa.nvOFUnregisterResourceD3D11)
        p->nvofa.nvOFUnregisterResourceD3D11(resource->handle);
    resource->handle = NULL;
}

static bool compile_shader(struct mp_filter *f, const char *entrypoint,
                           const char *target, ID3DBlob **bytecode)
{
    struct priv *p = f->priv;
    ID3DBlob *errors = NULL;
    HRESULT hr = p->compile(shader_source, sizeof(shader_source) - 1,
                            "vf_nvofa.hlsl", NULL, NULL, entrypoint, target,
                            D3DCOMPILE_ENABLE_STRICTNESS |
                                D3DCOMPILE_OPTIMIZATION_LEVEL3,
                            0, bytecode, &errors);
    if (FAILED(hr) && errors) {
        MP_ERR(f, "NVOFA shader %s failed: %s\n", entrypoint,
               (const char *)ID3D10Blob_GetBufferPointer(errors));
    }
    RELEASE_COM(errors, ID3D10Blob_Release);
    return check_hr(f, entrypoint, hr);
}

static bool create_shaders(struct mp_filter *f)
{
    struct priv *p = f->priv;
    ID3DBlob *vs = NULL, *luma = NULL, *scene = NULL, *interpolate = NULL;
    bool ok = compile_shader(f, "vertex_main", "vs_5_0", &vs) &&
              compile_shader(f, "pixel_luma", "ps_5_0", &luma) &&
              compile_shader(f, "pixel_scene", "ps_5_0", &scene) &&
              compile_shader(f, "pixel_interpolate", "ps_5_0", &interpolate);
    if (!ok)
        goto done;
    ok = check_hr(f, "CreateVertexShader",
                  ID3D11Device_CreateVertexShader(
                      p->device, ID3D10Blob_GetBufferPointer(vs),
                      ID3D10Blob_GetBufferSize(vs), NULL, &p->vertex_shader)) &&
         check_hr(f, "CreatePixelShader(luma)",
                  ID3D11Device_CreatePixelShader(
                      p->device, ID3D10Blob_GetBufferPointer(luma),
                      ID3D10Blob_GetBufferSize(luma), NULL, &p->luma_shader)) &&
         check_hr(f, "CreatePixelShader(scene)",
                  ID3D11Device_CreatePixelShader(
                      p->device, ID3D10Blob_GetBufferPointer(scene),
                      ID3D10Blob_GetBufferSize(scene), NULL, &p->scene_shader)) &&
         check_hr(f, "CreatePixelShader(interpolate)",
                  ID3D11Device_CreatePixelShader(
                      p->device, ID3D10Blob_GetBufferPointer(interpolate),
                      ID3D10Blob_GetBufferSize(interpolate), NULL,
                      &p->interpolation_shader));
done:
    RELEASE_COM(vs, ID3D10Blob_Release);
    RELEASE_COM(luma, ID3D10Blob_Release);
    RELEASE_COM(scene, ID3D10Blob_Release);
    RELEASE_COM(interpolate, ID3D10Blob_Release);
    if (!ok)
        return false;

    D3D11_SAMPLER_DESC sampler = {
        .Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR,
        .AddressU = D3D11_TEXTURE_ADDRESS_CLAMP,
        .AddressV = D3D11_TEXTURE_ADDRESS_CLAMP,
        .AddressW = D3D11_TEXTURE_ADDRESS_CLAMP,
        .MaxLOD = D3D11_FLOAT32_MAX,
    };
    D3D11_BUFFER_DESC constants = {
        .ByteWidth = sizeof(struct shader_constants),
        .Usage = D3D11_USAGE_DEFAULT,
        .BindFlags = D3D11_BIND_CONSTANT_BUFFER,
    };
    return check_hr(f, "CreateSamplerState",
                    ID3D11Device_CreateSamplerState(p->device, &sampler,
                                                    &p->sampler)) &&
           check_hr(f, "CreateBuffer(constants)",
                    ID3D11Device_CreateBuffer(p->device, &constants, NULL,
                                              &p->constants));
}

static void release_resources(struct mp_filter *f)
{
    struct priv *p = f->priv;
    unregister_of_resource(p, &p->luma_a);
    unregister_of_resource(p, &p->luma_b);
    unregister_of_resource(p, &p->flow_forward);
    unregister_of_resource(p, &p->flow_backward);
    if (p->nvofa_handle && p->nvofa.nvOFDestroy)
        p->nvofa.nvOFDestroy(p->nvofa_handle);
    p->nvofa_handle = NULL;

    RELEASE_COM(p->scene_staging, ID3D11Texture2D_Release);
    RELEASE_COM(p->scene_rtv, ID3D11RenderTargetView_Release);
    RELEASE_COM(p->scene_texture, ID3D11Texture2D_Release);
    RELEASE_COM(p->flow_backward_srv, ID3D11ShaderResourceView_Release);
    RELEASE_COM(p->flow_forward_srv, ID3D11ShaderResourceView_Release);
    RELEASE_COM(p->luma_b_rtv, ID3D11RenderTargetView_Release);
    RELEASE_COM(p->luma_a_rtv, ID3D11RenderTargetView_Release);
    RELEASE_COM(p->rgb_b_vp_view, ID3D11VideoProcessorOutputView_Release);
    RELEASE_COM(p->rgb_a_vp_view, ID3D11VideoProcessorOutputView_Release);
    RELEASE_COM(p->rgb_b_srv, ID3D11ShaderResourceView_Release);
    RELEASE_COM(p->rgb_a_srv, ID3D11ShaderResourceView_Release);
    RELEASE_COM(p->rgb_b, ID3D11Texture2D_Release);
    RELEASE_COM(p->rgb_a, ID3D11Texture2D_Release);
    RELEASE_COM(p->flow_backward.texture, ID3D11Texture2D_Release);
    RELEASE_COM(p->flow_forward.texture, ID3D11Texture2D_Release);
    RELEASE_COM(p->luma_b.texture, ID3D11Texture2D_Release);
    RELEASE_COM(p->luma_a.texture, ID3D11Texture2D_Release);
    RELEASE_COM(p->constants, ID3D11Buffer_Release);
    RELEASE_COM(p->sampler, ID3D11SamplerState_Release);
    RELEASE_COM(p->interpolation_shader, ID3D11PixelShader_Release);
    RELEASE_COM(p->scene_shader, ID3D11PixelShader_Release);
    RELEASE_COM(p->luma_shader, ID3D11PixelShader_Release);
    RELEASE_COM(p->vertex_shader, ID3D11VertexShader_Release);
    RELEASE_COM(p->video_processor, ID3D11VideoProcessor_Release);
    RELEASE_COM(p->vp_enumerator, ID3D11VideoProcessorEnumerator_Release);
    av_buffer_unref(&p->output_pool);
    p->resources_ready = false;
    p->pair_prepared = false;
}

static bool init_video_processor(struct mp_filter *f, struct mp_image *img)
{
    struct priv *p = f->priv;
    p->input_dxgi = img->params.hw_subfmt == IMGFMT_P010
        ? DXGI_FORMAT_P010 : DXGI_FORMAT_NV12;
    p->input_csp = mp_params_to_dxgi_colorspace(f->log, &img->params);
    struct mp_image_params output_params = img->params;
    output_params.imgfmt = IMGFMT_D3D11;
    output_params.hw_subfmt = IMGFMT_X2BGR10;
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
    HRESULT hr = ID3D11VideoProcessorEnumerator1_CheckVideoProcessorFormatConversion(
        enumerator1, p->input_dxgi, p->input_csp,
        DXGI_FORMAT_R10G10B10A2_UNORM, p->output_csp,
        &conversion_supported);
    ID3D11VideoProcessorEnumerator1_Release(enumerator1);
    if (!check_hr(f, "CheckVideoProcessorFormatConversion", hr) ||
        !conversion_supported) {
        MP_ERR(f, "NVOFA video processor does not support source=%d input-csp=%d output-csp=%d\n",
               p->input_dxgi, p->input_csp, p->output_csp);
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

static bool init_nvofa(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!check_of(f, "nvCreateOpticalFlowD3D11",
                  p->nvofa.nvCreateOpticalFlowD3D11(
                      p->device, p->context, &p->nvofa_handle)))
        return false;

    uint32_t *grids = NULL, *max_width = NULL, *max_height = NULL;
    uint32_t grid_count = 0, width_count = 0, height_count = 0;
    bool ok = query_cap_values(f, NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES,
                               &grids, &grid_count) &&
              query_cap_values(f, NV_OF_CAPS_WIDTH_MAX,
                               &max_width, &width_count) &&
              query_cap_values(f, NV_OF_CAPS_HEIGHT_MAX,
                               &max_height, &height_count);
    if (!ok)
        goto done;
    p->flow_grid = 0;
    for (uint32_t n = 0; n < grid_count; n++) {
        if (grids[n] == 4)
            p->flow_grid = 4;
    }
    if (!p->flow_grid && grid_count)
        p->flow_grid = grids[grid_count - 1];
    if (!p->flow_grid || !width_count || !height_count ||
        p->width > max_width[0] || p->height > max_height[0]) {
        MP_ERR(f, "NVOFA unsupported dimensions=%ux%u maximum=%ux%u grid=%u\n",
               p->width, p->height, width_count ? max_width[0] : 0,
               height_count ? max_height[0] : 0, p->flow_grid);
        ok = false;
        goto done;
    }
    if (!nvofa_has_format(f, NV_OF_BUFFER_USAGE_INPUT, DXGI_FORMAT_R8_UNORM) ||
        !nvofa_has_format(f, NV_OF_BUFFER_USAGE_OUTPUT,
                          DXGI_FORMAT_R16G16_SINT)) {
        MP_ERR(f, "NVOFA required R8/R16G16_SINT formats are unavailable\n");
        ok = false;
        goto done;
    }

    NV_OF_INIT_PARAMS init = {
        .width = p->width,
        .height = p->height,
        .outGridSize = (NV_OF_OUTPUT_VECTOR_GRID_SIZE)p->flow_grid,
        .hintGridSize = NV_OF_HINT_VECTOR_GRID_SIZE_UNDEFINED,
        .mode = NV_OF_MODE_OPTICALFLOW,
        .perfLevel = NV_OF_PERF_LEVEL_FAST,
        .enableExternalHints = NV_OF_FALSE,
        .enableOutputCost = NV_OF_FALSE,
        .disparityRange = NV_OF_STEREO_DISPARITY_RANGE_UNDEFINED,
        .enableRoi = NV_OF_FALSE,
        .predDirection = NV_OF_PRED_DIRECTION_BOTH,
        .enableGlobalFlow = NV_OF_FALSE,
        .inputBufferFormat = NV_OF_BUFFER_FORMAT_GRAYSCALE8,
    };
    ok = check_of(f, "nvOFInit",
                  p->nvofa.nvOFInit(p->nvofa_handle, &init));
done:
    talloc_free(grids);
    talloc_free(max_width);
    talloc_free(max_height);
    return ok;
}

static bool create_processing_textures(struct mp_filter *f)
{
    struct priv *p = f->priv;
    UINT rgb_bind = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
    UINT luma_bind = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
    UINT flow_width = (p->width + p->flow_grid - 1) / p->flow_grid;
    UINT flow_height = (p->height + p->flow_grid - 1) / p->flow_grid;
    if (!create_texture(f, p->width, p->height,
                        DXGI_FORMAT_R10G10B10A2_UNORM, rgb_bind,
                        D3D11_USAGE_DEFAULT, 0, &p->rgb_a) ||
        !create_texture(f, p->width, p->height,
                        DXGI_FORMAT_R10G10B10A2_UNORM, rgb_bind,
                        D3D11_USAGE_DEFAULT, 0, &p->rgb_b) ||
        !create_texture(f, p->width, p->height, DXGI_FORMAT_R8_UNORM,
                        luma_bind, D3D11_USAGE_DEFAULT, 0,
                        &p->luma_a.texture) ||
        !create_texture(f, p->width, p->height, DXGI_FORMAT_R8_UNORM,
                        luma_bind, D3D11_USAGE_DEFAULT, 0,
                        &p->luma_b.texture) ||
        !create_texture(f, flow_width, flow_height,
                        DXGI_FORMAT_R16G16_SINT, D3D11_BIND_SHADER_RESOURCE,
                        D3D11_USAGE_DEFAULT, 0, &p->flow_forward.texture) ||
        !create_texture(f, flow_width, flow_height,
                        DXGI_FORMAT_R16G16_SINT, D3D11_BIND_SHADER_RESOURCE,
                        D3D11_USAGE_DEFAULT, 0, &p->flow_backward.texture))
        return false;

    if (!check_hr(f, "CreateShaderResourceView(rgb_a)",
                  ID3D11Device_CreateShaderResourceView(
                      p->device, (ID3D11Resource *)p->rgb_a, NULL,
                      &p->rgb_a_srv)) ||
        !check_hr(f, "CreateShaderResourceView(rgb_b)",
                  ID3D11Device_CreateShaderResourceView(
                      p->device, (ID3D11Resource *)p->rgb_b, NULL,
                      &p->rgb_b_srv)) ||
        !check_hr(f, "CreateRenderTargetView(luma_a)",
                  ID3D11Device_CreateRenderTargetView(
                      p->device, (ID3D11Resource *)p->luma_a.texture, NULL,
                      &p->luma_a_rtv)) ||
        !check_hr(f, "CreateRenderTargetView(luma_b)",
                  ID3D11Device_CreateRenderTargetView(
                      p->device, (ID3D11Resource *)p->luma_b.texture, NULL,
                      &p->luma_b_rtv)) ||
        !check_hr(f, "CreateShaderResourceView(flow_forward)",
                  ID3D11Device_CreateShaderResourceView(
                      p->device, (ID3D11Resource *)p->flow_forward.texture,
                      NULL, &p->flow_forward_srv)) ||
        !check_hr(f, "CreateShaderResourceView(flow_backward)",
                  ID3D11Device_CreateShaderResourceView(
                      p->device, (ID3D11Resource *)p->flow_backward.texture,
                      NULL, &p->flow_backward_srv)))
        return false;

    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC vp_output = {
        .ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D,
        .Texture2D = { .MipSlice = 0 },
    };
    if (!check_hr(f, "CreateVideoProcessorOutputView(rgb_a)",
                  ID3D11VideoDevice_CreateVideoProcessorOutputView(
                      p->video_device, (ID3D11Resource *)p->rgb_a,
                      p->vp_enumerator, &vp_output, &p->rgb_a_vp_view)) ||
        !check_hr(f, "CreateVideoProcessorOutputView(rgb_b)",
                  ID3D11VideoDevice_CreateVideoProcessorOutputView(
                      p->video_device, (ID3D11Resource *)p->rgb_b,
                      p->vp_enumerator, &vp_output, &p->rgb_b_vp_view)))
        return false;

    if (!register_of_resource(f, &p->luma_a) ||
        !register_of_resource(f, &p->luma_b) ||
        !register_of_resource(f, &p->flow_forward) ||
        !register_of_resource(f, &p->flow_backward))
        return false;

    if (!create_texture(f, 64, 36, DXGI_FORMAT_R8G8_UNORM,
                        D3D11_BIND_RENDER_TARGET, D3D11_USAGE_DEFAULT, 0,
                        &p->scene_texture) ||
        !check_hr(f, "CreateRenderTargetView(scene)",
                  ID3D11Device_CreateRenderTargetView(
                      p->device, (ID3D11Resource *)p->scene_texture, NULL,
                      &p->scene_rtv)) ||
        !create_texture(f, 64, 36, DXGI_FORMAT_R8G8_UNORM, 0,
                        D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ,
                        &p->scene_staging))
        return false;
    return true;
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
        MP_ERR(f, "NVOFA refuses dimensions below 320x240\n");
        return false;
    }
    if (img->nominal_fps > 0 && p->opts->target_fps <= img->nominal_fps + 0.01) {
        MP_ERR(f, "NVOFA target %.3f must exceed source %.3f fps\n",
               p->opts->target_fps, img->nominal_fps);
        return false;
    }
    if (!create_shaders(f) || !init_video_processor(f, img) ||
        !init_nvofa(f) || !create_processing_textures(f)) {
        release_resources(f);
        return false;
    }
    p->resources_ready = true;
    MP_INFO(f, "NVOFA initialized api=%u.%u source=%s resolution=%ux%u target-fps=%.3f grid=%u input-csp=%d output-csp=%d\n",
            p->nvofa_api_version >> 4, p->nvofa_api_version & 0xf,
            mp_imgfmt_to_name(p->input_subfmt), p->width, p->height,
            p->opts->target_fps, p->flow_grid, p->input_csp, p->output_csp);
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
                          ID3D11VideoProcessorOutputView *output, UINT index)
{
    struct priv *p = f->priv;
    ID3D11VideoProcessorInputView *input = create_input_view(f, img);
    if (!input)
        return false;
    D3D11_VIDEO_PROCESSOR_STREAM stream = {
        .Enable = TRUE,
        .OutputIndex = 0,
        .InputFrameOrField = index,
        .pInputSurface = input,
    };
    HRESULT hr = ID3D11VideoContext_VideoProcessorBlt(
        p->video_context, p->video_processor, output, index, 1, &stream);
    ID3D11VideoProcessorInputView_Release(input);
    return check_hr(f, "VideoProcessorBlt", hr);
}

static void set_draw_state(struct priv *p, ID3D11PixelShader *shader,
                           UINT width, UINT height)
{
    D3D11_VIEWPORT viewport = {
        .Width = width,
        .Height = height,
        .MaxDepth = 1.0f,
    };
    ID3D11DeviceContext_IASetPrimitiveTopology(
        p->context, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_VSSetShader(p->context, p->vertex_shader, NULL, 0);
    ID3D11DeviceContext_PSSetShader(p->context, shader, NULL, 0);
    ID3D11DeviceContext_PSSetSamplers(p->context, 0, 1, &p->sampler);
    ID3D11DeviceContext_RSSetViewports(p->context, 1, &viewport);
}

static void unbind_draw_resources(struct priv *p)
{
    ID3D11ShaderResourceView *empty_srvs[4] = {0};
    ID3D11RenderTargetView *empty_rtv = NULL;
    ID3D11DeviceContext_PSSetShaderResources(p->context, 0, 4, empty_srvs);
    ID3D11DeviceContext_OMSetRenderTargets(p->context, 1, &empty_rtv, NULL);
}

static void extract_luma(struct priv *p)
{
    set_draw_state(p, p->luma_shader, p->width, p->height);
    ID3D11DeviceContext_PSSetShaderResources(p->context, 0, 1, &p->rgb_a_srv);
    ID3D11DeviceContext_OMSetRenderTargets(p->context, 1, &p->luma_a_rtv, NULL);
    ID3D11DeviceContext_Draw(p->context, 3, 0);
    unbind_draw_resources(p);
    ID3D11DeviceContext_PSSetShaderResources(p->context, 0, 1, &p->rgb_b_srv);
    ID3D11DeviceContext_OMSetRenderTargets(p->context, 1, &p->luma_b_rtv, NULL);
    ID3D11DeviceContext_Draw(p->context, 3, 0);
    unbind_draw_resources(p);
}

static bool detect_scene_cut(struct mp_filter *f, bool *scene_cut)
{
    struct priv *p = f->priv;
    set_draw_state(p, p->scene_shader, 64, 36);
    ID3D11ShaderResourceView *sources[2] = {p->rgb_a_srv, p->rgb_b_srv};
    ID3D11DeviceContext_PSSetShaderResources(p->context, 0, 2, sources);
    ID3D11DeviceContext_OMSetRenderTargets(p->context, 1, &p->scene_rtv, NULL);
    ID3D11DeviceContext_Draw(p->context, 3, 0);
    unbind_draw_resources(p);
    ID3D11DeviceContext_CopyResource(p->context,
                                    (ID3D11Resource *)p->scene_staging,
                                    (ID3D11Resource *)p->scene_texture);

    D3D11_MAPPED_SUBRESOURCE mapped;
    if (!check_hr(f, "Map(scene histogram)",
                  ID3D11DeviceContext_Map(p->context,
                                         (ID3D11Resource *)p->scene_staging,
                                         0, D3D11_MAP_READ, 0, &mapped)))
        return false;
    uint32_t hist_a[32] = {0};
    uint32_t hist_b[32] = {0};
    for (UINT y = 0; y < 36; y++) {
        const uint8_t *row = (const uint8_t *)mapped.pData + y * mapped.RowPitch;
        for (UINT x = 0; x < 64; x++) {
            hist_a[row[x * 2] >> 3]++;
            hist_b[row[x * 2 + 1] >> 3]++;
        }
    }
    ID3D11DeviceContext_Unmap(p->context,
                              (ID3D11Resource *)p->scene_staging, 0);
    double difference = 0;
    for (int n = 0; n < 32; n++)
        difference += abs((int)hist_a[n] - (int)hist_b[n]);
    p->last_scene_score = difference / (2.0 * 64.0 * 36.0);
    *scene_cut = p->last_scene_score >= p->opts->scene_threshold;
    if (*scene_cut) {
        p->scene_cuts++;
        MP_VERBOSE(f, "NVOFA scene cut score=%.4f threshold=%.4f pts=%.6f\n",
                   p->last_scene_score, p->opts->scene_threshold,
                   p->current->pts);
    }
    return true;
}

static bool execute_flow(struct mp_filter *f)
{
    struct priv *p = f->priv;
    NV_OF_EXECUTE_INPUT_PARAMS input = {
        .inputFrame = p->luma_a.handle,
        .referenceFrame = p->luma_b.handle,
        .disableTemporalHints = NV_OF_TRUE,
    };
    NV_OF_EXECUTE_OUTPUT_PARAMS output = {
        .outputBuffer = p->flow_forward.handle,
        .bwdOutputBuffer = p->flow_backward.handle,
    };
    return check_of(f, "nvOFExecute",
                    p->nvofa.nvOFExecute(p->nvofa_handle, &input, &output));
}

static bool prepare_pair(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (p->pair_prepared)
        return true;
    if (!convert_frame(f, p->current, p->rgb_a_vp_view, 0) ||
        !convert_frame(f, p->future, p->rgb_b_vp_view, 1))
        return false;
    extract_luma(p);
    if (!detect_scene_cut(f, &p->pair_scene_cut))
        return false;
    if (!p->pair_scene_cut && !execute_flow(f))
        return false;
    p->pair_prepared = true;
    p->source_pairs++;
    return true;
}

static struct mp_image *alloc_output(struct mp_filter *f,
                                     struct mp_image *attributes,
                                     double pts)
{
    struct priv *p = f->priv;
    if (!mp_update_av_hw_frames_pool(&p->output_pool, p->av_device_ref,
                                     IMGFMT_D3D11, IMGFMT_X2BGR10,
                                     p->width, p->height, false)) {
        MP_ERR(f, "NVOFA failed to allocate RGB10 hardware pool\n");
        return NULL;
    }
    AVFrame *frame = av_frame_alloc();
    MP_HANDLE_OOM(frame);
    if (av_hwframe_get_buffer(p->output_pool, frame, 0) < 0) {
        MP_ERR(f, "NVOFA failed to allocate RGB10 output frame\n");
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
        view.Texture2DArray.FirstArraySlice = (uintptr_t)out->planes[1];
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

static struct mp_image *render_output(struct mp_filter *f, double alpha,
                                      bool force_nearest, double pts)
{
    struct priv *p = f->priv;
    struct mp_image *attributes = alpha < 0.5 ? p->current : p->future;
    if (!attributes)
        attributes = p->current;
    struct mp_image *out = alloc_output(f, attributes, pts);
    if (!out)
        return NULL;
    ID3D11RenderTargetView *rtv = create_output_rtv(f, out);
    if (!rtv) {
        talloc_free(out);
        return NULL;
    }
    struct shader_constants constants = {
        .alpha = alpha,
        .flow_grid = p->flow_grid,
        .frame_width = p->width,
        .frame_height = p->height,
        .scene_cut = force_nearest || p->pair_scene_cut,
    };
    ID3D11DeviceContext_UpdateSubresource(
        p->context, (ID3D11Resource *)p->constants, 0, NULL, &constants, 0, 0);
    set_draw_state(p, p->interpolation_shader, p->width, p->height);
    ID3D11ShaderResourceView *sources[4] = {
        p->rgb_a_srv, p->rgb_b_srv,
        p->flow_forward_srv, p->flow_backward_srv,
    };
    ID3D11DeviceContext_PSSetConstantBuffers(p->context, 0, 1, &p->constants);
    ID3D11DeviceContext_PSSetShaderResources(p->context, 0, 4, sources);
    ID3D11DeviceContext_OMSetRenderTargets(p->context, 1, &rtv, NULL);
    ID3D11DeviceContext_Draw(p->context, 3, 0);
    unbind_draw_resources(p);
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
    p->pair_scene_cut = false;
    p->output_clock_valid = false;
    p->output_pts = MP_NOPTS_VALUE;
}

static void reset_filter(struct mp_filter *f)
{
    struct priv *p = f->priv;
    clear_frames(p);
    p->timeline_resets++;
    MP_VERBOSE(f, "NVOFA timeline reset\n");
}

static bool prepare_single(struct mp_filter *f)
{
    struct priv *p = f->priv;
    if (!convert_frame(f, p->current, p->rgb_a_vp_view, 0))
        return false;
    p->pair_scene_cut = true;
    return true;
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
            MP_WARN(f, "NVOFA discontinuity current=%.6f future=%.6f delta=%.6f; resetting timeline\n",
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
            double alpha = (p->output_pts - p->current->pts) / delta;
            alpha = MPCLAMP(alpha, 0.0, 1.0);
            struct mp_image *out = render_output(
                f, alpha, false, p->output_pts);
            if (!out) {
                fail_filter(f);
                return;
            }
            p->output_pts += 1.0 / p->opts->target_fps;
            mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_VIDEO, out));
            return;
        }
        mp_image_unrefp(&p->current);
        p->current = p->future;
        p->future = NULL;
        p->pair_prepared = false;
        p->pair_scene_cut = false;
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
                ? MPMAX(p->output_pts, p->current->pts) : p->current->pts;
            struct mp_image *out = render_output(f, 0, true, pts);
            if (!out) {
                fail_filter(f);
                return;
            }
            mp_image_unrefp(&p->current);
            p->output_pts = pts + 1.0 / p->opts->target_fps;
            p->output_clock_valid = true;
            mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_VIDEO, out));
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
                    f, 0, true, MPMAX(p->output_pts, p->current->pts));
                if (!out) {
                    fail_filter(f);
                    return;
                }
                p->output_pts += 1.0 / p->opts->target_fps;
                mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_VIDEO, out));
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
        MP_ERR(f, "NVOFA received unsupported frame type=%d\n", frame.type);
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
        MP_INFO(f, "NVOFA input format changed; draining and reinitializing\n");
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
    MP_INFO(f, "NVOFA shutdown source-pairs=%llu output-frames=%llu scene-cuts=%llu timeline-resets=%llu last-scene-score=%.4f\n",
            (unsigned long long)p->source_pairs,
            (unsigned long long)p->output_frames,
            (unsigned long long)p->scene_cuts,
            (unsigned long long)p->timeline_resets,
            p->last_scene_score);
    clear_frames(p);
    release_resources(f);
    if (p->nvofa_module)
        FreeLibrary(p->nvofa_module);
    if (p->compiler_module)
        FreeLibrary(p->compiler_module);
    RELEASE_COM(p->video_context1, ID3D11VideoContext1_Release);
    RELEASE_COM(p->video_context, ID3D11VideoContext_Release);
    RELEASE_COM(p->video_device, ID3D11VideoDevice_Release);
    RELEASE_COM(p->context, ID3D11DeviceContext_Release);
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
        MP_ERR(f, "NVOFA could not obtain mpv's D3D11 device\n");
        goto fail;
    }
    p->av_device_ref = av_buffer_ref(hwctx->av_device_ref);
    AVHWDeviceContext *av_device = (AVHWDeviceContext *)p->av_device_ref->data;
    AVD3D11VADeviceContext *d3d = av_device->hwctx;
    p->device = d3d->device;
    ID3D11Device_AddRef(p->device);
    ID3D11Device_GetImmediateContext(p->device, &p->context);
    if (!p->context ||
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
                      (void **)&p->video_context1)))
        goto fail;

    p->compiler_module = LoadLibraryW(L"d3dcompiler_47.dll");
    if (!p->compiler_module) {
        MP_ERR(f, "NVOFA could not load d3dcompiler_47.dll error=%lu\n",
               GetLastError());
        goto fail;
    }
    p->compile = (d3d_compile_fn)GetProcAddress(p->compiler_module,
                                               "D3DCompile");
    if (!p->compile) {
        MP_ERR(f, "NVOFA D3DCompile entry point is unavailable\n");
        goto fail;
    }

    p->nvofa_module = LoadLibraryW(L"nvofapi64.dll");
    if (!p->nvofa_module) {
        MP_ERR(f, "NVOFA driver library is unavailable error=%lu\n",
               GetLastError());
        goto fail;
    }
    get_max_version_fn get_max_version = (get_max_version_fn)GetProcAddress(
        p->nvofa_module, "NvOFGetMaxSupportedApiVersion");
    create_instance_d3d11_fn create_instance =
        (create_instance_d3d11_fn)GetProcAddress(
            p->nvofa_module, "NvOFAPICreateInstanceD3D11");
    if (!get_max_version || !create_instance ||
        !check_of(f, "NvOFGetMaxSupportedApiVersion",
                  get_max_version(&p->nvofa_api_version)) ||
        p->nvofa_api_version < NV_OF_API_VERSION ||
        !check_of(f, "NvOFAPICreateInstanceD3D11",
                  create_instance(NV_OF_API_VERSION, &p->nvofa))) {
        MP_ERR(f, "NVOFA API 5.0 is unavailable in the installed driver\n");
        goto fail;
    }
    return f;
fail:
    talloc_free(f);
    return NULL;
}

#define OPT_BASE_STRUCT struct opts
static const m_option_t option_fields[] = {
    {"target-fps", OPT_DOUBLE(target_fps), M_RANGE(50.0, 61.0)},
    {"scene-threshold", OPT_DOUBLE(scene_threshold), M_RANGE(0.05, 1.0)},
    {0}
};

const struct mp_user_filter_entry vf_nvofa = {
    .desc = {
        .description = "NVIDIA Optical Flow D3D11 frame interpolation",
        .name = "nvofa",
        .priv_size = sizeof(OPT_BASE_STRUCT),
        .priv_defaults = &(const OPT_BASE_STRUCT) {
            .target_fps = 60.0,
            .scene_threshold = 0.35,
        },
        .options = option_fields,
    },
    .create = create,
};
