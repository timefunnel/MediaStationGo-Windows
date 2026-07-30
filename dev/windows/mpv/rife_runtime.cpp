#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>

#include "rife_runtime.h"

#include <d3d11_3.h>
#include <d3dcompiler.h>
#include <wrl/client.h>

#include <NvInferRuntime.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <cstdarg>
#include <cstdint>
#include <cstdio>
#include <fstream>
#include <memory>
#include <string>
#include <vector>

using Microsoft::WRL::ComPtr;

namespace {

using CudaError = int;
using CudaStream = cudaStream_t;
using CudaGraphicsResource = void *;

constexpr CudaError kCudaSuccess = 0;
constexpr unsigned int kCudaStreamNonBlocking = 1;
constexpr size_t kInferenceHistogramBuckets = 4001;
constexpr double kInferenceHistogramBucketMs = 0.025;

const char kShaderSource[] = R"hlsl(
cbuffer RifeConstants : register(b0)
{
    uint source_width;
    uint source_height;
    uint padded_width;
    uint padded_height;
    uint input_plane_stride;
    uint output_plane_stride;
    uint matrix_mode;
    uint limited_range;
};

cbuffer SceneConstants : register(b1)
{
    uint scene_sample_stride;
    uint scene_pixel_threshold;
    uint scene_reserved0;
    uint scene_reserved1;
};

Texture2D<float> input_y0 : register(t0);
Texture2D<float> input_y1 : register(t1);
Texture2D<float2> input_uv0 : register(t2);
Texture2D<float2> input_uv1 : register(t3);
RWByteAddressBuffer rife_input : register(u0);
RWStructuredBuffer<uint> scene_stats : register(u2);

ByteAddressBuffer rife_output : register(t4);
RWTexture2D<unorm float> output_y : register(u0);
RWTexture2D<unorm float2> output_uv : register(u1);

float code10(float value)
{
    return value * (65535.0 / 64.0);
}

float2 code10(float2 value)
{
    return value * (65535.0 / 64.0);
}

float3 decode_yuv(float y_code, float2 uv_code)
{
    float y;
    float cb;
    float cr;
    if (limited_range != 0) {
        y = (y_code - 64.0) / 876.0;
        cb = (uv_code.x - 512.0) / 896.0;
        cr = (uv_code.y - 512.0) / 896.0;
    } else {
        y = y_code / 1023.0;
        cb = (uv_code.x - 512.0) / 1023.0;
        cr = (uv_code.y - 512.0) / 1023.0;
    }
    if (matrix_mode == 0) {
        return float3(
            y + 1.402 * cr,
            y - 0.3441362862 * cb - 0.7141362862 * cr,
            y + 1.772 * cb);
    }
    if (matrix_mode == 2) {
        return float3(
            y + 1.4746 * cr,
            y - 0.1645531268 * cb - 0.5713531268 * cr,
            y + 1.8814 * cb);
    }
    return float3(
        y + 1.5748 * cr,
        y - 0.1873242729 * cb - 0.4681242729 * cr,
        y + 1.8556 * cb);
}

float3 load_rgb(uint frame_index, uint2 position)
{
    if (position.x >= source_width || position.y >= source_height)
        return 0.0;
    float y = frame_index == 0
        ? input_y0.Load(int3(position, 0))
        : input_y1.Load(int3(position, 0));
    float2 uv = frame_index == 0
        ? input_uv0.Load(int3(position / 2, 0))
        : input_uv1.Load(int3(position / 2, 0));
    return decode_yuv(code10(y), code10(uv));
}

void store_half_pair(uint channel, uint pair_index, float left, float right)
{
    uint packed = f32tof16(left) | (f32tof16(right) << 16);
    rife_input.Store(channel * input_plane_stride * 2 + pair_index * 4, packed);
}

[numthreads(8, 8, 1)]
void prepare_input(uint3 dispatch_id : SV_DispatchThreadID)
{
    uint pair_x = dispatch_id.x;
    uint y = dispatch_id.y;
    uint pair_width = padded_width / 2;
    if (pair_x >= pair_width || y >= padded_height)
        return;
    uint x0 = pair_x * 2;
    uint x1 = x0 + 1;
    float3 frame0_left = load_rgb(0, uint2(x0, y));
    float3 frame0_right = load_rgb(0, uint2(x1, y));
    float3 frame1_left = load_rgb(1, uint2(x0, y));
    float3 frame1_right = load_rgb(1, uint2(x1, y));
    uint pair_index = y * pair_width + pair_x;
    store_half_pair(0, pair_index, frame0_left.r, frame0_right.r);
    store_half_pair(1, pair_index, frame0_left.g, frame0_right.g);
    store_half_pair(2, pair_index, frame0_left.b, frame0_right.b);
    store_half_pair(3, pair_index, frame1_left.r, frame1_right.r);
    store_half_pair(4, pair_index, frame1_left.g, frame1_right.g);
    store_half_pair(5, pair_index, frame1_left.b, frame1_right.b);
    store_half_pair(6, pair_index, 0.5, 0.5);
    store_half_pair(7, pair_index,
                    2.0 * x0 / (padded_width - 1.0) - 1.0,
                    2.0 * x1 / (padded_width - 1.0) - 1.0);
    float vertical = 2.0 * y / (padded_height - 1.0) - 1.0;
    store_half_pair(8, pair_index, vertical, vertical);
    store_half_pair(9, pair_index,
                    2.0 / (padded_width - 1.0),
                    2.0 / (padded_width - 1.0));
    store_half_pair(10, pair_index,
                    2.0 / (padded_height - 1.0),
                    2.0 / (padded_height - 1.0));
}

[numthreads(8, 8, 1)]
void detect_scene(uint3 dispatch_id : SV_DispatchThreadID)
{
    uint2 position = dispatch_id.xy * scene_sample_stride;
    if (position.x >= source_width || position.y >= source_height)
        return;
    float y0 = input_y0.Load(int3(position, 0));
    float y1 = input_y1.Load(int3(position, 0));
    uint delta = (uint)round(abs(code10(y1) - code10(y0)));
    InterlockedAdd(scene_stats[0], delta);
    if (delta >= scene_pixel_threshold)
        InterlockedAdd(scene_stats[1], 1);
    InterlockedAdd(scene_stats[2], 1);
}

float load_output_channel(uint channel, uint2 position)
{
    uint linear_index = position.y * padded_width + position.x;
    uint byte_offset = channel * output_plane_stride * 2
                     + (linear_index & ~1u) * 2;
    uint packed = rife_output.Load(byte_offset);
    uint bits = (linear_index & 1u) == 0 ? packed & 0xffff : packed >> 16;
    return f16tof32(bits);
}

float3 load_output_rgb(uint2 position)
{
    return float3(
        load_output_channel(0, position),
        load_output_channel(1, position),
        load_output_channel(2, position));
}

float3 encode_yuv(float3 rgb)
{
    float y;
    float cb;
    float cr;
    if (matrix_mode == 0) {
        y = dot(rgb, float3(0.299, 0.587, 0.114));
        cb = (rgb.b - y) / 1.772;
        cr = (rgb.r - y) / 1.402;
    } else if (matrix_mode == 2) {
        y = dot(rgb, float3(0.2627, 0.6780, 0.0593));
        cb = (rgb.b - y) / 1.8814;
        cr = (rgb.r - y) / 1.4746;
    } else {
        y = dot(rgb, float3(0.2126, 0.7152, 0.0722));
        cb = (rgb.b - y) / 1.8556;
        cr = (rgb.r - y) / 1.5748;
    }
    return float3(y, cb, cr);
}

float p010_unorm(float code)
{
    return round(clamp(code, 0.0, 1023.0)) * (64.0 / 65535.0);
}

[numthreads(8, 8, 1)]
void write_output(uint3 dispatch_id : SV_DispatchThreadID)
{
    uint pair_x = dispatch_id.x;
    uint y = dispatch_id.y;
    uint pair_width = (source_width + 1) / 2;
    if (pair_x >= pair_width || y >= source_height)
        return;
    uint x0 = pair_x * 2;
    uint x1 = min(x0 + 1, source_width - 1);
    float3 yuv0 = encode_yuv(load_output_rgb(uint2(x0, y)));
    float3 yuv1 = encode_yuv(load_output_rgb(uint2(x1, y)));
    float y_code0 = limited_range != 0 ? yuv0.x * 876.0 + 64.0
                                        : yuv0.x * 1023.0;
    float y_code1 = limited_range != 0 ? yuv1.x * 876.0 + 64.0
                                        : yuv1.x * 1023.0;
    output_y[uint2(x0, y)] = p010_unorm(y_code0);
    output_y[uint2(x1, y)] = p010_unorm(y_code1);

    if ((y & 1u) == 0) {
        uint y1 = min(y + 1, source_height - 1);
        float3 lower0 = encode_yuv(load_output_rgb(uint2(x0, y1)));
        float3 lower1 = encode_yuv(load_output_rgb(uint2(x1, y1)));
        float cb = (yuv0.y + yuv1.y + lower0.y + lower1.y) * 0.25;
        float cr = (yuv0.z + yuv1.z + lower0.z + lower1.z) * 0.25;
        float cb_code = limited_range != 0 ? cb * 896.0 + 512.0
                                           : cb * 1023.0 + 512.0;
        float cr_code = limited_range != 0 ? cr * 896.0 + 512.0
                                           : cr * 1023.0 + 512.0;
        output_uv[uint2(pair_x, y / 2)] = float2(
            p010_unorm(cb_code), p010_unorm(cr_code));
    }
}
)hlsl";

