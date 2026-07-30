#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>

#include <d3d11.h>
#include <d3d11_3.h>
#include <d3dcompiler.h>
#include <dxgi1_2.h>
#include <wrl/client.h>

#include <NvInferRuntime.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <memory>
#include <string>
#include <vector>

using Microsoft::WRL::ComPtr;

namespace {

using CudaError = int;
using CudaStream = cudaStream_t;
using CudaGraphicsResource = void*;

constexpr CudaError kCudaSuccess = 0;
constexpr unsigned int kCudaStreamNonBlocking = 1;

struct CudaApi {
    HMODULE module = nullptr;
    CudaError(__cdecl* d3d11_set_device)(ID3D11Device*, int) = nullptr;
    CudaError(__cdecl* register_d3d11_resource)(
        CudaGraphicsResource*, ID3D11Resource*, unsigned int) = nullptr;
    CudaError(__cdecl* map_resources)(
        int, CudaGraphicsResource*, CudaStream) = nullptr;
    CudaError(__cdecl* get_mapped_pointer)(
        void**, size_t*, CudaGraphicsResource) = nullptr;
    CudaError(__cdecl* unmap_resources)(
        int, CudaGraphicsResource*, CudaStream) = nullptr;
    CudaError(__cdecl* unregister_resource)(CudaGraphicsResource) = nullptr;
    CudaError(__cdecl* stream_create)(CudaStream*, unsigned int) = nullptr;
    CudaError(__cdecl* stream_synchronize)(CudaStream) = nullptr;
    CudaError(__cdecl* stream_destroy)(CudaStream) = nullptr;
    const char*(__cdecl* error_string)(CudaError) = nullptr;
};

template <typename T>
bool load_symbol(HMODULE module, const char* name, T& output)
{
    output = reinterpret_cast<T>(GetProcAddress(module, name));
    if (!output) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_CUDA_SYMBOL_MISSING name=%s\n", name);
        return false;
    }
    return true;
}

bool load_cuda(const wchar_t* path, CudaApi& api)
{
    api.module = LoadLibraryW(path);
    if (!api.module) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_CUDA_LOAD_FAILED win32=%lu\n", GetLastError());
        return false;
    }
    return load_symbol(api.module, "cudaD3D11SetDirect3DDevice", api.d3d11_set_device)
        && load_symbol(api.module, "cudaGraphicsD3D11RegisterResource", api.register_d3d11_resource)
        && load_symbol(api.module, "cudaGraphicsMapResources", api.map_resources)
        && load_symbol(api.module, "cudaGraphicsResourceGetMappedPointer", api.get_mapped_pointer)
        && load_symbol(api.module, "cudaGraphicsUnmapResources", api.unmap_resources)
        && load_symbol(api.module, "cudaGraphicsUnregisterResource", api.unregister_resource)
        && load_symbol(api.module, "cudaStreamCreateWithFlags", api.stream_create)
        && load_symbol(api.module, "cudaStreamSynchronize", api.stream_synchronize)
        && load_symbol(api.module, "cudaStreamDestroy", api.stream_destroy)
        && load_symbol(api.module, "cudaGetErrorString", api.error_string);
}

bool cuda_ok(const CudaApi& api, CudaError status, const char* operation)
{
    if (status == kCudaSuccess)
        return true;
    const char* detail = api.error_string ? api.error_string(status) : "unknown";
    std::fprintf(stderr, "RIFE_D3D11_TRT_CUDA_FAILED operation=%s status=%d detail=%s\n",
                 operation, status, detail ? detail : "unknown");
    return false;
}

class Logger final : public nvinfer1::ILogger {
public:
    void log(Severity severity, const char* message) noexcept override
    {
        if (severity <= Severity::kWARNING)
            std::fprintf(stderr, "TensorRT: %s\n", message);
    }
};

template <typename T>
struct TrtDelete {
    void operator()(T* value) const noexcept { delete value; }
};

template <typename T>
using TrtPtr = std::unique_ptr<T, TrtDelete<T>>;

std::vector<char> read_file(const wchar_t* path)
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

