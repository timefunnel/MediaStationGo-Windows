#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <d3d11.h>
#include <d3d11_1.h>
#include <d3dcompiler.h>
#include <dxgi1_2.h>

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

#include "nvOpticalFlowD3D11.h"

template <typename T>
class ComPtr {
public:
    ComPtr() = default;
    ~ComPtr() { reset(); }
    ComPtr(const ComPtr &) = delete;
    ComPtr &operator=(const ComPtr &) = delete;

    T *get() const { return value_; }
    T **put() {
        reset();
        return &value_;
    }
    T *operator->() const { return value_; }
    explicit operator bool() const { return value_ != nullptr; }
    void reset(T *value = nullptr) {
        if (value_)
            value_->Release();
        value_ = value;
    }

private:
    T *value_ = nullptr;
};

struct OfResource {
    ID3D11Texture2D *texture = nullptr;
    NvOFGPUBufferHandle handle = nullptr;
};

struct OfSession {
    HMODULE module = nullptr;
    NV_OF_D3D11_API_FUNCTION_LIST api{};
    NvOFHandle handle = nullptr;
    std::vector<OfResource *> resources;

    ~OfSession() {
        for (auto *resource : resources) {
            if (resource->handle)
                api.nvOFUnregisterResourceD3D11(resource->handle);
            if (resource->texture)
                resource->texture->Release();
        }
        if (handle)
            api.nvOFDestroy(handle);
        if (module)
            FreeLibrary(module);
    }
};

using GetMaxVersionFn = NV_OF_STATUS(NVOFAPI *)(uint32_t *);
using CreateInstanceD3D11Fn = NV_OF_STATUS(NVOFAPI *)(
    uint32_t, NV_OF_D3D11_API_FUNCTION_LIST *);