void set_error(char *error, size_t capacity, const char *format, ...)
{
    if (!error || capacity == 0)
        return;
    va_list args;
    va_start(args, format);
    vsnprintf(error, capacity, format, args);
    va_end(args);
    error[capacity - 1] = '\0';
}

struct CudaApi {
    HMODULE module = nullptr;
    CudaError(__cdecl *d3d11_set_device)(ID3D11Device *, int) = nullptr;
    CudaError(__cdecl *register_d3d11_resource)(
        CudaGraphicsResource *, ID3D11Resource *, unsigned int) = nullptr;
    CudaError(__cdecl *map_resources)(
        int, CudaGraphicsResource *, CudaStream) = nullptr;
    CudaError(__cdecl *get_mapped_pointer)(
        void **, size_t *, CudaGraphicsResource) = nullptr;
    CudaError(__cdecl *unmap_resources)(
        int, CudaGraphicsResource *, CudaStream) = nullptr;
    CudaError(__cdecl *unregister_resource)(CudaGraphicsResource) = nullptr;
    CudaError(__cdecl *stream_create)(CudaStream *, unsigned int) = nullptr;
    CudaError(__cdecl *stream_synchronize)(CudaStream) = nullptr;
    CudaError(__cdecl *stream_destroy)(CudaStream) = nullptr;
    const char *(__cdecl *error_string)(CudaError) = nullptr;
};

template <typename T>
bool load_symbol(HMODULE module, const char *name, T &output,
                 char *error, size_t error_capacity)
{
    output = reinterpret_cast<T>(GetProcAddress(module, name));
    if (output)
        return true;
    set_error(error, error_capacity, "CUDA symbol is missing: %s", name);
    return false;
}

bool load_cuda(const wchar_t *path, CudaApi &api,
               char *error, size_t error_capacity)
{
    api.module = LoadLibraryW(path);
    if (!api.module) {
        set_error(error, error_capacity,
                  "CUDA Runtime could not be loaded (win32=%lu)", GetLastError());
        return false;
    }
    return load_symbol(api.module, "cudaD3D11SetDirect3DDevice",
                       api.d3d11_set_device, error, error_capacity)
        && load_symbol(api.module, "cudaGraphicsD3D11RegisterResource",
                       api.register_d3d11_resource, error, error_capacity)
        && load_symbol(api.module, "cudaGraphicsMapResources",
                       api.map_resources, error, error_capacity)
        && load_symbol(api.module, "cudaGraphicsResourceGetMappedPointer",
                       api.get_mapped_pointer, error, error_capacity)
        && load_symbol(api.module, "cudaGraphicsUnmapResources",
                       api.unmap_resources, error, error_capacity)
        && load_symbol(api.module, "cudaGraphicsUnregisterResource",
                       api.unregister_resource, error, error_capacity)
        && load_symbol(api.module, "cudaStreamCreateWithFlags",
                       api.stream_create, error, error_capacity)
        && load_symbol(api.module, "cudaStreamSynchronize",
                       api.stream_synchronize, error, error_capacity)
        && load_symbol(api.module, "cudaStreamDestroy",
                       api.stream_destroy, error, error_capacity)
        && load_symbol(api.module, "cudaGetErrorString",
                       api.error_string, error, error_capacity);
}