bool find_nvidia_adapter(ComPtr<IDXGIAdapter1>& adapter, std::wstring& description)
{
    ComPtr<IDXGIFactory1> factory;
    if (FAILED(CreateDXGIFactory1(IID_PPV_ARGS(&factory))))
        return false;
    for (UINT index = 0;; ++index) {
        ComPtr<IDXGIAdapter1> candidate;
        if (factory->EnumAdapters1(index, &candidate) == DXGI_ERROR_NOT_FOUND)
            break;
        DXGI_ADAPTER_DESC1 desc{};
        if (FAILED(candidate->GetDesc1(&desc)))
            continue;
        if (desc.VendorId == 0x10DE && !(desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) {
            adapter = candidate;
            description = desc.Description;
            return true;
        }
    }
    return false;
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

bool tensor_size(const nvinfer1::Dims& dims, nvinfer1::DataType type, size_t& bytes)
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

bool create_raw_buffer(ID3D11Device* device, size_t bytes, ComPtr<ID3D11Buffer>& buffer)
{
    if (!bytes || bytes > UINT32_MAX || bytes % 4 != 0)
        return false;
    D3D11_BUFFER_DESC desc{};
    desc.ByteWidth = static_cast<UINT>(bytes);
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS;
    desc.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS;
    return SUCCEEDED(device->CreateBuffer(&desc, nullptr, &buffer));
}

struct ShaderConstants {
    uint32_t source_width;
    uint32_t source_height;
    uint32_t padded_width;
    uint32_t padded_height;
    uint32_t input_plane_stride;
    uint32_t output_plane_stride;
    uint32_t matrix_mode;
    uint32_t limited_range;
};

bool create_p010_texture(ID3D11Device* device, uint32_t width, uint32_t height,
                         UINT bind_flags, ComPtr<ID3D11Texture2D>& texture)
{
    D3D11_TEXTURE2D_DESC desc{};
    desc.Width = width;
    desc.Height = height;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_P010;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = bind_flags;
    return SUCCEEDED(device->CreateTexture2D(&desc, nullptr, &texture));
}

bool initialize_p010_gradient(ID3D11DeviceContext* context, ID3D11Texture2D* texture,
                              uint32_t width, uint32_t height, uint32_t horizontal_shift)
{
    const size_t luma_samples = static_cast<size_t>(width) * height;
    const size_t chroma_samples = luma_samples / 2;
    std::vector<uint16_t> samples(luma_samples + chroma_samples);
    for (uint32_t y = 0; y < height; ++y) {
        for (uint32_t x = 0; x < width; ++x) {
            const uint32_t shifted = (x + horizontal_shift) % width;
            const uint32_t code = 64 + static_cast<uint32_t>(
                (876.0 * shifted / (width - 1)) + 0.5);
            samples[static_cast<size_t>(y) * width + x] = static_cast<uint16_t>(code << 6);
        }
    }
    std::fill(samples.begin() + static_cast<ptrdiff_t>(luma_samples), samples.end(),
              static_cast<uint16_t>(512 << 6));
    context->UpdateSubresource(texture, 0, nullptr, samples.data(), width * 2,
                               static_cast<UINT>(samples.size() * sizeof(uint16_t)));
    return true;
}

struct OutputValidation {
    uint32_t unique_luma_codes = 0;
    uint64_t non_8bit_luma_samples = 0;
    uint32_t center_luma_code = 0;
};

bool validate_p010_output(ID3D11Device* device, ID3D11DeviceContext* context,
                          ID3D11Texture2D* texture, uint32_t width, uint32_t height,
                          OutputValidation& validation)
{
    D3D11_TEXTURE2D_DESC desc{};
    texture->GetDesc(&desc);
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    desc.MiscFlags = 0;
    ComPtr<ID3D11Texture2D> staging;
    if (FAILED(device->CreateTexture2D(&desc, nullptr, &staging)))
        return false;
    context->CopyResource(staging.Get(), texture);
    D3D11_MAPPED_SUBRESOURCE mapped{};
    if (FAILED(context->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped)))
        return false;

    bool packed = true;
    std::array<bool, 1024> seen{};
    uint64_t non_8bit = 0;
    for (uint32_t y = 0; y < height; ++y) {
        const auto* row = reinterpret_cast<const uint16_t*>(
            static_cast<const uint8_t*>(mapped.pData) + static_cast<size_t>(mapped.RowPitch) * y);
        for (uint32_t x = 0; x < width; ++x) {
            const uint16_t stored = row[x];
            packed = packed && (stored & 0x3f) == 0;
            const uint32_t code = stored >> 6;
            seen[code] = true;
            if ((code & 0x3) != 0)
                ++non_8bit;
        }
    }
    const auto* center_row = reinterpret_cast<const uint16_t*>(
        static_cast<const uint8_t*>(mapped.pData)
        + static_cast<size_t>(mapped.RowPitch) * (height / 2));
    validation.center_luma_code = center_row[width / 2] >> 6;
    const auto* chroma_base = static_cast<const uint8_t*>(mapped.pData)
        + static_cast<size_t>(mapped.RowPitch) * height;
    for (uint32_t y = 0; y < height / 2; ++y) {
        const auto* row = reinterpret_cast<const uint16_t*>(
            chroma_base + static_cast<size_t>(mapped.RowPitch) * y);
        for (uint32_t x = 0; x < width; ++x)
            packed = packed && (row[x] & 0x3f) == 0;
    }
    context->Unmap(staging.Get(), 0);
    validation.unique_luma_codes = static_cast<uint32_t>(
        std::count(seen.begin(), seen.end(), true));
    validation.non_8bit_luma_samples = non_8bit;
    return packed && validation.unique_luma_codes > 256
        && validation.non_8bit_luma_samples > 0;
}

bool create_p010_input_views(ID3D11Device3* device, ID3D11Texture2D* texture,
                             ComPtr<ID3D11ShaderResourceView1>& y,
                             ComPtr<ID3D11ShaderResourceView1>& uv)
{
    D3D11_SHADER_RESOURCE_VIEW_DESC1 desc{};
    desc.Format = DXGI_FORMAT_R16_UNORM;
    desc.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D;
    desc.Texture2D.MostDetailedMip = 0;
    desc.Texture2D.MipLevels = 1;
    desc.Texture2D.PlaneSlice = 0;
    if (FAILED(device->CreateShaderResourceView1(texture, &desc, &y)))
        return false;
    desc.Format = DXGI_FORMAT_R16G16_UNORM;
    desc.Texture2D.PlaneSlice = 1;
    return SUCCEEDED(device->CreateShaderResourceView1(texture, &desc, &uv));
}

bool create_p010_output_views(ID3D11Device3* device, ID3D11Texture2D* texture,
                              ComPtr<ID3D11UnorderedAccessView1>& y,
                              ComPtr<ID3D11UnorderedAccessView1>& uv)
{
    D3D11_UNORDERED_ACCESS_VIEW_DESC1 desc{};
    desc.Format = DXGI_FORMAT_R16_UNORM;
    desc.ViewDimension = D3D11_UAV_DIMENSION_TEXTURE2D;
    desc.Texture2D.MipSlice = 0;
    desc.Texture2D.PlaneSlice = 0;
    if (FAILED(device->CreateUnorderedAccessView1(texture, &desc, &y)))
        return false;
    desc.Format = DXGI_FORMAT_R16G16_UNORM;
    desc.Texture2D.PlaneSlice = 1;
    return SUCCEEDED(device->CreateUnorderedAccessView1(texture, &desc, &uv));
}

bool create_raw_uav(ID3D11Device* device, ID3D11Buffer* buffer, size_t bytes,
                    ComPtr<ID3D11UnorderedAccessView>& view)
{
    D3D11_UNORDERED_ACCESS_VIEW_DESC desc{};
    desc.Format = DXGI_FORMAT_R32_TYPELESS;
    desc.ViewDimension = D3D11_UAV_DIMENSION_BUFFER;
    desc.Buffer.NumElements = static_cast<UINT>(bytes / 4);
    desc.Buffer.Flags = D3D11_BUFFER_UAV_FLAG_RAW;
    return SUCCEEDED(device->CreateUnorderedAccessView(buffer, &desc, &view));
}

bool create_raw_srv(ID3D11Device* device, ID3D11Buffer* buffer, size_t bytes,
                    ComPtr<ID3D11ShaderResourceView>& view)
{
    D3D11_SHADER_RESOURCE_VIEW_DESC desc{};
    desc.Format = DXGI_FORMAT_R32_TYPELESS;
    desc.ViewDimension = D3D11_SRV_DIMENSION_BUFFEREX;
    desc.BufferEx.NumElements = static_cast<UINT>(bytes / 4);
    desc.BufferEx.Flags = D3D11_BUFFEREX_SRV_FLAG_RAW;
    return SUCCEEDED(device->CreateShaderResourceView(buffer, &desc, &view));
}

bool compile_compute_shader(ID3D11Device* device, const wchar_t* path,
                            const char* entrypoint, ComPtr<ID3D11ComputeShader>& shader)
{
    ComPtr<ID3DBlob> bytecode;
    ComPtr<ID3DBlob> errors;
    const HRESULT status = D3DCompileFromFile(
        path, nullptr, D3D_COMPILE_STANDARD_FILE_INCLUDE, entrypoint, "cs_5_0",
        D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_WARNINGS_ARE_ERRORS,
        0, &bytecode, &errors);
    if (FAILED(status)) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_SHADER_COMPILE_FAILED entry=%s hr=0x%08lx detail=%s\n",
                     entrypoint, status,
                     errors ? static_cast<const char*>(errors->GetBufferPointer()) : "unknown");
        return false;
    }
    return SUCCEEDED(device->CreateComputeShader(
        bytecode->GetBufferPointer(), bytecode->GetBufferSize(), nullptr, &shader));
}