static const char *status_name(NV_OF_STATUS status) {
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

static bool check_status(const char *operation, NV_OF_STATUS status) {
    if (status == NV_OF_SUCCESS)
        return true;
    std::fprintf(stderr, "NVOFA_POC_ERROR operation=%s status=%u name=%s\n",
                 operation, static_cast<unsigned>(status), status_name(status));
    return false;
}

static bool check_hr(const char *operation, HRESULT result) {
    if (SUCCEEDED(result))
        return true;
    std::fprintf(stderr, "NVOFA_POC_ERROR operation=%s hresult=0x%08lx\n",
                 operation, static_cast<unsigned long>(result));
    return false;
}

static bool create_nvidia_device(ComPtr<IDXGIAdapter1> &selected_adapter,
                                 ComPtr<ID3D11Device> &device,
                                 ComPtr<ID3D11DeviceContext> &context,
                                 std::string &adapter_name) {
    ComPtr<IDXGIFactory1> factory;
    if (!check_hr("CreateDXGIFactory1",
                  CreateDXGIFactory1(__uuidof(IDXGIFactory1),
                                     reinterpret_cast<void **>(factory.put()))))
        return false;

    for (UINT index = 0;; ++index) {
        ComPtr<IDXGIAdapter1> adapter;
        HRESULT result = factory->EnumAdapters1(index, adapter.put());
        if (result == DXGI_ERROR_NOT_FOUND)
            break;
        if (!check_hr("EnumAdapters1", result))
            return false;

        DXGI_ADAPTER_DESC1 description{};
        if (!check_hr("GetDesc1", adapter->GetDesc1(&description)))
            return false;
        if (description.VendorId != 0x10de ||
            (description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE))
            continue;

        char utf8_name[256]{};
        WideCharToMultiByte(CP_UTF8, 0, description.Description, -1,
                            utf8_name, sizeof(utf8_name), nullptr, nullptr);
        adapter_name = utf8_name;

        const D3D_FEATURE_LEVEL requested[] = {
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
        };
        D3D_FEATURE_LEVEL actual{};
        result = D3D11CreateDevice(
            adapter.get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT |
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            requested,
            static_cast<UINT>(std::size(requested)), D3D11_SDK_VERSION,
            device.put(), &actual, context.put());
        if (result == E_INVALIDARG) {
            result = D3D11CreateDevice(
                adapter.get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT |
                    D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                requested + 1, 1,
                D3D11_SDK_VERSION, device.put(), &actual, context.put());
        }
        if (!check_hr("D3D11CreateDevice", result))
            return false;

        adapter->AddRef();
        selected_adapter.reset(adapter.get());
        return true;
    }

    std::fprintf(stderr, "NVOFA_POC_ERROR operation=select_adapter detail=no_nvidia_adapter\n");
    return false;
}

static bool query_cap(OfSession &session, NV_OF_CAPS capability,
                      std::vector<uint32_t> &values) {
    uint32_t count = 0;
    if (!check_status("nvOFGetCaps(count)",
                      session.api.nvOFGetCaps(session.handle, capability,
                                              nullptr, &count)))
        return false;
    values.resize(count);
    if (count == 0)
        return true;
    return check_status("nvOFGetCaps(values)",
                        session.api.nvOFGetCaps(session.handle, capability,
                                                values.data(), &count));
}

static bool query_formats(OfSession &session, NV_OF_BUFFER_USAGE usage,
                          std::vector<DXGI_FORMAT> &formats) {
    uint32_t count = 0;
    if (!check_status("nvOFGetSurfaceFormatCountD3D11",
                      session.api.nvOFGetSurfaceFormatCountD3D11(
                          session.handle, usage, NV_OF_MODE_OPTICALFLOW,
                          &count)))
        return false;
    formats.resize(count);
    if (count == 0)
        return true;
    return check_status("nvOFGetSurfaceFormatD3D11",
                        session.api.nvOFGetSurfaceFormatD3D11(
                            session.handle, usage, NV_OF_MODE_OPTICALFLOW,
                            formats.data()));
}

static bool has_format(const std::vector<DXGI_FORMAT> &formats,
                       DXGI_FORMAT expected) {
    return std::find(formats.begin(), formats.end(), expected) != formats.end();
}

static bool create_texture(ID3D11Device *device, UINT width, UINT height,
                           DXGI_FORMAT format, UINT bind_flags,
                           D3D11_USAGE usage, UINT cpu_flags,
                           ID3D11Texture2D **texture) {
    D3D11_TEXTURE2D_DESC description{};
    description.Width = width;
    description.Height = height;
    description.MipLevels = 1;
    description.ArraySize = 1;
    description.Format = format;
    description.SampleDesc.Count = 1;
    description.Usage = usage;
    description.BindFlags = bind_flags;
    description.CPUAccessFlags = cpu_flags;
    return check_hr("CreateTexture2D",
                    device->CreateTexture2D(&description, nullptr, texture));
}

static bool register_resource(OfSession &session, OfResource &resource) {
    if (!check_status("nvOFRegisterResourceD3D11",
                      session.api.nvOFRegisterResourceD3D11(
                          session.handle, resource.texture, &resource.handle)))
        return false;
    resource.texture->AddRef();
    session.resources.push_back(&resource);
    return true;
}

static uint8_t pattern_value(int x, int y) {
    uint32_t value = static_cast<uint32_t>(x) * 0x9e3779b1u;
    value ^= static_cast<uint32_t>(y) * 0x85ebca6bu;
    value ^= value >> 16;
    value *= 0x7feb352du;
    value ^= value >> 15;
    return static_cast<uint8_t>(value >> 24);
}

static bool wait_for_gpu(ID3D11DeviceContext *context, ID3D11Query *query) {
    context->End(query);
    for (;;) {
        HRESULT result = context->GetData(query, nullptr, 0, 0);
        if (result == S_OK)
            return true;
        if (result != S_FALSE)
            return check_hr("GetData", result);
        SwitchToThread();
    }
}

struct FlowStats {
    uint64_t samples = 0;
    uint64_t nonzero = 0;
    int maximum = 0;
    double median_x = 0.0;
};

static bool inspect_flow(ID3D11Device *device, ID3D11DeviceContext *context,
                         ID3D11Texture2D *flow, FlowStats &stats) {
    D3D11_TEXTURE2D_DESC description{};
    flow->GetDesc(&description);
    ComPtr<ID3D11Texture2D> staging;
    if (!create_texture(device, description.Width, description.Height,
                        description.Format, 0, D3D11_USAGE_STAGING,
                        D3D11_CPU_ACCESS_READ, staging.put()))
        return false;
    context->CopyResource(staging.get(), flow);

    D3D11_MAPPED_SUBRESOURCE mapped{};
    if (!check_hr("Map(flow)", context->Map(staging.get(), 0,
                                             D3D11_MAP_READ, 0, &mapped)))
        return false;
    std::vector<int> interior_x;
    const UINT margin_x = description.Width / 10;
    const UINT margin_y = description.Height / 10;
    for (UINT y = 0; y < description.Height; ++y) {
        auto *row = reinterpret_cast<const int16_t *>(
            static_cast<const uint8_t *>(mapped.pData) +
            static_cast<size_t>(mapped.RowPitch) * y);
        for (UINT x = 0; x < description.Width; ++x) {
            int flow_x = row[x * 2];
            int flow_y = row[x * 2 + 1];
            ++stats.samples;
            if (flow_x != 0 || flow_y != 0)
                ++stats.nonzero;
            stats.maximum = std::max(stats.maximum,
                                     std::max(std::abs(flow_x), std::abs(flow_y)));
            if (x >= margin_x && x + margin_x < description.Width &&
                y >= margin_y && y + margin_y < description.Height)
                interior_x.push_back(flow_x);
        }
    }
    context->Unmap(staging.get(), 0);
    if (!interior_x.empty()) {
        auto middle = interior_x.begin() + interior_x.size() / 2;
        std::nth_element(interior_x.begin(), middle, interior_x.end());
        stats.median_x = *middle / 32.0;
    }
    return true;
}

static constexpr char interpolation_shader[] = R"hlsl(
cbuffer InterpolationParams : register(b0)
{
    float alpha;
    float flow_grid;
    float frame_width;
    float frame_height;
};

Texture2D<float4> source_a : register(t0);
Texture2D<float4> source_b : register(t1);
Texture2D<int2> flow_forward : register(t2);
Texture2D<int2> flow_backward : register(t3);
SamplerState source_sampler : register(s0);

struct VertexOutput
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

VertexOutput vertex_main(uint vertex_id : SV_VertexID)
{
    VertexOutput output;
    float2 position = float2((vertex_id << 1) & 2, vertex_id & 2);
    output.uv = position;
    output.position = float4(position * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
    return output;
}

float2 load_flow(Texture2D<int2> texture_value, float2 pixel)
{
    uint flow_width;
    uint flow_height;
    texture_value.GetDimensions(flow_width, flow_height);
    float2 coordinate = pixel / flow_grid - 0.5;
    float2 base_value = floor(coordinate);
    float2 fraction = coordinate - base_value;
    int2 maximum = int2(flow_width - 1, flow_height - 1);
    int2 p00 = clamp(int2(base_value), int2(0, 0), maximum);
    int2 p10 = clamp(p00 + int2(1, 0), int2(0, 0), maximum);
    int2 p01 = clamp(p00 + int2(0, 1), int2(0, 0), maximum);
    int2 p11 = clamp(p00 + int2(1, 1), int2(0, 0), maximum);
    float2 f00 = float2(texture_value.Load(int3(p00, 0))) / 32.0;
    float2 f10 = float2(texture_value.Load(int3(p10, 0))) / 32.0;
    float2 f01 = float2(texture_value.Load(int3(p01, 0))) / 32.0;
    float2 f11 = float2(texture_value.Load(int3(p11, 0))) / 32.0;
    return lerp(lerp(f00, f10, fraction.x), lerp(f01, f11, fraction.x), fraction.y);
}

float4 pixel_main(VertexOutput input) : SV_Target
{
    float2 dimensions = float2(frame_width, frame_height);
    float2 pixel = input.uv * dimensions;
    float2 forward = load_flow(flow_forward, pixel);
    float2 backward = load_flow(flow_backward, pixel);
    float2 uv_a = clamp((pixel - alpha * forward) / dimensions, 0.0, 1.0);
    float2 uv_b = clamp((pixel - (1.0 - alpha) * backward) / dimensions, 0.0, 1.0);
    float4 value_a = source_a.SampleLevel(source_sampler, uv_a, 0.0);
    float4 value_b = source_b.SampleLevel(source_sampler, uv_b, 0.0);

    float2 backward_at_forward = load_flow(flow_backward, pixel + forward);
    float consistency = length(forward + backward_at_forward);
    float difference = max(max(abs(value_a.r - value_b.r), abs(value_a.g - value_b.g)),
                           abs(value_a.b - value_b.b));
    if (consistency > 6.0 || difference > 0.45)
        return alpha < 0.5 ? value_a : value_b;
    return lerp(value_a, value_b, alpha);
}

float pixel_luma(VertexOutput input) : SV_Target
{
    float3 value = source_a.SampleLevel(source_sampler, input.uv, 0.0).rgb;
    return dot(value, float3(0.2627, 0.6780, 0.0593));
}
)hlsl";

struct InterpolationPipeline {
    ComPtr<ID3D11VertexShader> vertex_shader;
    ComPtr<ID3D11PixelShader> pixel_shader;
    ComPtr<ID3D11PixelShader> luma_shader;
    ComPtr<ID3D11SamplerState> sampler;
    ComPtr<ID3D11Buffer> constants;
    ComPtr<ID3D11Texture2D> source_a;
    ComPtr<ID3D11Texture2D> source_b;
    ComPtr<ID3D11Texture2D> output;
    ComPtr<ID3D11ShaderResourceView> source_a_view;
    ComPtr<ID3D11ShaderResourceView> source_b_view;
    ComPtr<ID3D11ShaderResourceView> forward_view;
    ComPtr<ID3D11ShaderResourceView> backward_view;
    ComPtr<ID3D11RenderTargetView> output_view;
    ComPtr<ID3D11RenderTargetView> luma_a_view;
    ComPtr<ID3D11RenderTargetView> luma_b_view;
    UINT width = 0;
    UINT height = 0;
    float grid = 0.0f;
};

static bool compile_shader(const char *entry_point, const char *target,
                           ComPtr<ID3DBlob> &bytecode) {
    ComPtr<ID3DBlob> errors;
    HRESULT result = D3DCompile(
        interpolation_shader, sizeof(interpolation_shader) - 1,
        "nvofa_interpolation.hlsl", nullptr, nullptr, entry_point, target,
        D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3, 0,
        bytecode.put(), errors.put());
    if (FAILED(result)) {
        if (errors)
            std::fprintf(stderr, "NVOFA_POC_SHADER_ERROR %s\n",
                         static_cast<const char *>(errors->GetBufferPointer()));
        return check_hr("D3DCompile", result);
    }
    return true;
}

static bool create_interpolation_pipeline(
    ID3D11Device *device,
    ID3D11Texture2D *forward, ID3D11Texture2D *backward,
    ID3D11Texture2D *luma_a, ID3D11Texture2D *luma_b,
    UINT width, UINT height, UINT grid, InterpolationPipeline &pipeline) {
    pipeline.width = width;
    pipeline.height = height;
    pipeline.grid = static_cast<float>(grid);

    ComPtr<ID3DBlob> vertex_bytecode;
    ComPtr<ID3DBlob> pixel_bytecode;
    ComPtr<ID3DBlob> luma_bytecode;
    if (!compile_shader("vertex_main", "vs_5_0", vertex_bytecode) ||
        !compile_shader("pixel_main", "ps_5_0", pixel_bytecode) ||
        !compile_shader("pixel_luma", "ps_5_0", luma_bytecode))
        return false;
    if (!check_hr("CreateVertexShader",
                  device->CreateVertexShader(vertex_bytecode->GetBufferPointer(),
                                             vertex_bytecode->GetBufferSize(),
                                             nullptr, pipeline.vertex_shader.put())) ||
        !check_hr("CreatePixelShader",
                  device->CreatePixelShader(pixel_bytecode->GetBufferPointer(),
                                            pixel_bytecode->GetBufferSize(),
                                            nullptr, pipeline.pixel_shader.put())) ||
        !check_hr("CreatePixelShader(luma)",
                  device->CreatePixelShader(luma_bytecode->GetBufferPointer(),
                                            luma_bytecode->GetBufferSize(),
                                            nullptr, pipeline.luma_shader.put())))
        return false;

    D3D11_SAMPLER_DESC sampler_description{};
    sampler_description.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
    sampler_description.AddressU = D3D11_TEXTURE_ADDRESS_CLAMP;
    sampler_description.AddressV = D3D11_TEXTURE_ADDRESS_CLAMP;
    sampler_description.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
    sampler_description.MaxLOD = D3D11_FLOAT32_MAX;
    if (!check_hr("CreateSamplerState",
                  device->CreateSamplerState(&sampler_description,
                                             pipeline.sampler.put())))
        return false;

    D3D11_BUFFER_DESC buffer_description{};
    buffer_description.ByteWidth = 16;
    buffer_description.Usage = D3D11_USAGE_DEFAULT;
    buffer_description.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
    if (!check_hr("CreateBuffer",
                  device->CreateBuffer(&buffer_description, nullptr,
                                       pipeline.constants.put())))
        return false;

    constexpr UINT source_bind =
        D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
    if (!create_texture(device, width, height, DXGI_FORMAT_R10G10B10A2_UNORM,
                        source_bind, D3D11_USAGE_DEFAULT, 0,
                        pipeline.source_a.put()) ||
        !create_texture(device, width, height, DXGI_FORMAT_R10G10B10A2_UNORM,
                        source_bind, D3D11_USAGE_DEFAULT, 0,
                        pipeline.source_b.put()) ||
        !create_texture(device, width, height, DXGI_FORMAT_R10G10B10A2_UNORM,
                        source_bind, D3D11_USAGE_DEFAULT, 0,
                        pipeline.output.put()))
        return false;
    if (!check_hr("CreateShaderResourceView(source_a)",
                  device->CreateShaderResourceView(pipeline.source_a.get(), nullptr,
                                                   pipeline.source_a_view.put())) ||
        !check_hr("CreateShaderResourceView(source_b)",
                  device->CreateShaderResourceView(pipeline.source_b.get(), nullptr,
                                                   pipeline.source_b_view.put())) ||
        !check_hr("CreateShaderResourceView(forward)",
                  device->CreateShaderResourceView(forward, nullptr,
                                                   pipeline.forward_view.put())) ||
        !check_hr("CreateShaderResourceView(backward)",
                  device->CreateShaderResourceView(backward, nullptr,
                                                   pipeline.backward_view.put())) ||
        !check_hr("CreateRenderTargetView",
                  device->CreateRenderTargetView(pipeline.output.get(), nullptr,
                                                 pipeline.output_view.put())) ||
        !check_hr("CreateRenderTargetView(luma_a)",
                  device->CreateRenderTargetView(luma_a, nullptr,
                                                 pipeline.luma_a_view.put())) ||
        !check_hr("CreateRenderTargetView(luma_b)",
                  device->CreateRenderTargetView(luma_b, nullptr,
                                                 pipeline.luma_b_view.put())))
        return false;
    return true;
}

struct VideoConversionPipeline {
    ComPtr<ID3D11VideoDevice> video_device;
    ComPtr<ID3D11VideoContext> video_context;
    ComPtr<ID3D11VideoContext1> video_context1;
    ComPtr<ID3D11VideoProcessorEnumerator> enumerator;
    ComPtr<ID3D11VideoProcessor> processor;
    ComPtr<ID3D11Texture2D> input_a;
    ComPtr<ID3D11Texture2D> input_b;
    ComPtr<ID3D11VideoProcessorInputView> input_a_view;
    ComPtr<ID3D11VideoProcessorInputView> input_b_view;
    ComPtr<ID3D11VideoProcessorOutputView> output_a_view;
    ComPtr<ID3D11VideoProcessorOutputView> output_b_view;
};

static void upload_p010_patterns(ID3D11DeviceContext *context,
                                 ID3D11Texture2D *input_a,
                                 ID3D11Texture2D *input_b,
                                 UINT width, UINT height) {
    constexpr int shift = 8;
    const size_t luma_samples = static_cast<size_t>(width) * height;
    const size_t chroma_samples = luma_samples / 2;
    std::vector<uint16_t> first(luma_samples + chroma_samples);
    std::vector<uint16_t> second(first.size());
    for (UINT y = 0; y < height; ++y) {
        for (UINT x = 0; x < width; ++x) {
            auto limited_p010 = [](int px, int py) {
                const uint32_t value = pattern_value(px, py);
                const uint32_t y10 = 64u + (value * 876u + 127u) / 255u;
                return static_cast<uint16_t>(y10 << 6);
            };
            first[static_cast<size_t>(y) * width + x] =
                limited_p010(static_cast<int>(x), static_cast<int>(y));
            second[static_cast<size_t>(y) * width + x] =
                limited_p010(std::max(0, static_cast<int>(x) - shift),
                             static_cast<int>(y));
        }
    }
    const uint16_t neutral_chroma = static_cast<uint16_t>(512u << 6);
    std::fill(first.begin() + luma_samples, first.end(), neutral_chroma);
    std::fill(second.begin() + luma_samples, second.end(), neutral_chroma);
    context->UpdateSubresource(input_a, 0, nullptr, first.data(), width * 2, 0);
    context->UpdateSubresource(input_b, 0, nullptr, second.data(), width * 2, 0);
}

static bool create_video_conversion_pipeline(
    ID3D11Device *device, ID3D11DeviceContext *context,
    ID3D11Texture2D *output_a, ID3D11Texture2D *output_b,
    UINT width, UINT height, VideoConversionPipeline &pipeline) {
    if (!check_hr("QueryInterface(ID3D11VideoDevice)",
                  device->QueryInterface(__uuidof(ID3D11VideoDevice),
                                         reinterpret_cast<void **>(
                                             pipeline.video_device.put()))) ||
        !check_hr("QueryInterface(ID3D11VideoContext)",
                  context->QueryInterface(__uuidof(ID3D11VideoContext),
                                          reinterpret_cast<void **>(
                                              pipeline.video_context.put()))))
        return false;
    if (!check_hr("QueryInterface(ID3D11VideoContext1)",
                  pipeline.video_context->QueryInterface(
                      __uuidof(ID3D11VideoContext1),
                      reinterpret_cast<void **>(pipeline.video_context1.put()))))
        return false;

    D3D11_VIDEO_PROCESSOR_CONTENT_DESC content{};
    content.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
    content.InputFrameRate = {24, 1};
    content.InputWidth = width;
    content.InputHeight = height;
    content.OutputFrameRate = {60, 1};
    content.OutputWidth = width;
    content.OutputHeight = height;
    content.Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;
    if (!check_hr("CreateVideoProcessorEnumerator",
                  pipeline.video_device->CreateVideoProcessorEnumerator(
                      &content, pipeline.enumerator.put())))
        return false;

    UINT input_support = 0;
    UINT output_support = 0;
    if (!check_hr("CheckVideoProcessorFormat(P010)",
                  pipeline.enumerator->CheckVideoProcessorFormat(
                      DXGI_FORMAT_P010, &input_support)) ||
        !check_hr("CheckVideoProcessorFormat(RGB10)",
                  pipeline.enumerator->CheckVideoProcessorFormat(
                      DXGI_FORMAT_R10G10B10A2_UNORM, &output_support)))
        return false;
    if (!(input_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT) ||
        !(output_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT)) {
        std::fprintf(stderr,
                     "NVOFA_POC_ERROR operation=video_processor_formats p010=0x%x rgb10=0x%x\n",
                     input_support, output_support);
        return false;
    }
    if (!check_hr("CreateVideoProcessor",
                  pipeline.video_device->CreateVideoProcessor(
                      pipeline.enumerator.get(), 0, pipeline.processor.put())))
        return false;

    if (!create_texture(device, width, height, DXGI_FORMAT_P010,
                        D3D11_BIND_DECODER, D3D11_USAGE_DEFAULT, 0,
                        pipeline.input_a.put()) ||
        !create_texture(device, width, height, DXGI_FORMAT_P010,
                        D3D11_BIND_DECODER, D3D11_USAGE_DEFAULT, 0,
                        pipeline.input_b.put()))
        return false;
    upload_p010_patterns(context, pipeline.input_a.get(), pipeline.input_b.get(),
                         width, height);

    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC input_view{};
    input_view.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
    input_view.Texture2D.ArraySlice = 0;
    if (!check_hr("CreateVideoProcessorInputView(a)",
                  pipeline.video_device->CreateVideoProcessorInputView(
                      pipeline.input_a.get(), pipeline.enumerator.get(),
                      &input_view, pipeline.input_a_view.put())) ||
        !check_hr("CreateVideoProcessorInputView(b)",
                  pipeline.video_device->CreateVideoProcessorInputView(
                      pipeline.input_b.get(), pipeline.enumerator.get(),
                      &input_view, pipeline.input_b_view.put())))
        return false;

    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC output_view{};
    output_view.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
    output_view.Texture2D.MipSlice = 0;
    if (!check_hr("CreateVideoProcessorOutputView(a)",
                  pipeline.video_device->CreateVideoProcessorOutputView(
                      output_a, pipeline.enumerator.get(), &output_view,
                      pipeline.output_a_view.put())) ||
        !check_hr("CreateVideoProcessorOutputView(b)",
                  pipeline.video_device->CreateVideoProcessorOutputView(
                      output_b, pipeline.enumerator.get(), &output_view,
                      pipeline.output_b_view.put())))
        return false;

    RECT frame_rect{0, 0, static_cast<LONG>(width), static_cast<LONG>(height)};
    pipeline.video_context->VideoProcessorSetStreamSourceRect(
        pipeline.processor.get(), 0, TRUE, &frame_rect);
    pipeline.video_context->VideoProcessorSetStreamDestRect(
        pipeline.processor.get(), 0, TRUE, &frame_rect);
    pipeline.video_context->VideoProcessorSetOutputTargetRect(
        pipeline.processor.get(), TRUE, &frame_rect);
    pipeline.video_context->VideoProcessorSetStreamAutoProcessingMode(
        pipeline.processor.get(), 0, FALSE);
    pipeline.video_context1->VideoProcessorSetStreamColorSpace1(
        pipeline.processor.get(), 0,
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020);
    pipeline.video_context1->VideoProcessorSetOutputColorSpace1(
        pipeline.processor.get(), DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
    return true;
}

static bool convert_p010_pair(ID3D11DeviceContext *context,
                              VideoConversionPipeline &pipeline) {
    D3D11_VIDEO_PROCESSOR_STREAM stream{};
    stream.Enable = TRUE;
    stream.pInputSurface = pipeline.input_a_view.get();
    if (!check_hr("VideoProcessorBlt(a)",
                  pipeline.video_context->VideoProcessorBlt(
                      pipeline.processor.get(), pipeline.output_a_view.get(),
                      0, 1, &stream)))
        return false;
    stream.pInputSurface = pipeline.input_b_view.get();
    if (!check_hr("VideoProcessorBlt(b)",
                  pipeline.video_context->VideoProcessorBlt(
                      pipeline.processor.get(), pipeline.output_b_view.get(),
                      1, 1, &stream)))
        return false;
    context->Flush();
    return true;
}

struct ShaderParameters {
    float alpha;
    float flow_grid;
    float frame_width;
    float frame_height;
};

static void render_interpolated(ID3D11DeviceContext *context,
                                InterpolationPipeline &pipeline,
                                float alpha) {
    ShaderParameters parameters{
        alpha,
        pipeline.grid,
        static_cast<float>(pipeline.width),
        static_cast<float>(pipeline.height),
    };
    context->UpdateSubresource(pipeline.constants.get(), 0, nullptr,
                               &parameters, 0, 0);
    ID3D11Buffer *constant_buffers[] = {pipeline.constants.get()};
    ID3D11SamplerState *samplers[] = {pipeline.sampler.get()};
    ID3D11ShaderResourceView *resources[] = {
        pipeline.source_a_view.get(), pipeline.source_b_view.get(),
        pipeline.forward_view.get(), pipeline.backward_view.get(),
    };
    ID3D11RenderTargetView *targets[] = {pipeline.output_view.get()};
    D3D11_VIEWPORT viewport{};
    viewport.Width = static_cast<float>(pipeline.width);
    viewport.Height = static_cast<float>(pipeline.height);
    viewport.MaxDepth = 1.0f;

    context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    context->VSSetShader(pipeline.vertex_shader.get(), nullptr, 0);
    context->PSSetShader(pipeline.pixel_shader.get(), nullptr, 0);
    context->PSSetConstantBuffers(0, 1, constant_buffers);
    context->PSSetSamplers(0, 1, samplers);
    context->PSSetShaderResources(0, 4, resources);
    context->OMSetRenderTargets(1, targets, nullptr);
    context->RSSetViewports(1, &viewport);
    context->Draw(3, 0);
}

static void render_luma_inputs(ID3D11DeviceContext *context,
                               InterpolationPipeline &pipeline) {
    ID3D11SamplerState *samplers[] = {pipeline.sampler.get()};
    D3D11_VIEWPORT viewport{};
    viewport.Width = static_cast<float>(pipeline.width);
    viewport.Height = static_cast<float>(pipeline.height);
    viewport.MaxDepth = 1.0f;
    context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    context->VSSetShader(pipeline.vertex_shader.get(), nullptr, 0);
    context->PSSetShader(pipeline.luma_shader.get(), nullptr, 0);
    context->PSSetSamplers(0, 1, samplers);
    context->RSSetViewports(1, &viewport);

    ID3D11ShaderResourceView *source[] = {pipeline.source_a_view.get()};
    ID3D11RenderTargetView *target[] = {pipeline.luma_a_view.get()};
    context->PSSetShaderResources(0, 1, source);
    context->OMSetRenderTargets(1, target, nullptr);
    context->Draw(3, 0);
    ID3D11ShaderResourceView *empty_source[] = {nullptr};
    context->PSSetShaderResources(0, 1, empty_source);

    source[0] = pipeline.source_b_view.get();
    target[0] = pipeline.luma_b_view.get();
    context->PSSetShaderResources(0, 1, source);
    context->OMSetRenderTargets(1, target, nullptr);
    context->Draw(3, 0);
    context->PSSetShaderResources(0, 1, empty_source);
    ID3D11RenderTargetView *empty_target[] = {nullptr};
    context->OMSetRenderTargets(1, empty_target, nullptr);
}

static void unbind_interpolation_resources(ID3D11DeviceContext *context) {
    ID3D11ShaderResourceView *empty_resources[4]{};
    ID3D11RenderTargetView *empty_targets[1]{};
    context->PSSetShaderResources(0, 4, empty_resources);
    context->OMSetRenderTargets(1, empty_targets, nullptr);
}

static int run(UINT width, UINT height, int iterations, double minimum_fps) {
    ComPtr<IDXGIAdapter1> adapter;
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    std::string adapter_name;
    if (!create_nvidia_device(adapter, device, context, adapter_name))
        return 2;

    OfSession session;
    session.module = LoadLibraryW(L"nvofapi64.dll");
    if (!session.module) {
        std::fprintf(stderr, "NVOFA_POC_ERROR operation=LoadLibrary detail=win32_%lu\n",
                     static_cast<unsigned long>(GetLastError()));
        return 2;
    }
    auto get_max_version = reinterpret_cast<GetMaxVersionFn>(
        GetProcAddress(session.module, "NvOFGetMaxSupportedApiVersion"));
    auto create_instance = reinterpret_cast<CreateInstanceD3D11Fn>(
        GetProcAddress(session.module, "NvOFAPICreateInstanceD3D11"));
    if (!get_max_version || !create_instance) {
        std::fprintf(stderr, "NVOFA_POC_ERROR operation=GetProcAddress detail=entrypoint_missing\n");
        return 2;
    }

    uint32_t driver_api_version = 0;
    if (!check_status("NvOFGetMaxSupportedApiVersion",
                      get_max_version(&driver_api_version)))
        return 2;
    if (driver_api_version < NV_OF_API_VERSION) {
        std::fprintf(stderr,
                     "NVOFA_POC_ERROR operation=api_version driver=0x%x required=0x%x\n",
                     driver_api_version, NV_OF_API_VERSION);
        return 2;
    }
    if (!check_status("NvOFAPICreateInstanceD3D11",
                      create_instance(NV_OF_API_VERSION, &session.api)))
        return 2;
    if (!check_status("nvCreateOpticalFlowD3D11",
                      session.api.nvCreateOpticalFlowD3D11(
                          device.get(), context.get(), &session.handle)))
        return 2;

    std::vector<uint32_t> grids;
    std::vector<uint32_t> width_max;
    std::vector<uint32_t> height_max;
    if (!query_cap(session, NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES, grids) ||
        !query_cap(session, NV_OF_CAPS_WIDTH_MAX, width_max) ||
        !query_cap(session, NV_OF_CAPS_HEIGHT_MAX, height_max))
        return 2;
    if (width_max.empty() || height_max.empty() || width > width_max[0] ||
        height > height_max[0]) {
        std::fprintf(stderr,
                     "NVOFA_POC_ERROR operation=dimensions requested=%ux%u maximum=%ux%u\n",
                     width, height, width_max.empty() ? 0 : width_max[0],
                     height_max.empty() ? 0 : height_max[0]);
        return 2;
    }
    const uint32_t grid = std::find(grids.begin(), grids.end(), 4) != grids.end()
        ? 4
        : (grids.empty() ? 0 : grids.back());
    if (grid == 0) {
        std::fprintf(stderr, "NVOFA_POC_ERROR operation=grid detail=no_supported_grid\n");
        return 2;
    }

    std::vector<DXGI_FORMAT> input_formats;
    std::vector<DXGI_FORMAT> output_formats;
    if (!query_formats(session, NV_OF_BUFFER_USAGE_INPUT, input_formats) ||
        !query_formats(session, NV_OF_BUFFER_USAGE_OUTPUT, output_formats))
        return 2;
    if (!has_format(input_formats, DXGI_FORMAT_R8_UNORM) ||
        !has_format(output_formats, DXGI_FORMAT_R16G16_SINT)) {
        std::fprintf(stderr,
                     "NVOFA_POC_ERROR operation=formats r8=%d r16g16_sint=%d\n",
                     has_format(input_formats, DXGI_FORMAT_R8_UNORM),
                     has_format(output_formats, DXGI_FORMAT_R16G16_SINT));
        return 2;
    }

    NV_OF_INIT_PARAMS init{};
    init.width = width;
    init.height = height;
    init.outGridSize = static_cast<NV_OF_OUTPUT_VECTOR_GRID_SIZE>(grid);
    init.hintGridSize = NV_OF_HINT_VECTOR_GRID_SIZE_UNDEFINED;
    init.mode = NV_OF_MODE_OPTICALFLOW;
    init.perfLevel = NV_OF_PERF_LEVEL_FAST;
    init.enableExternalHints = NV_OF_FALSE;
    init.enableOutputCost = NV_OF_FALSE;
    init.disparityRange = NV_OF_STEREO_DISPARITY_RANGE_UNDEFINED;
    init.enableRoi = NV_OF_FALSE;
    init.predDirection = NV_OF_PRED_DIRECTION_BOTH;
    init.enableGlobalFlow = NV_OF_FALSE;
    init.inputBufferFormat = NV_OF_BUFFER_FORMAT_GRAYSCALE8;
    if (!check_status("nvOFInit", session.api.nvOFInit(session.handle, &init)))
        return 2;

    const UINT flow_width = (width + grid - 1) / grid;
    const UINT flow_height = (height + grid - 1) / grid;
    OfResource input;
    OfResource reference;
    OfResource forward;
    OfResource backward;
    constexpr UINT luma_bind =
        D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
    if (!create_texture(device.get(), width, height, DXGI_FORMAT_R8_UNORM,
                        luma_bind, D3D11_USAGE_DEFAULT, 0,
                        &input.texture) ||
        !create_texture(device.get(), width, height, DXGI_FORMAT_R8_UNORM,
                        luma_bind, D3D11_USAGE_DEFAULT, 0,
                        &reference.texture) ||
        !create_texture(device.get(), flow_width, flow_height,
                        DXGI_FORMAT_R16G16_SINT, D3D11_BIND_SHADER_RESOURCE,
                        D3D11_USAGE_DEFAULT, 0, &forward.texture) ||
        !create_texture(device.get(), flow_width, flow_height,
                        DXGI_FORMAT_R16G16_SINT, D3D11_BIND_SHADER_RESOURCE,
                        D3D11_USAGE_DEFAULT, 0, &backward.texture))
        return 2;
    std::unique_ptr<ID3D11Texture2D, void (*)(ID3D11Texture2D *)> input_guard(
        input.texture, [](ID3D11Texture2D *value) { if (value) value->Release(); });
    std::unique_ptr<ID3D11Texture2D, void (*)(ID3D11Texture2D *)> reference_guard(
        reference.texture, [](ID3D11Texture2D *value) { if (value) value->Release(); });
    std::unique_ptr<ID3D11Texture2D, void (*)(ID3D11Texture2D *)> forward_guard(
        forward.texture, [](ID3D11Texture2D *value) { if (value) value->Release(); });
    std::unique_ptr<ID3D11Texture2D, void (*)(ID3D11Texture2D *)> backward_guard(
        backward.texture, [](ID3D11Texture2D *value) { if (value) value->Release(); });
    if (!register_resource(session, input) ||
        !register_resource(session, reference) ||
        !register_resource(session, forward) ||
        !register_resource(session, backward))
        return 2;

    InterpolationPipeline pipeline;
    if (!create_interpolation_pipeline(device.get(),
                                       forward.texture, backward.texture,
                                       input.texture, reference.texture,
                                       width, height, grid, pipeline))
        return 2;
    VideoConversionPipeline conversion;
    if (!create_video_conversion_pipeline(
            device.get(), context.get(), pipeline.source_a.get(),
            pipeline.source_b.get(), width, height, conversion) ||
        !convert_p010_pair(context.get(), conversion))
        return 2;
    render_luma_inputs(context.get(), pipeline);

    NV_OF_EXECUTE_INPUT_PARAMS execute_input{};
    execute_input.inputFrame = input.handle;
    execute_input.referenceFrame = reference.handle;
    execute_input.disableTemporalHints = NV_OF_TRUE;
    NV_OF_EXECUTE_OUTPUT_PARAMS execute_output{};
    execute_output.outputBuffer = forward.handle;
    execute_output.bwdOutputBuffer = backward.handle;

    ComPtr<ID3D11Query> completion;
    D3D11_QUERY_DESC query_description{};
    query_description.Query = D3D11_QUERY_EVENT;
    if (!check_hr("CreateQuery",
                  device->CreateQuery(&query_description, completion.put())))
        return 2;

    constexpr int warmup_iterations = 20;
    for (int index = 0; index < warmup_iterations; ++index) {
        if (!check_status("nvOFExecute(warmup)",
                          session.api.nvOFExecute(session.handle, &execute_input,
                                                  &execute_output)) ||
            !wait_for_gpu(context.get(), completion.get()))
            return 2;
    }

    auto started = std::chrono::steady_clock::now();
    for (int index = 0; index < iterations; ++index) {
        if (!check_status("nvOFExecute",
                          session.api.nvOFExecute(session.handle, &execute_input,
                                                  &execute_output)) ||
            !wait_for_gpu(context.get(), completion.get()))
            return 2;
    }
    auto elapsed = std::chrono::duration<double>(
        std::chrono::steady_clock::now() - started).count();
    double flow_pairs_fps = iterations / elapsed;

    for (int index = 0; index < warmup_iterations; ++index) {
        if (!convert_p010_pair(context.get(), conversion))
            return 2;
        render_luma_inputs(context.get(), pipeline);
        if (!check_status("nvOFExecute(pipeline_warmup)",
                          session.api.nvOFExecute(session.handle, &execute_input,
                                                  &execute_output)))
            return 2;
        if ((index & 1) == 0) {
            render_interpolated(context.get(), pipeline, 0.0f);
            render_interpolated(context.get(), pipeline, 0.4f);
            render_interpolated(context.get(), pipeline, 0.8f);
        } else {
            render_interpolated(context.get(), pipeline, 0.2f);
            render_interpolated(context.get(), pipeline, 0.6f);
        }
        unbind_interpolation_resources(context.get());
        if (!wait_for_gpu(context.get(), completion.get()))
            return 2;
    }

    uint64_t output_frames = 0;
    started = std::chrono::steady_clock::now();
    for (int index = 0; index < iterations; ++index) {
        if (!convert_p010_pair(context.get(), conversion))
            return 2;
        render_luma_inputs(context.get(), pipeline);
        if (!check_status("nvOFExecute(pipeline)",
                          session.api.nvOFExecute(session.handle, &execute_input,
                                                  &execute_output)))
            return 2;
        if ((index & 1) == 0) {
            render_interpolated(context.get(), pipeline, 0.0f);
            render_interpolated(context.get(), pipeline, 0.4f);
            render_interpolated(context.get(), pipeline, 0.8f);
            output_frames += 3;
        } else {
            render_interpolated(context.get(), pipeline, 0.2f);
            render_interpolated(context.get(), pipeline, 0.6f);
            output_frames += 2;
        }
        unbind_interpolation_resources(context.get());
        if (!wait_for_gpu(context.get(), completion.get()))
            return 2;
    }
    double pipeline_elapsed = std::chrono::duration<double>(
        std::chrono::steady_clock::now() - started).count();
    double pipeline_output_fps = output_frames / pipeline_elapsed;

    FlowStats forward_stats;
    FlowStats backward_stats;
    if (!inspect_flow(device.get(), context.get(), forward.texture, forward_stats) ||
        !inspect_flow(device.get(), context.get(), backward.texture, backward_stats))
        return 2;
    if (forward_stats.nonzero == 0 || backward_stats.nonzero == 0) {
        std::fprintf(stderr,
                     "NVOFA_POC_ERROR operation=flow_validation forward_nonzero=%llu backward_nonzero=%llu\n",
                     static_cast<unsigned long long>(forward_stats.nonzero),
                     static_cast<unsigned long long>(backward_stats.nonzero));
        return 2;
    }

    std::printf(
        "NVOFA_POC_OK adapter=\"%s\" api=%u.%u resolution=%ux%u grid=%u "
        "source=P010 synthesis=RGB10 flow_input=R8 hdr_colorspace=PQ_BT2020 "
        "iterations=%d flow_seconds=%.6f flow_pairs_fps=%.2f "
        "pipeline_seconds=%.6f pipeline_output_frames=%llu pipeline_output_fps=%.2f "
        "realtime60=%.2fx forward_nonzero=%llu/%llu forward_median_x=%.2f "
        "backward_nonzero=%llu/%llu backward_median_x=%.2f max_vector_s10_5=%d\n",
        adapter_name.c_str(), driver_api_version >> 4,
        driver_api_version & 0xf, width, height, grid, iterations, elapsed,
        flow_pairs_fps, pipeline_elapsed,
        static_cast<unsigned long long>(output_frames), pipeline_output_fps,
        pipeline_output_fps / 60.0,
        static_cast<unsigned long long>(forward_stats.nonzero),
        static_cast<unsigned long long>(forward_stats.samples),
        forward_stats.median_x,
        static_cast<unsigned long long>(backward_stats.nonzero),
        static_cast<unsigned long long>(backward_stats.samples),
        backward_stats.median_x,
        std::max(forward_stats.maximum, backward_stats.maximum));
    if (pipeline_output_fps < minimum_fps) {
        std::fprintf(stderr,
                     "NVOFA_POC_ERROR operation=throughput measured=%.2f required=%.2f\n",
                     pipeline_output_fps, minimum_fps);
        return 3;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 5) {
        std::fprintf(stderr,
                     "usage: nvofa_poc.exe <width> <height> <iterations> <minimum-fps>\n");
        return 64;
    }
    unsigned long width = std::strtoul(argv[1], nullptr, 10);
    unsigned long height = std::strtoul(argv[2], nullptr, 10);
    long iterations = std::strtol(argv[3], nullptr, 10);
    double minimum_fps = std::strtod(argv[4], nullptr);
    if (width < 320 || width > 7680 || height < 240 || height > 4320 ||
        iterations < 1 || iterations > 10000 || !std::isfinite(minimum_fps) ||
        minimum_fps <= 0.0) {
        std::fprintf(stderr, "NVOFA_POC_ERROR operation=arguments detail=invalid_range\n");
        return 64;
    }
    return run(static_cast<UINT>(width), static_cast<UINT>(height),
               static_cast<int>(iterations), minimum_fps);
}