bool cuda_ok(const CudaApi &api, CudaError status, const char *operation,
             char *error, size_t error_capacity)
{
    if (status == kCudaSuccess)
        return true;
    const char *detail = api.error_string ? api.error_string(status) : "unknown";
    set_error(error, error_capacity, "%s failed: CUDA %d (%s)", operation,
              status, detail ? detail : "unknown");
    return false;
}

class Logger final : public nvinfer1::ILogger {
public:
    void log(Severity severity, const char *message) noexcept override
    {
        if (severity <= Severity::kERROR && message)
            last_error = message;
        if (severity <= Severity::kWARNING && message) {
            OutputDebugStringA("MediaStation RIFE TensorRT: ");
            OutputDebugStringA(message);
            OutputDebugStringA("\n");
        }
    }

    std::string last_error;
};

template <typename T>
struct TrtDelete {
    void operator()(T *value) const noexcept { delete value; }
};

template <typename T>
using TrtPtr = std::unique_ptr<T, TrtDelete<T>>;

std::vector<char> read_file(const wchar_t *path)
{
    std::ifstream stream(path, std::ios::binary | std::ios::ate);
    if (!stream)
        return {};
    const auto length = stream.tellg();
    if (length <= 0)
        return {};
    std::vector<char> data(static_cast<size_t>(length));
    stream.seekg(0, std::ios::beg);
    if (!stream.read(data.data(), length))
        return {};
    return data;
}

size_t data_type_size(nvinfer1::DataType type)
{
    switch (type) {
    case nvinfer1::DataType::kFLOAT: return 4;
    case nvinfer1::DataType::kHALF: return 2;
    case nvinfer1::DataType::kINT8: return 1;
    case nvinfer1::DataType::kINT32: return 4;
    case nvinfer1::DataType::kBOOL: return 1;
    case nvinfer1::DataType::kUINT8: return 1;
    case nvinfer1::DataType::kFP8: return 1;
    case nvinfer1::DataType::kBF16: return 2;
    case nvinfer1::DataType::kINT64: return 8;
    case nvinfer1::DataType::kINT4: return 1;
    case nvinfer1::DataType::kFP4: return 1;
    default: return 0;
    }
}

bool tensor_size(const nvinfer1::Dims &dims, nvinfer1::DataType type,
                 size_t &bytes)
{
    size_t elements = 1;
    for (int index = 0; index < dims.nbDims; ++index) {
        if (dims.d[index] <= 0)
            return false;
        const size_t dimension = static_cast<size_t>(dims.d[index]);
        if (elements > SIZE_MAX / dimension)
            return false;
        elements *= dimension;
    }
    const size_t element_size = data_type_size(type);
    if (!element_size || elements > SIZE_MAX / element_size)
        return false;
    bytes = elements * element_size;
    return true;
}

bool create_raw_buffer(ID3D11Device *device, size_t bytes,
                       ComPtr<ID3D11Buffer> &buffer)
{
    if (!bytes || bytes > UINT32_MAX || bytes % 4 != 0)
        return false;
    D3D11_BUFFER_DESC desc{};
    desc.ByteWidth = static_cast<UINT>(bytes);
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE
                   | D3D11_BIND_UNORDERED_ACCESS;
    desc.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS;
    return SUCCEEDED(device->CreateBuffer(&desc, nullptr, &buffer));
}

bool create_raw_uav(ID3D11Device *device, ID3D11Buffer *buffer, size_t bytes,
                    ComPtr<ID3D11UnorderedAccessView> &view)
{
    D3D11_UNORDERED_ACCESS_VIEW_DESC desc{};
    desc.Format = DXGI_FORMAT_R32_TYPELESS;
    desc.ViewDimension = D3D11_UAV_DIMENSION_BUFFER;
    desc.Buffer.NumElements = static_cast<UINT>(bytes / 4);
    desc.Buffer.Flags = D3D11_BUFFER_UAV_FLAG_RAW;
    return SUCCEEDED(device->CreateUnorderedAccessView(buffer, &desc, &view));
}

bool create_raw_srv(ID3D11Device *device, ID3D11Buffer *buffer, size_t bytes,
                    ComPtr<ID3D11ShaderResourceView> &view)
{
    D3D11_SHADER_RESOURCE_VIEW_DESC desc{};
    desc.Format = DXGI_FORMAT_R32_TYPELESS;
    desc.ViewDimension = D3D11_SRV_DIMENSION_BUFFEREX;
    desc.BufferEx.NumElements = static_cast<UINT>(bytes / 4);
    desc.BufferEx.Flags = D3D11_BUFFEREX_SRV_FLAG_RAW;
    return SUCCEEDED(device->CreateShaderResourceView(buffer, &desc, &view));
}

bool create_scene_buffers(ID3D11Device *device,
                          ComPtr<ID3D11Buffer> &buffer,
                          ComPtr<ID3D11UnorderedAccessView> &uav,
                          ComPtr<ID3D11Buffer> &readback)
{
    D3D11_BUFFER_DESC desc{};
    desc.ByteWidth = 3 * sizeof(uint32_t);
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_UNORDERED_ACCESS;
    desc.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED;
    desc.StructureByteStride = sizeof(uint32_t);
    if (FAILED(device->CreateBuffer(&desc, nullptr, &buffer)))
        return false;
    D3D11_UNORDERED_ACCESS_VIEW_DESC uav_desc{};
    uav_desc.Format = DXGI_FORMAT_UNKNOWN;
    uav_desc.ViewDimension = D3D11_UAV_DIMENSION_BUFFER;
    uav_desc.Buffer.NumElements = 3;
    if (FAILED(device->CreateUnorderedAccessView(buffer.Get(), &uav_desc, &uav)))
        return false;
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.MiscFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    return SUCCEEDED(device->CreateBuffer(&desc, nullptr, &readback));
}