bool create_constant_buffer(ID3D11Device* device, const ShaderConstants& constants,
                            ComPtr<ID3D11Buffer>& buffer)
{
    D3D11_BUFFER_DESC desc{};
    desc.ByteWidth = sizeof(constants);
    desc.Usage = D3D11_USAGE_IMMUTABLE;
    desc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
    D3D11_SUBRESOURCE_DATA data{};
    data.pSysMem = &constants;
    return SUCCEEDED(device->CreateBuffer(&desc, &data, &buffer));
}

bool wait_for_d3d11(ID3D11DeviceContext* context, ID3D11Query* query)
{
    context->End(query);
    BOOL complete = FALSE;
    for (;;) {
        const HRESULT status = context->GetData(query, &complete, sizeof(complete), 0);
        if (status == S_OK && complete)
            return true;
        if (FAILED(status))
            return false;
        SwitchToThread();
    }
}

double percentile(std::vector<double> values, double fraction)
{
    if (values.empty())
        return 0.0;
    std::sort(values.begin(), values.end());
    const size_t index = static_cast<size_t>((values.size() - 1) * fraction + 0.5);
    return values[std::min(index, values.size() - 1)];
}

} // namespace

int wmain(int argc, wchar_t** argv)
{
    if (argc != 8) {
        std::fprintf(stderr,
                     "usage: rife_d3d11_trt_probe.exe <engine> <cudart> <hlsl> "
                     "<width> <height> <warmup> <iterations>\n");
        return 2;
    }
    const int source_width = _wtoi(argv[4]);
    const int source_height = _wtoi(argv[5]);
    const int warmup = _wtoi(argv[6]);
    const int iterations = _wtoi(argv[7]);
    if (source_width < 1 || source_height < 1 || warmup < 1 || iterations < 1) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_ARGUMENT_INVALID\n");
        return 2;
    }

    CudaApi cuda;
    if (!load_cuda(argv[2], cuda))
        return 3;

    ComPtr<IDXGIAdapter1> adapter;
    std::wstring adapter_name;
    if (!find_nvidia_adapter(adapter, adapter_name)) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_NVIDIA_ADAPTER_MISSING\n");
        return 4;
    }
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> d3d_context;
    D3D_FEATURE_LEVEL feature_level{};
    const D3D_FEATURE_LEVEL requested[] = {D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0};
    const HRESULT create_status = D3D11CreateDevice(
        adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, requested,
        static_cast<UINT>(std::size(requested)), D3D11_SDK_VERSION,
        &device, &feature_level, &d3d_context);
    if (FAILED(create_status)) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_DEVICE_FAILED hr=0x%08lx\n", create_status);
        return 5;
    }
    if (!cuda_ok(cuda, cuda.d3d11_set_device(device.Get(), -1), "cudaD3D11SetDirect3DDevice"))
        return 6;

    const std::vector<char> engine_data = read_file(argv[1]);
    if (engine_data.empty()) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_ENGINE_READ_FAILED\n");
        return 7;
    }
    Logger logger;
    TrtPtr<nvinfer1::IRuntime> runtime(nvinfer1::createInferRuntime(logger));
    if (!runtime) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_RUNTIME_FAILED\n");
        return 8;
    }
    TrtPtr<nvinfer1::ICudaEngine> engine(
        runtime->deserializeCudaEngine(engine_data.data(), engine_data.size()));
    if (!engine) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_DESERIALIZE_FAILED\n");
        return 9;
    }
    TrtPtr<nvinfer1::IExecutionContext> execution(engine->createExecutionContext());
    if (!execution) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_CONTEXT_FAILED\n");
        return 10;
    }

    const char* input_name = nullptr;
    const char* output_name = nullptr;
    for (int index = 0; index < engine->getNbIOTensors(); ++index) {
        const char* name = engine->getIOTensorName(index);
        if (!name)
            continue;
        if (engine->getTensorIOMode(name) == nvinfer1::TensorIOMode::kINPUT)
            input_name = name;
        else
            output_name = name;
    }
    if (!input_name || !output_name || engine->getNbIOTensors() != 2) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_IO_LAYOUT_UNSUPPORTED count=%d\n",
                     engine->getNbIOTensors());
        return 11;
    }
    const nvinfer1::Dims input_dims = engine->getTensorShape(input_name);
    const nvinfer1::Dims output_dims = engine->getTensorShape(output_name);
    size_t input_bytes = 0;
    size_t output_bytes = 0;
    if (input_dims.nbDims != 4 || output_dims.nbDims != 4
        || input_dims.d[0] != 1 || input_dims.d[1] != 11
        || output_dims.d[0] != 1 || output_dims.d[1] != 3
        || input_dims.d[2] != output_dims.d[2]
        || input_dims.d[3] != output_dims.d[3]
        || source_width > input_dims.d[3] || source_height > input_dims.d[2]
        || engine->getTensorDataType(input_name) != nvinfer1::DataType::kHALF
        || engine->getTensorDataType(output_name) != nvinfer1::DataType::kHALF
        || !tensor_size(input_dims, engine->getTensorDataType(input_name),
                     input_bytes)
        || !tensor_size(output_dims, engine->getTensorDataType(output_name),
                        output_bytes)) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_IO_SHAPE_INVALID\n");
        return 12;
    }

    ComPtr<ID3D11Buffer> input_buffer;
    ComPtr<ID3D11Buffer> output_buffer;
    if (!create_raw_buffer(device.Get(), input_bytes, input_buffer)
        || !create_raw_buffer(device.Get(), output_bytes, output_buffer)) {
        std::fprintf(stderr,
                     "RIFE_D3D11_TRT_BUFFER_FAILED input_bytes=%zu output_bytes=%zu\n",
                     input_bytes, output_bytes);
        return 13;
    }

    const uint32_t padded_width = static_cast<uint32_t>(input_dims.d[3]);
    const uint32_t padded_height = static_cast<uint32_t>(input_dims.d[2]);
    ComPtr<ID3D11Device3> device3;
    ComPtr<ID3D11Texture2D> input_texture0;
    ComPtr<ID3D11Texture2D> input_texture1;
    ComPtr<ID3D11Texture2D> output_texture;
    ComPtr<ID3D11ShaderResourceView1> input_y0;
    ComPtr<ID3D11ShaderResourceView1> input_y1;
    ComPtr<ID3D11ShaderResourceView1> input_uv0;
    ComPtr<ID3D11ShaderResourceView1> input_uv1;
    ComPtr<ID3D11UnorderedAccessView1> output_y;
    ComPtr<ID3D11UnorderedAccessView1> output_uv;
    ComPtr<ID3D11UnorderedAccessView> input_tensor_uav;
    ComPtr<ID3D11ShaderResourceView> output_tensor_srv;
    ComPtr<ID3D11ComputeShader> prepare_shader;
    ComPtr<ID3D11ComputeShader> output_shader;
    ComPtr<ID3D11Buffer> constants_buffer;
    ComPtr<ID3D11Query> completion_query;
    const ShaderConstants shader_constants{
        static_cast<uint32_t>(source_width),
        static_cast<uint32_t>(source_height),
        padded_width,
        padded_height,
        padded_width * padded_height,
        padded_width * padded_height,
        1,
        1,
    };
    D3D11_QUERY_DESC query_desc{};
    query_desc.Query = D3D11_QUERY_EVENT;
    if (FAILED(device.As(&device3))
        || !create_p010_texture(device.Get(), source_width, source_height,
                               D3D11_BIND_SHADER_RESOURCE, input_texture0)
        || !create_p010_texture(device.Get(), source_width, source_height,
                               D3D11_BIND_SHADER_RESOURCE, input_texture1)
        || !create_p010_texture(device.Get(), source_width, source_height,
                               D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS,
                               output_texture)
        || !initialize_p010_gradient(d3d_context.Get(), input_texture0.Get(),
                                     static_cast<uint32_t>(source_width),
                                     static_cast<uint32_t>(source_height), 0)
        || !initialize_p010_gradient(d3d_context.Get(), input_texture1.Get(),
                                     static_cast<uint32_t>(source_width),
                                     static_cast<uint32_t>(source_height), 64)
        || !create_p010_input_views(device3.Get(), input_texture0.Get(), input_y0, input_uv0)
        || !create_p010_input_views(device3.Get(), input_texture1.Get(), input_y1, input_uv1)
        || !create_p010_output_views(device3.Get(), output_texture.Get(), output_y, output_uv)
        || !create_raw_uav(device.Get(), input_buffer.Get(), input_bytes, input_tensor_uav)
        || !create_raw_srv(device.Get(), output_buffer.Get(), output_bytes, output_tensor_srv)
        || !compile_compute_shader(device.Get(), argv[3], "prepare_input", prepare_shader)
        || !compile_compute_shader(device.Get(), argv[3], "write_output", output_shader)
        || !create_constant_buffer(device.Get(), shader_constants, constants_buffer)
        || FAILED(device->CreateQuery(&query_desc, &completion_query))) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_SHADER_RESOURCES_FAILED\n");
        return 14;
    }

    CudaGraphicsResource input_resource = nullptr;
    CudaGraphicsResource output_resource = nullptr;
    if (!cuda_ok(cuda, cuda.register_d3d11_resource(
                           &input_resource, input_buffer.Get(), 0),
                 "cudaGraphicsD3D11RegisterResource(input)")
        || !cuda_ok(cuda, cuda.register_d3d11_resource(
                           &output_resource, output_buffer.Get(), 0),
                    "cudaGraphicsD3D11RegisterResource(output)")) {
        if (input_resource)
            cuda.unregister_resource(input_resource);
        return 15;
    }
    CudaStream stream = nullptr;
    if (!cuda_ok(cuda, cuda.stream_create(&stream, kCudaStreamNonBlocking),
                 "cudaStreamCreateWithFlags")) {
        cuda.unregister_resource(output_resource);
        cuda.unregister_resource(input_resource);
        return 16;
    }

    CudaGraphicsResource resources[] = {input_resource, output_resource};
    auto run_once = [&]() -> bool {
        ID3D11ShaderResourceView* input_srvs[] = {
            input_y0.Get(), input_y1.Get(), input_uv0.Get(), input_uv1.Get(),
        };
        ID3D11UnorderedAccessView* input_uavs[] = {input_tensor_uav.Get()};
        ID3D11Buffer* constants[] = {constants_buffer.Get()};
        d3d_context->CSSetShader(prepare_shader.Get(), nullptr, 0);
        d3d_context->CSSetConstantBuffers(0, 1, constants);
        d3d_context->CSSetShaderResources(0, static_cast<UINT>(std::size(input_srvs)), input_srvs);
        d3d_context->CSSetUnorderedAccessViews(0, 1, input_uavs, nullptr);
        d3d_context->Dispatch((padded_width / 2 + 7) / 8, (padded_height + 7) / 8, 1);
        ID3D11ShaderResourceView* null_input_srvs[4]{};
        ID3D11UnorderedAccessView* null_input_uavs[1]{};
        d3d_context->CSSetShaderResources(0, 4, null_input_srvs);
        d3d_context->CSSetUnorderedAccessViews(0, 1, null_input_uavs, nullptr);
        d3d_context->CSSetShader(nullptr, nullptr, 0);

        if (!cuda_ok(cuda, cuda.map_resources(2, resources, stream),
                     "cudaGraphicsMapResources"))
            return false;
        void* input_pointer = nullptr;
        void* output_pointer = nullptr;
        size_t mapped_input_bytes = 0;
        size_t mapped_output_bytes = 0;
        const bool mapped =
            cuda_ok(cuda, cuda.get_mapped_pointer(
                              &input_pointer, &mapped_input_bytes, input_resource),
                    "cudaGraphicsResourceGetMappedPointer(input)")
            && cuda_ok(cuda, cuda.get_mapped_pointer(
                                 &output_pointer, &mapped_output_bytes, output_resource),
                       "cudaGraphicsResourceGetMappedPointer(output)");
        if (!mapped || mapped_input_bytes < input_bytes || mapped_output_bytes < output_bytes
            || !execution->setTensorAddress(input_name, input_pointer)
            || !execution->setTensorAddress(output_name, output_pointer)
            || !execution->enqueueV3(stream)
            || !cuda_ok(cuda, cuda.stream_synchronize(stream), "cudaStreamSynchronize")) {
            cuda.unmap_resources(2, resources, stream);
            return false;
        }
        if (!cuda_ok(cuda, cuda.unmap_resources(2, resources, stream),
                     "cudaGraphicsUnmapResources"))
            return false;

        ID3D11ShaderResourceView* output_srvs[5]{};
        output_srvs[4] = output_tensor_srv.Get();
        ID3D11UnorderedAccessView* output_uavs[] = {output_y.Get(), output_uv.Get()};
        d3d_context->CSSetShader(output_shader.Get(), nullptr, 0);
        d3d_context->CSSetConstantBuffers(0, 1, constants);
        d3d_context->CSSetShaderResources(0, 5, output_srvs);
        d3d_context->CSSetUnorderedAccessViews(0, 2, output_uavs, nullptr);
        const UINT output_pair_width = (static_cast<UINT>(source_width) + 1) / 2;
        d3d_context->Dispatch((output_pair_width + 7) / 8,
                              (static_cast<UINT>(source_height) + 7) / 8, 1);
        ID3D11ShaderResourceView* null_output_srvs[5]{};
        ID3D11UnorderedAccessView* null_output_uavs[2]{};
        d3d_context->CSSetShaderResources(0, 5, null_output_srvs);
        d3d_context->CSSetUnorderedAccessViews(0, 2, null_output_uavs, nullptr);
        d3d_context->CSSetShader(nullptr, nullptr, 0);
        return wait_for_d3d11(d3d_context.Get(), completion_query.Get());
    };

    bool ok = true;
    for (int index = 0; index < warmup && ok; ++index)
        ok = run_once();
    std::vector<double> samples;
    samples.reserve(static_cast<size_t>(iterations));
    for (int index = 0; index < iterations && ok; ++index) {
        const auto started = std::chrono::steady_clock::now();
        ok = run_once();
        const auto ended = std::chrono::steady_clock::now();
        samples.push_back(std::chrono::duration<double, std::milli>(ended - started).count());
    }

    OutputValidation validation;
    if (ok && !validate_p010_output(
                  device.Get(), d3d_context.Get(), output_texture.Get(),
                  static_cast<uint32_t>(source_width),
                  static_cast<uint32_t>(source_height), validation)) {
        std::fprintf(stderr,
                     "RIFE_D3D11_TRT_P010_VALIDATION_FAILED unique_luma=%u "
                     "non_8bit_luma=%llu center_luma=%u\n",
                     validation.unique_luma_codes,
                     static_cast<unsigned long long>(validation.non_8bit_luma_samples),
                     validation.center_luma_code);
        ok = false;
    }

    cuda.stream_destroy(stream);
    cuda.unregister_resource(output_resource);
    cuda.unregister_resource(input_resource);
    if (!ok || samples.size() != static_cast<size_t>(iterations)) {
        std::fprintf(stderr, "RIFE_D3D11_TRT_INFERENCE_FAILED\n");
        return 17;
    }
    double total = 0.0;
    for (double sample : samples)
        total += sample;
    const double mean = total / samples.size();
    const double p95 = percentile(samples, 0.95);
    const double throughput = 1000.0 / mean;
    std::printf(
        "RIFE_D3D11_TRT_PROBE_OK adapter=%ls feature_level=0x%x source=%dx%d "
        "tensor=%ux%u input_bytes=%zu output_bytes=%zu unique_luma=%u "
        "non_8bit_luma=%llu center_luma=%u warmup=%d iterations=%d "
        "throughput=%.2fqps mean=%.2fms "
        "p95=%.2fms\n",
        adapter_name.c_str(), static_cast<unsigned int>(feature_level),
        source_width, source_height, padded_width, padded_height, input_bytes,
        output_bytes, validation.unique_luma_codes,
        static_cast<unsigned long long>(validation.non_8bit_luma_samples),
        validation.center_luma_code, warmup, iterations, throughput, mean, p95);
    return 0;
}