bool compile_shader(ID3D11Device *device, const char *entrypoint,
                    ComPtr<ID3D11ComputeShader> &shader,
                    char *error, size_t error_capacity)
{
    ComPtr<ID3DBlob> bytecode;
    ComPtr<ID3DBlob> errors;
    const HRESULT status = D3DCompile(
        kShaderSource, sizeof(kShaderSource) - 1, "rife_runtime.hlsl", nullptr,
        nullptr, entrypoint, "cs_5_0",
        D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_WARNINGS_ARE_ERRORS,
        0, &bytecode, &errors);
    if (FAILED(status)) {
        const char *detail = errors
            ? static_cast<const char *>(errors->GetBufferPointer()) : "unknown";
        set_error(error, error_capacity,
                  "D3DCompile failed for %s (hr=0x%08lx): %s", entrypoint,
                  static_cast<unsigned long>(status), detail);
        return false;
    }
    const HRESULT create = device->CreateComputeShader(
        bytecode->GetBufferPointer(), bytecode->GetBufferSize(), nullptr,
        &shader);
    if (FAILED(create)) {
        set_error(error, error_capacity,
                  "CreateComputeShader failed for %s (hr=0x%08lx)",
                  entrypoint, static_cast<unsigned long>(create));
        return false;
    }
    return true;
}

template <typename T>
bool create_constant_buffer(ID3D11Device *device, const T &value,
                            ComPtr<ID3D11Buffer> &buffer)
{
    static_assert(sizeof(T) % 16 == 0);
    D3D11_BUFFER_DESC desc{};
    desc.ByteWidth = sizeof(T);
    desc.Usage = D3D11_USAGE_IMMUTABLE;
    desc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
    D3D11_SUBRESOURCE_DATA data{};
    data.pSysMem = &value;
    return SUCCEEDED(device->CreateBuffer(&desc, &data, &buffer));
}

bool create_input_views(ID3D11Device3 *device, ID3D11Texture2D *texture,
                        uint32_t slice,
                        ComPtr<ID3D11ShaderResourceView1> &y,
                        ComPtr<ID3D11ShaderResourceView1> &uv)
{
    D3D11_TEXTURE2D_DESC texture_desc{};
    texture->GetDesc(&texture_desc);
    D3D11_SHADER_RESOURCE_VIEW_DESC1 desc{};
    desc.Format = DXGI_FORMAT_R16_UNORM;
    if (texture_desc.ArraySize > 1) {
        desc.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2DARRAY;
        desc.Texture2DArray.MostDetailedMip = 0;
        desc.Texture2DArray.MipLevels = 1;
        desc.Texture2DArray.FirstArraySlice = slice;
        desc.Texture2DArray.ArraySize = 1;
        desc.Texture2DArray.PlaneSlice = 0;
    } else {
        desc.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D;
        desc.Texture2D.MostDetailedMip = 0;
        desc.Texture2D.MipLevels = 1;
        desc.Texture2D.PlaneSlice = 0;
    }
    if (FAILED(device->CreateShaderResourceView1(texture, &desc, &y)))
        return false;
    desc.Format = DXGI_FORMAT_R16G16_UNORM;
    if (texture_desc.ArraySize > 1)
        desc.Texture2DArray.PlaneSlice = 1;
    else
        desc.Texture2D.PlaneSlice = 1;
    return SUCCEEDED(device->CreateShaderResourceView1(texture, &desc, &uv));
}

bool create_output_views(ID3D11Device3 *device, ID3D11Texture2D *texture,
                         uint32_t slice,
                         ComPtr<ID3D11UnorderedAccessView1> &y,
                         ComPtr<ID3D11UnorderedAccessView1> &uv)
{
    D3D11_TEXTURE2D_DESC texture_desc{};
    texture->GetDesc(&texture_desc);
    D3D11_UNORDERED_ACCESS_VIEW_DESC1 desc{};
    desc.Format = DXGI_FORMAT_R16_UNORM;
    if (texture_desc.ArraySize > 1) {
        desc.ViewDimension = D3D11_UAV_DIMENSION_TEXTURE2DARRAY;
        desc.Texture2DArray.MipSlice = 0;
        desc.Texture2DArray.FirstArraySlice = slice;
        desc.Texture2DArray.ArraySize = 1;
        desc.Texture2DArray.PlaneSlice = 0;
    } else {
        desc.ViewDimension = D3D11_UAV_DIMENSION_TEXTURE2D;
        desc.Texture2D.MipSlice = 0;
        desc.Texture2D.PlaneSlice = 0;
    }
    if (FAILED(device->CreateUnorderedAccessView1(texture, &desc, &y)))
        return false;
    desc.Format = DXGI_FORMAT_R16G16_UNORM;
    if (texture_desc.ArraySize > 1)
        desc.Texture2DArray.PlaneSlice = 1;
    else
        desc.Texture2D.PlaneSlice = 1;
    return SUCCEEDED(device->CreateUnorderedAccessView1(texture, &desc, &uv));
}

struct RifeConstants {
    uint32_t source_width;
    uint32_t source_height;
    uint32_t padded_width;
    uint32_t padded_height;
    uint32_t input_plane_stride;
    uint32_t output_plane_stride;
    uint32_t matrix_mode;
    uint32_t limited_range;
};

struct SceneConstants {
    uint32_t sample_stride;
    uint32_t pixel_threshold;
    uint32_t reserved[2];
};

class Runtime {
public:
    ~Runtime()
    {
        if (stream_ && cuda_.stream_destroy)
            cuda_.stream_destroy(stream_);
        if (output_resource_ && cuda_.unregister_resource)
            cuda_.unregister_resource(output_resource_);
        if (input_resource_ && cuda_.unregister_resource)
            cuda_.unregister_resource(input_resource_);
        if (cuda_.module)
            FreeLibrary(cuda_.module);
    }

    bool initialize(const rife_runtime_config &config,
                    char *error, size_t error_capacity)
    {
        config_ = config;
        device_ = config.device;
        context_ = config.context;
        if (FAILED(device_.As(&device3_))) {
            set_error(error, error_capacity,
                      "RIFE requires ID3D11Device3 planar views");
            return false;
        }
        if (!load_cuda(config.cuda_runtime_path, cuda_, error, error_capacity))
            return false;
        if (!cuda_ok(cuda_, cuda_.d3d11_set_device(device_.Get(), -1),
                     "cudaD3D11SetDirect3DDevice", error, error_capacity))
            return false;

        const std::vector<char> engine_data = read_file(config.engine_path);
        if (engine_data.empty()) {
            set_error(error, error_capacity, "TensorRT engine could not be read");
            return false;
        }
        trt_runtime_.reset(nvinfer1::createInferRuntime(logger_));
        if (!trt_runtime_) {
            set_error(error, error_capacity, "TensorRT runtime creation failed: %s",
                      logger_.last_error.c_str());
            return false;
        }
        engine_.reset(trt_runtime_->deserializeCudaEngine(
            engine_data.data(), engine_data.size()));
        if (!engine_) {
            set_error(error, error_capacity,
                      "TensorRT engine deserialization failed: %s",
                      logger_.last_error.c_str());
            return false;
        }
        execution_.reset(engine_->createExecutionContext());
        if (!execution_) {
            set_error(error, error_capacity,
                      "TensorRT execution context creation failed");
            return false;
        }
        if (!validate_engine(error, error_capacity))
            return false;
        if (!create_resources(error, error_capacity))
            return false;
        return true;
    }

    int process(ID3D11Texture2D *frame0, uint32_t frame0_slice,
                ID3D11Texture2D *frame1, uint32_t frame1_slice,
                ID3D11Texture2D *output, uint32_t output_slice,
                int *scene_cut, char *error, size_t error_capacity)
    {
        if (!validate_texture(frame0, frame0_slice, false, "frame0", error,
                              error_capacity)
            || !validate_texture(frame1, frame1_slice, false, "frame1", error,
                                 error_capacity)
            || !validate_texture(output, output_slice, true, "output", error,
                                 error_capacity)) {
            stats_.failures++;
            return RIFE_RUNTIME_D3D11_FAILED;
        }

        ComPtr<ID3D11ShaderResourceView1> input_y0;
        ComPtr<ID3D11ShaderResourceView1> input_y1;
        ComPtr<ID3D11ShaderResourceView1> input_uv0;
        ComPtr<ID3D11ShaderResourceView1> input_uv1;
        ComPtr<ID3D11UnorderedAccessView1> output_y;
        ComPtr<ID3D11UnorderedAccessView1> output_uv;
        if (!create_input_views(device3_.Get(), frame0, frame0_slice,
                                input_y0, input_uv0)
            || !create_input_views(device3_.Get(), frame1, frame1_slice,
                                   input_y1, input_uv1)
            || !create_output_views(device3_.Get(), output, output_slice,
                                    output_y, output_uv)) {
            set_error(error, error_capacity,
                      "P010 SRV/UAV creation failed for the current frame pair");
            stats_.failures++;
            return RIFE_RUNTIME_D3D11_FAILED;
        }

        ID3D11ShaderResourceView *input_srvs[] = {
            input_y0.Get(), input_y1.Get(), input_uv0.Get(), input_uv1.Get(),
        };
        const auto scene_started = std::chrono::steady_clock::now();
        bool cut = false;
        if (!detect_scene(input_srvs, cut, error, error_capacity)) {
            stats_.failures++;
            return RIFE_RUNTIME_D3D11_FAILED;
        }
        const auto scene_ended = std::chrono::steady_clock::now();
        const double scene_ms = std::chrono::duration<double, std::milli>(
            scene_ended - scene_started).count();
        stats_.scene_total_ms += scene_ms;
        stats_.scene_max_ms = std::max(stats_.scene_max_ms, scene_ms);
        stats_.pairs++;

        if (cut) {
            D3D11_TEXTURE2D_DESC source_desc{};
            frame0->GetDesc(&source_desc);
            D3D11_BOX box{};
            box.right = source_desc.Width;
            box.bottom = source_desc.Height;
            box.back = 1;
            context_->CopySubresourceRegion(
                output, output_slice, 0, 0, 0, frame0, frame0_slice, &box);
            stats_.scene_cuts++;
            *scene_cut = 1;
            return RIFE_RUNTIME_OK;
        }

        const auto inference_started = std::chrono::steady_clock::now();
        if (!run_inference(input_srvs, output_y.Get(), output_uv.Get(),
                           error, error_capacity)) {
            stats_.failures++;
            return RIFE_RUNTIME_INFERENCE_FAILED;
        }
        const auto inference_ended = std::chrono::steady_clock::now();
        const double inference_ms = std::chrono::duration<double, std::milli>(
            inference_ended - inference_started).count();
        record_inference(inference_ms);
        stats_.inferred_pairs++;
        *scene_cut = 0;
        return RIFE_RUNTIME_OK;
    }

    void reset()
    {
        // RIFE inference is pair-local. The mpv filter owns temporal history.
    }

    rife_runtime_stats stats() const
    {
        rife_runtime_stats result = stats_;
        if (result.inferred_pairs) {
            const uint64_t target = static_cast<uint64_t>(
                std::ceil(result.inferred_pairs * 0.95));
            uint64_t accumulated = 0;
            for (size_t index = 0; index < inference_histogram_.size(); ++index) {
                accumulated += inference_histogram_[index];
                if (accumulated >= target) {
                    result.inference_p95_ms =
                        static_cast<double>(index) * kInferenceHistogramBucketMs;
                    break;
                }
            }
        }
        return result;
    }

private:
    bool validate_engine(char *error, size_t error_capacity)
    {
        if (engine_->getNbIOTensors() != 2) {
            set_error(error, error_capacity,
                      "TensorRT engine must expose exactly two IO tensors");
            return false;
        }
        for (int index = 0; index < engine_->getNbIOTensors(); ++index) {
            const char *name = engine_->getIOTensorName(index);
            if (!name)
                continue;
            if (engine_->getTensorIOMode(name)
                == nvinfer1::TensorIOMode::kINPUT)
                input_name_ = name;
            else
                output_name_ = name;
        }
        if (!input_name_ || !output_name_) {
            set_error(error, error_capacity,
                      "TensorRT engine input or output tensor is missing");
            return false;
        }
        const nvinfer1::Dims input_dims = engine_->getTensorShape(input_name_);
        const nvinfer1::Dims output_dims = engine_->getTensorShape(output_name_);
        if (input_dims.nbDims != 4 || output_dims.nbDims != 4
            || input_dims.d[0] != 1 || input_dims.d[1] != 11
            || output_dims.d[0] != 1 || output_dims.d[1] != 3
            || input_dims.d[2] != output_dims.d[2]
            || input_dims.d[3] != output_dims.d[3]
            || config_.source_width > static_cast<uint32_t>(input_dims.d[3])
            || config_.source_height > static_cast<uint32_t>(input_dims.d[2])
            || engine_->getTensorDataType(input_name_)
                   != nvinfer1::DataType::kHALF
            || engine_->getTensorDataType(output_name_)
                   != nvinfer1::DataType::kHALF
            || !tensor_size(input_dims, engine_->getTensorDataType(input_name_),
                            input_bytes_)
            || !tensor_size(output_dims,
                            engine_->getTensorDataType(output_name_),
                            output_bytes_)) {
            set_error(error, error_capacity,
                      "TensorRT engine shape or FP16 IO is incompatible with %ux%u",
                      config_.source_width, config_.source_height);
            return false;
        }
        padded_width_ = static_cast<uint32_t>(input_dims.d[3]);
        padded_height_ = static_cast<uint32_t>(input_dims.d[2]);
        if ((padded_width_ & 1u) != 0) {
            set_error(error, error_capacity,
                      "TensorRT engine width must be even, received %u",
                      padded_width_);
            return false;
        }
        return true;
    }

    bool create_resources(char *error, size_t error_capacity)
    {
        if (!create_raw_buffer(device_.Get(), input_bytes_, input_buffer_)
            || !create_raw_buffer(device_.Get(), output_bytes_, output_buffer_)
            || !create_raw_uav(device_.Get(), input_buffer_.Get(), input_bytes_,
                               input_tensor_uav_)
            || !create_raw_srv(device_.Get(), output_buffer_.Get(), output_bytes_,
                               output_tensor_srv_)
            || !create_scene_buffers(device_.Get(), scene_buffer_, scene_uav_,
                                     scene_readback_)
            || !compile_shader(device_.Get(), "prepare_input", prepare_shader_,
                               error, error_capacity)
            || !compile_shader(device_.Get(), "detect_scene", scene_shader_,
                               error, error_capacity)
            || !compile_shader(device_.Get(), "write_output", output_shader_,
                               error, error_capacity)) {
            if (!error || !error[0])
                set_error(error, error_capacity,
                          "RIFE D3D11 shader resources could not be created");
            return false;
        }
        const RifeConstants constants{
            config_.source_width,
            config_.source_height,
            padded_width_,
            padded_height_,
            padded_width_ * padded_height_,
            padded_width_ * padded_height_,
            config_.color_matrix,
            config_.limited_range,
        };
        const SceneConstants scene_constants{
            config_.scene_sample_stride,
            config_.scene_pixel_threshold,
            {0, 0},
        };
        if (!create_constant_buffer(device_.Get(), constants, constants_buffer_)
            || !create_constant_buffer(device_.Get(), scene_constants,
                                       scene_constants_buffer_)) {
            set_error(error, error_capacity,
                      "RIFE constant buffers could not be created");
            return false;
        }
        D3D11_QUERY_DESC query_desc{};
        query_desc.Query = D3D11_QUERY_EVENT;
        if (FAILED(device_->CreateQuery(&query_desc, &completion_query_))) {
            set_error(error, error_capacity,
                      "RIFE D3D11 completion query could not be created");
            return false;
        }
        if (!cuda_ok(cuda_, cuda_.register_d3d11_resource(
                               &input_resource_, input_buffer_.Get(), 0),
                     "cudaGraphicsD3D11RegisterResource(input)", error,
                     error_capacity)
            || !cuda_ok(cuda_, cuda_.register_d3d11_resource(
                               &output_resource_, output_buffer_.Get(), 0),
                        "cudaGraphicsD3D11RegisterResource(output)", error,
                        error_capacity))
            return false;
        if (!cuda_ok(cuda_, cuda_.stream_create(&stream_, kCudaStreamNonBlocking),
                     "cudaStreamCreateWithFlags", error, error_capacity))
            return false;
        return true;
    }

    bool validate_texture(ID3D11Texture2D *texture, uint32_t slice,
                          bool output, const char *label,
                          char *error, size_t error_capacity) const
    {
        if (!texture) {
            set_error(error, error_capacity, "%s texture is null", label);
            return false;
        }
        ComPtr<ID3D11Device> texture_device;
        texture->GetDevice(&texture_device);
        D3D11_TEXTURE2D_DESC desc{};
        texture->GetDesc(&desc);
        const UINT required_bind = output ? D3D11_BIND_UNORDERED_ACCESS
                                          : D3D11_BIND_SHADER_RESOURCE;
        if (texture_device.Get() != device_.Get()
            || desc.Format != DXGI_FORMAT_P010
            || desc.Width < config_.source_width
            || desc.Height < config_.source_height
            || slice >= desc.ArraySize
            || (desc.BindFlags & required_bind) != required_bind) {
            set_error(error, error_capacity,
                      "%s P010 texture is incompatible: size=%ux%u array=%u "
                      "slice=%u format=%u bind=0x%x", label, desc.Width,
                      desc.Height, desc.ArraySize, slice,
                      static_cast<unsigned int>(desc.Format), desc.BindFlags);
            return false;
        }
        return true;
    }

    bool detect_scene(ID3D11ShaderResourceView **input_srvs, bool &cut,
                      char *error, size_t error_capacity)
    {
        const UINT clear[4] = {0, 0, 0, 0};
        context_->ClearUnorderedAccessViewUint(scene_uav_.Get(), clear);
        ID3D11Buffer *constants[] = {
            constants_buffer_.Get(), scene_constants_buffer_.Get(),
        };
        ID3D11UnorderedAccessView *uavs[3] = {nullptr, nullptr,
                                             scene_uav_.Get()};
        context_->CSSetShader(scene_shader_.Get(), nullptr, 0);
        context_->CSSetConstantBuffers(0, 2, constants);
        context_->CSSetShaderResources(0, 4, input_srvs);
        context_->CSSetUnorderedAccessViews(0, 3, uavs, nullptr);
        const UINT sampled_width =
            (config_.source_width + config_.scene_sample_stride - 1)
            / config_.scene_sample_stride;
        const UINT sampled_height =
            (config_.source_height + config_.scene_sample_stride - 1)
            / config_.scene_sample_stride;
        context_->Dispatch((sampled_width + 7) / 8,
                           (sampled_height + 7) / 8, 1);
        ID3D11ShaderResourceView *null_srvs[4]{};
        ID3D11UnorderedAccessView *null_uavs[3]{};
        context_->CSSetShaderResources(0, 4, null_srvs);
        context_->CSSetUnorderedAccessViews(0, 3, null_uavs, nullptr);
        context_->CSSetShader(nullptr, nullptr, 0);
        context_->CopyResource(scene_readback_.Get(), scene_buffer_.Get());
        D3D11_MAPPED_SUBRESOURCE mapped{};
        const HRESULT status = context_->Map(scene_readback_.Get(), 0,
                                             D3D11_MAP_READ, 0, &mapped);
        if (FAILED(status)) {
            set_error(error, error_capacity,
                      "Scene detector readback failed (hr=0x%08lx)",
                      static_cast<unsigned long>(status));
            return false;
        }
        const auto *values = static_cast<const uint32_t *>(mapped.pData);
        const uint32_t delta_sum = values[0];
        const uint32_t changed = values[1];
        const uint32_t samples = values[2];
        context_->Unmap(scene_readback_.Get(), 0);
        if (!samples) {
            set_error(error, error_capacity,
                      "Scene detector produced no samples");
            return false;
        }
        const double average = static_cast<double>(delta_sum) / samples;
        const double changed_ratio = static_cast<double>(changed) / samples;
        cut = average >= config_.scene_average_threshold
           && changed_ratio >= config_.scene_changed_ratio;
        return true;
    }

    bool run_inference(ID3D11ShaderResourceView **input_srvs,
                       ID3D11UnorderedAccessView *output_y,
                       ID3D11UnorderedAccessView *output_uv,
                       char *error, size_t error_capacity)
    {
        ID3D11Buffer *constants[] = {constants_buffer_.Get()};
        ID3D11UnorderedAccessView *input_uavs[] = {input_tensor_uav_.Get()};
        context_->CSSetShader(prepare_shader_.Get(), nullptr, 0);
        context_->CSSetConstantBuffers(0, 1, constants);
        context_->CSSetShaderResources(0, 4, input_srvs);
        context_->CSSetUnorderedAccessViews(0, 1, input_uavs, nullptr);
        context_->Dispatch((padded_width_ / 2 + 7) / 8,
                           (padded_height_ + 7) / 8, 1);
        ID3D11ShaderResourceView *null_input_srvs[4]{};
        ID3D11UnorderedAccessView *null_input_uavs[1]{};
        context_->CSSetShaderResources(0, 4, null_input_srvs);
        context_->CSSetUnorderedAccessViews(0, 1, null_input_uavs, nullptr);
        context_->CSSetShader(nullptr, nullptr, 0);

        CudaGraphicsResource resources[] = {input_resource_, output_resource_};
        if (!cuda_ok(cuda_, cuda_.map_resources(2, resources, stream_),
                     "cudaGraphicsMapResources", error, error_capacity))
            return false;
        bool mapped = true;
        void *input_pointer = nullptr;
        void *output_pointer = nullptr;
        size_t mapped_input_bytes = 0;
        size_t mapped_output_bytes = 0;
        bool ok = cuda_ok(cuda_, cuda_.get_mapped_pointer(
                                    &input_pointer, &mapped_input_bytes,
                                    input_resource_),
                          "cudaGraphicsResourceGetMappedPointer(input)", error,
                          error_capacity)
            && cuda_ok(cuda_, cuda_.get_mapped_pointer(
                                  &output_pointer, &mapped_output_bytes,
                                  output_resource_),
                       "cudaGraphicsResourceGetMappedPointer(output)", error,
                       error_capacity);
        if (ok && (mapped_input_bytes < input_bytes_
                   || mapped_output_bytes < output_bytes_)) {
            set_error(error, error_capacity,
                      "CUDA mapped buffers are smaller than TensorRT IO");
            ok = false;
        }
        if (ok && (!execution_->setTensorAddress(input_name_, input_pointer)
                   || !execution_->setTensorAddress(output_name_, output_pointer))) {
            set_error(error, error_capacity,
                      "TensorRT tensor address binding failed");
            ok = false;
        }
        if (ok && !execution_->enqueueV3(stream_)) {
            set_error(error, error_capacity, "TensorRT enqueueV3 failed: %s",
                      logger_.last_error.c_str());
            ok = false;
        }
        if (ok && !cuda_ok(cuda_, cuda_.stream_synchronize(stream_),
                           "cudaStreamSynchronize", error, error_capacity))
            ok = false;
        if (mapped && !cuda_ok(cuda_, cuda_.unmap_resources(2, resources, stream_),
                               "cudaGraphicsUnmapResources", error,
                               error_capacity))
            ok = false;
        if (!ok)
            return false;

        ID3D11ShaderResourceView *output_srvs[5]{};
        output_srvs[4] = output_tensor_srv_.Get();
        ID3D11UnorderedAccessView *output_uavs[] = {output_y, output_uv};
        context_->CSSetShader(output_shader_.Get(), nullptr, 0);
        context_->CSSetConstantBuffers(0, 1, constants);
        context_->CSSetShaderResources(0, 5, output_srvs);
        context_->CSSetUnorderedAccessViews(0, 2, output_uavs, nullptr);
        const UINT pair_width = (config_.source_width + 1) / 2;
        context_->Dispatch((pair_width + 7) / 8,
                           (config_.source_height + 7) / 8, 1);
        ID3D11ShaderResourceView *null_output_srvs[5]{};
        ID3D11UnorderedAccessView *null_output_uavs[2]{};
        context_->CSSetShaderResources(0, 5, null_output_srvs);
        context_->CSSetUnorderedAccessViews(0, 2, null_output_uavs, nullptr);
        context_->CSSetShader(nullptr, nullptr, 0);
        context_->End(completion_query_.Get());
        BOOL complete = FALSE;
        for (;;) {
            const HRESULT status = context_->GetData(
                completion_query_.Get(), &complete, sizeof(complete), 0);
            if (status == S_OK && complete)
                return true;
            if (FAILED(status)) {
                set_error(error, error_capacity,
                          "D3D11 completion wait failed (hr=0x%08lx)",
                          static_cast<unsigned long>(status));
                return false;
            }
            SwitchToThread();
        }
    }

    void record_inference(double milliseconds)
    {
        stats_.inference_total_ms += milliseconds;
        stats_.inference_max_ms = std::max(stats_.inference_max_ms,
                                          milliseconds);
        const size_t bucket = std::min(
            static_cast<size_t>(milliseconds / kInferenceHistogramBucketMs),
            inference_histogram_.size() - 1);
        inference_histogram_[bucket]++;
    }

    rife_runtime_config config_{};
    CudaApi cuda_{};
    CudaStream stream_ = nullptr;
    CudaGraphicsResource input_resource_ = nullptr;
    CudaGraphicsResource output_resource_ = nullptr;
    Logger logger_;
    TrtPtr<nvinfer1::IRuntime> trt_runtime_;
    TrtPtr<nvinfer1::ICudaEngine> engine_;
    TrtPtr<nvinfer1::IExecutionContext> execution_;
    const char *input_name_ = nullptr;
    const char *output_name_ = nullptr;
    size_t input_bytes_ = 0;
    size_t output_bytes_ = 0;
    uint32_t padded_width_ = 0;
    uint32_t padded_height_ = 0;
    ComPtr<ID3D11Device> device_;
    ComPtr<ID3D11DeviceContext> context_;
    ComPtr<ID3D11Device3> device3_;
    ComPtr<ID3D11Buffer> input_buffer_;
    ComPtr<ID3D11Buffer> output_buffer_;
    ComPtr<ID3D11UnorderedAccessView> input_tensor_uav_;
    ComPtr<ID3D11ShaderResourceView> output_tensor_srv_;
    ComPtr<ID3D11Buffer> scene_buffer_;
    ComPtr<ID3D11UnorderedAccessView> scene_uav_;
    ComPtr<ID3D11Buffer> scene_readback_;
    ComPtr<ID3D11ComputeShader> prepare_shader_;
    ComPtr<ID3D11ComputeShader> scene_shader_;
    ComPtr<ID3D11ComputeShader> output_shader_;
    ComPtr<ID3D11Buffer> constants_buffer_;
    ComPtr<ID3D11Buffer> scene_constants_buffer_;
    ComPtr<ID3D11Query> completion_query_;
    rife_runtime_stats stats_{};
    std::array<uint64_t, kInferenceHistogramBuckets> inference_histogram_{};
};

} // namespace

struct rife_runtime {
    std::unique_ptr<Runtime> implementation;
};

extern "C" uint32_t __cdecl rife_runtime_abi_version(void)
{
    return RIFE_RUNTIME_ABI_VERSION;
}

extern "C" struct rife_runtime *__cdecl rife_runtime_create(
    const struct rife_runtime_config *config,
    char *error,
    size_t error_capacity)
{
    if (error && error_capacity)
        error[0] = '\0';
    if (!config || config->abi_version != RIFE_RUNTIME_ABI_VERSION
        || !config->device || !config->context || !config->engine_path
        || !config->cuda_runtime_path || config->source_width == 0
        || config->source_height == 0
        || config->color_matrix > RIFE_COLOR_MATRIX_BT2020_NCL
        || config->limited_range > 1 || config->scene_sample_stride < 2
        || config->scene_pixel_threshold == 0
        || !std::isfinite(config->scene_average_threshold)
        || config->scene_average_threshold <= 0
        || !std::isfinite(config->scene_changed_ratio)
        || config->scene_changed_ratio <= 0
        || config->scene_changed_ratio > 1) {
        set_error(error, error_capacity, "RIFE runtime configuration is invalid");
        return nullptr;
    }
    auto runtime = std::make_unique<rife_runtime>();
    runtime->implementation = std::make_unique<Runtime>();
    if (!runtime->implementation->initialize(*config, error, error_capacity))
        return nullptr;
    return runtime.release();
}

extern "C" int __cdecl rife_runtime_process(
    struct rife_runtime *runtime,
    ID3D11Texture2D *frame0,
    uint32_t frame0_slice,
    ID3D11Texture2D *frame1,
    uint32_t frame1_slice,
    ID3D11Texture2D *output,
    uint32_t output_slice,
    int *scene_cut,
    char *error,
    size_t error_capacity)
{
    if (error && error_capacity)
        error[0] = '\0';
    if (!runtime || !runtime->implementation || !scene_cut) {
        set_error(error, error_capacity, "RIFE process arguments are invalid");
        return RIFE_RUNTIME_INVALID_ARGUMENT;
    }
    return runtime->implementation->process(
        frame0, frame0_slice, frame1, frame1_slice, output, output_slice,
        scene_cut, error, error_capacity);
}

extern "C" void __cdecl rife_runtime_reset(struct rife_runtime *runtime)
{
    if (runtime && runtime->implementation)
        runtime->implementation->reset();
}

extern "C" int __cdecl rife_runtime_get_stats(
    const struct rife_runtime *runtime,
    struct rife_runtime_stats *stats)
{
    if (!runtime || !runtime->implementation || !stats)
        return RIFE_RUNTIME_INVALID_ARGUMENT;
    *stats = runtime->implementation->stats();
    return RIFE_RUNTIME_OK;
}

extern "C" void __cdecl rife_runtime_destroy(struct rife_runtime *runtime)
{
    delete runtime;
}
