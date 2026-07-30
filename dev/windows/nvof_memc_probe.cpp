#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <d3d11.h>
#include <d3d11_3.h>
#include <dxgi1_2.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstring>
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
    ComPtr<ID3D11Texture2D> texture;
    NvOFGPUBufferHandle handle = nullptr;
};

struct OfSession {
    HMODULE module = nullptr;
    NV_OF_D3D11_API_FUNCTION_LIST api{};
    NvOFHandle handle = nullptr;
    std::vector<NvOFGPUBufferHandle> resources;

    void unregister_all() {
        for (auto resource : resources) {
            if (resource)
                api.nvOFUnregisterResourceD3D11(resource);
        }
        resources.clear();
    }

    ~OfSession() {
        unregister_all();
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
    std::fprintf(stderr,
                 "NVOF_MEMC_PROBE_ERROR operation=%s status=%u name=%s\n",
                 operation, static_cast<unsigned>(status), status_name(status));
    return false;
}

static bool check_hr(const char *operation, HRESULT result) {
    if (SUCCEEDED(result))
        return true;
    std::fprintf(stderr,
                 "NVOF_MEMC_PROBE_ERROR operation=%s hresult=0x%08lx\n",
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
            requested, static_cast<UINT>(std::size(requested)),
            D3D11_SDK_VERSION, device.put(), &actual, context.put());
        if (result == E_INVALIDARG) {
            result = D3D11CreateDevice(
                adapter.get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT |
                    D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                requested + 1, 1, D3D11_SDK_VERSION,
                device.put(), &actual, context.put());
        }
        if (!check_hr("D3D11CreateDevice", result))
            return false;

        adapter->AddRef();
        selected_adapter.reset(adapter.get());
        return true;
    }

    std::fprintf(stderr,
                 "NVOF_MEMC_PROBE_ERROR operation=select_adapter detail=no_nvidia_adapter\n");
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

template <typename T>
static bool contains(const std::vector<T> &values, T expected) {
    return std::find(values.begin(), values.end(), expected) != values.end();
}

static std::string join_values(const std::vector<uint32_t> &values) {
    std::string result;
    for (size_t index = 0; index < values.size(); ++index) {
        if (index)
            result += ',';
        result += std::to_string(values[index]);
    }
    return result.empty() ? "none" : result;
}

static std::string join_formats(const std::vector<DXGI_FORMAT> &values) {
    std::string result;
    for (size_t index = 0; index < values.size(); ++index) {
        if (index)
            result += ',';
        result += std::to_string(static_cast<unsigned>(values[index]));
    }
    return result.empty() ? "none" : result;
}

static bool create_texture(ID3D11Device *device, UINT width, UINT height,
                           DXGI_FORMAT format, UINT bind_flags,
                           ID3D11Texture2D **texture) {
    D3D11_TEXTURE2D_DESC description{};
    description.Width = width;
    description.Height = height;
    description.MipLevels = 1;
    description.ArraySize = 1;
    description.Format = format;
    description.SampleDesc.Count = 1;
    description.Usage = D3D11_USAGE_DEFAULT;
    description.BindFlags = bind_flags;
    return check_hr("CreateTexture2D",
                    device->CreateTexture2D(&description, nullptr, texture));
}

static bool create_readback_texture(ID3D11Device *device, UINT width,
                                    UINT height, DXGI_FORMAT format,
                                    ID3D11Texture2D **texture) {
    D3D11_TEXTURE2D_DESC description{};
    description.Width = width;
    description.Height = height;
    description.MipLevels = 1;
    description.ArraySize = 1;
    description.Format = format;
    description.SampleDesc.Count = 1;
    description.Usage = D3D11_USAGE_STAGING;
    description.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    return check_hr("CreateTexture2D(readback)",
                    device->CreateTexture2D(&description, nullptr, texture));
}

static bool sync_resource(ID3D11DeviceContext *context,
                          ID3D11Texture2D *readback,
                          ID3D11Texture2D *resource) {
    context->CopyResource(readback, resource);
    D3D11_MAPPED_SUBRESOURCE mapped{};
    if (!check_hr("Map(readback)",
                  context->Map(readback, 0, D3D11_MAP_READ, 0, &mapped)))
        return false;
    volatile uint8_t marker = *static_cast<const uint8_t *>(mapped.pData);
    (void)marker;
    context->Unmap(readback, 0);
    return true;
}

static bool register_resource(OfSession &session, OfResource &resource) {
    if (!check_status("nvOFRegisterResourceD3D11",
                      session.api.nvOFRegisterResourceD3D11(
                          session.handle, resource.texture.get(),
                          &resource.handle)))
        return false;
    session.resources.push_back(resource.handle);
    return true;
}

static bool test_p010_views(ID3D11Device *device) {
    ComPtr<ID3D11Device3> device3;
    if (!check_hr("QueryInterface(ID3D11Device3)",
                  device->QueryInterface(__uuidof(ID3D11Device3),
                                         reinterpret_cast<void **>(device3.put()))))
        return false;

    ComPtr<ID3D11Texture2D> texture;
    if (!create_texture(device, 3840, 2160, DXGI_FORMAT_P010,
                        D3D11_BIND_SHADER_RESOURCE |
                            D3D11_BIND_UNORDERED_ACCESS,
                        texture.put()))
        return false;

    D3D11_SHADER_RESOURCE_VIEW_DESC1 y_srv_desc{};
    y_srv_desc.Format = DXGI_FORMAT_R16_UNORM;
    y_srv_desc.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D;
    y_srv_desc.Texture2D.MipLevels = 1;
    y_srv_desc.Texture2D.PlaneSlice = 0;
    ComPtr<ID3D11ShaderResourceView1> y_srv;
    if (!check_hr("CreateShaderResourceView1(P010 Y)",
                  device3->CreateShaderResourceView1(
                      texture.get(), &y_srv_desc, y_srv.put())))
        return false;

    D3D11_SHADER_RESOURCE_VIEW_DESC1 uv_srv_desc = y_srv_desc;
    uv_srv_desc.Format = DXGI_FORMAT_R16G16_UNORM;
    uv_srv_desc.Texture2D.PlaneSlice = 1;
    ComPtr<ID3D11ShaderResourceView1> uv_srv;
    if (!check_hr("CreateShaderResourceView1(P010 UV)",
                  device3->CreateShaderResourceView1(
                      texture.get(), &uv_srv_desc, uv_srv.put())))
        return false;

    D3D11_UNORDERED_ACCESS_VIEW_DESC1 y_uav_desc{};
    y_uav_desc.Format = DXGI_FORMAT_R16_UNORM;
    y_uav_desc.ViewDimension = D3D11_UAV_DIMENSION_TEXTURE2D;
    y_uav_desc.Texture2D.PlaneSlice = 0;
    ComPtr<ID3D11UnorderedAccessView1> y_uav;
    if (!check_hr("CreateUnorderedAccessView1(P010 Y)",
                  device3->CreateUnorderedAccessView1(
                      texture.get(), &y_uav_desc, y_uav.put())))
        return false;

    D3D11_UNORDERED_ACCESS_VIEW_DESC1 uv_uav_desc = y_uav_desc;
    uv_uav_desc.Format = DXGI_FORMAT_R16G16_UNORM;
    uv_uav_desc.Texture2D.PlaneSlice = 1;
    ComPtr<ID3D11UnorderedAccessView1> uv_uav;
    if (!check_hr("CreateUnorderedAccessView1(P010 UV)",
                  device3->CreateUnorderedAccessView1(
                      texture.get(), &uv_uav_desc, uv_uav.put())))
        return false;

    std::printf("NVOF_MEMC_PROBE_P010_OK resolution=3840x2160 "
                "srv_y=R16_UNORM srv_uv=R16G16_UNORM "
                "uav_y=R16_UNORM uav_uv=R16G16_UNORM\n");
    return true;
}

static bool test_nvof_session(OfSession &session, ID3D11Device *device,
                              ID3D11DeviceContext *context,
                              bool use_nv12,
                              const char *profile_name,
                              bool bidirectional,
                              bool output_cost,
                              bool temporal_hints,
                              const std::vector<uint32_t> &grids,
                              const std::vector<uint32_t> &width_max,
                              const std::vector<uint32_t> &height_max,
                              const std::vector<DXGI_FORMAT> &input_formats,
                              const std::vector<DXGI_FORMAT> &output_formats,
                              const std::vector<DXGI_FORMAT> &cost_formats,
                              const std::vector<DXGI_FORMAT> &global_formats) {
    constexpr UINT width = 3840;
    constexpr UINT height = 2160;
    DXGI_FORMAT input_dxgi_format = use_nv12
        ? DXGI_FORMAT_NV12 : DXGI_FORMAT_R8_UNORM;
    NV_OF_BUFFER_FORMAT input_buffer_format = use_nv12
        ? NV_OF_BUFFER_FORMAT_NV12 : NV_OF_BUFFER_FORMAT_GRAYSCALE8;
    if (!contains(grids, uint32_t{1}) || width_max.empty() ||
        height_max.empty() || width_max[0] < width || height_max[0] < height ||
        !contains(input_formats, input_dxgi_format) ||
        !contains(output_formats, DXGI_FORMAT_R16G16_SINT) ||
        (output_cost && !contains(cost_formats, DXGI_FORMAT_R8_UINT))) {
        std::fprintf(stderr,
                     "NVOF_MEMC_PROBE_ERROR operation=required_capabilities "
                     "grid1=%d max=%ux%u input=%s input_supported=%d "
                     "flow=%d cost8=%d global=%d\n",
                     contains(grids, uint32_t{1}),
                     width_max.empty() ? 0 : width_max[0],
                     height_max.empty() ? 0 : height_max[0],
                     use_nv12 ? "nv12" : "gray8",
                     contains(input_formats, input_dxgi_format),
                     contains(output_formats, DXGI_FORMAT_R16G16_SINT),
                     contains(cost_formats, DXGI_FORMAT_R8_UINT),
                     contains(global_formats, DXGI_FORMAT_R16G16_SINT));
        return false;
    }

    NV_OF_INIT_PARAMS init{};
    init.width = width;
    init.height = height;
    init.outGridSize = NV_OF_OUTPUT_VECTOR_GRID_SIZE_1;
    init.hintGridSize = NV_OF_HINT_VECTOR_GRID_SIZE_UNDEFINED;
    init.mode = NV_OF_MODE_OPTICALFLOW;
    init.perfLevel = NV_OF_PERF_LEVEL_SLOW;
    init.enableExternalHints = NV_OF_FALSE;
    init.enableOutputCost = output_cost ? NV_OF_TRUE : NV_OF_FALSE;
    init.disparityRange = NV_OF_STEREO_DISPARITY_RANGE_UNDEFINED;
    init.enableRoi = NV_OF_FALSE;
    init.predDirection = bidirectional
        ? NV_OF_PRED_DIRECTION_BOTH : NV_OF_PRED_DIRECTION_FORWARD;
    init.enableGlobalFlow = NV_OF_FALSE;
    init.inputBufferFormat = input_buffer_format;
    if (!check_status("nvOFInit(profile)",
                      session.api.nvOFInit(session.handle, &init)))
        return false;

    constexpr size_t max_batch_size = 15;
    std::array<OfResource, max_batch_size + 1> inputs;
    std::array<OfResource, max_batch_size> forward;
    std::array<OfResource, max_batch_size> backward;
    std::array<OfResource, max_batch_size> cost_forward;
    std::array<OfResource, max_batch_size> cost_backward;
    constexpr UINT io_bind =
        D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
    for (auto &resource : inputs) {
        if (!create_texture(device, width, height, input_dxgi_format,
                            io_bind, resource.texture.put()) ||
            !register_resource(session, resource)) {
            session.unregister_all();
            return false;
        }
    }
    size_t luma_size = static_cast<size_t>(width) * height;
    std::vector<uint8_t> input_pixels(
        use_nv12 ? luma_size * 3 / 2 : luma_size);
    for (size_t frame = 0; frame < inputs.size(); ++frame) {
        for (UINT y = 0; y < height; ++y) {
            for (UINT x = 0; x < width; ++x) {
                UINT shifted_x = x + static_cast<UINT>(frame * 11);
                UINT checker = ((shifted_x / 24) ^ (y / 24)) & 1;
                UINT diagonal = (shifted_x * 3 + y * 5) & 0xff;
                input_pixels[static_cast<size_t>(y) * width + x] =
                    static_cast<uint8_t>((checker ? 96 : 24) +
                                         diagonal / 2);
            }
        }
        if (use_nv12) {
            for (UINT y = 0; y < height / 2; ++y) {
                for (UINT x = 0; x < width; x += 2) {
                    size_t offset = luma_size +
                        static_cast<size_t>(y) * width + x;
                    input_pixels[offset] = static_cast<uint8_t>(
                        96 + ((x / 32 + frame * 3) & 0x3f));
                    input_pixels[offset + 1] = static_cast<uint8_t>(
                        96 + ((y / 16 + frame * 5) & 0x3f));
                }
            }
        }
        context->UpdateSubresource(inputs[frame].texture.get(), 0, nullptr,
                                   input_pixels.data(), width, 0);
    }
    for (size_t index = 0; index < max_batch_size; ++index) {
        if (!create_texture(device, width, height,
                            DXGI_FORMAT_R16G16_SINT,
                            D3D11_BIND_SHADER_RESOURCE,
                            forward[index].texture.put()) ||
            !register_resource(session, forward[index])) {
            session.unregister_all();
            return false;
        }
        if (bidirectional) {
            if (!create_texture(device, width, height,
                                DXGI_FORMAT_R16G16_SINT,
                                D3D11_BIND_SHADER_RESOURCE,
                                backward[index].texture.put())) {
                session.unregister_all();
                return false;
            }
            if (!register_resource(session, backward[index])) {
                session.unregister_all();
                return false;
            }
        }
        if (output_cost) {
            if (!create_texture(device, width, height, DXGI_FORMAT_R8_UINT,
                                D3D11_BIND_SHADER_RESOURCE,
                                cost_forward[index].texture.put()) ||
                !register_resource(session, cost_forward[index])) {
                session.unregister_all();
                return false;
            }
            if (bidirectional) {
                OfResource *resource = &cost_backward[index];
                if (!create_texture(device, width, height,
                                    DXGI_FORMAT_R8_UINT,
                                    D3D11_BIND_SHADER_RESOURCE,
                                    resource->texture.put())) {
                    session.unregister_all();
                    return false;
                }
                if (!register_resource(session, *resource)) {
                    session.unregister_all();
                    return false;
                }
            }
        }
    }
    ComPtr<ID3D11Texture2D> input_readback;
    ComPtr<ID3D11Texture2D> flow_readback;
    if (!create_readback_texture(device, width, height, input_dxgi_format,
                                 input_readback.put()) ||
        !create_readback_texture(device, width, height,
                                 DXGI_FORMAT_R16G16_SINT,
                                 flow_readback.put()) ||
        !sync_resource(context, input_readback.get(),
                       inputs.back().texture.get())) {
        session.unregister_all();
        return false;
    }

    auto execute_batch = [&](size_t batch_size) {
        for (size_t index = 0; index < batch_size; ++index) {
            NV_OF_EXECUTE_INPUT_PARAMS execute_input{};
            execute_input.inputFrame = inputs[index].handle;
            execute_input.referenceFrame = inputs[index + 1].handle;
            execute_input.disableTemporalHints = temporal_hints
                ? NV_OF_FALSE : NV_OF_TRUE;
            NV_OF_EXECUTE_OUTPUT_PARAMS execute_output{};
            execute_output.outputBuffer = forward[index].handle;
            if (output_cost)
                execute_output.outputCostBuffer = cost_forward[index].handle;
            if (bidirectional) {
                execute_output.bwdOutputBuffer = backward[index].handle;
                if (output_cost) {
                    execute_output.bwdOutputCostBuffer =
                        cost_backward[index].handle;
                }
            }
            if (!check_status(
                    "nvOFExecute(profile_batch)",
                    session.api.nvOFExecute(session.handle, &execute_input,
                                            &execute_output)))
                return false;
        }
        ID3D11Texture2D *last_output = bidirectional
            ? backward[batch_size - 1].texture.get()
            : forward[batch_size - 1].texture.get();
        return sync_resource(context, flow_readback.get(), last_output);
    };

    constexpr int warmup_batches = 4;
    constexpr int measured_batches = 20;
    for (size_t batch_size :
         {size_t{1}, size_t{2}, size_t{4}, size_t{8}, size_t{15}}) {
        for (int iteration = 0; iteration < warmup_batches; ++iteration) {
            if (!execute_batch(batch_size)) {
                session.unregister_all();
                return false;
            }
        }
        auto started = std::chrono::steady_clock::now();
        for (int iteration = 0; iteration < measured_batches; ++iteration) {
            if (!execute_batch(batch_size)) {
                session.unregister_all();
                return false;
            }
        }
        double elapsed_ms = std::chrono::duration<double, std::milli>(
            std::chrono::steady_clock::now() - started).count();
        double pairs = static_cast<double>(batch_size * measured_batches);
        std::printf("NVOF_MEMC_PROBE_BATCH resolution=3840x2160 "
                    "grid=1 preset=slow profile=%s direction=%s cost=%s "
                    "temporal_hints=%s input=%s "
                    "batch=%zu pairs=%.0f elapsed_ms=%.3f "
                    "pair_fps=%.3f per_pair_ms=%.3f\n",
                    profile_name,
                    bidirectional ? "both" : "forward",
                    output_cost ? "uint8" : "off",
                    temporal_hints ? "on" : "off",
                    use_nv12 ? "nv12" : "gray8", batch_size, pairs, elapsed_ms,
                    pairs * 1000.0 / elapsed_ms, elapsed_ms / pairs);
    }

    session.unregister_all();

    std::printf("NVOF_MEMC_PROBE_NVOF_OK resolution=3840x2160 grid=1 "
                "preset=slow profile=%s direction=%s cost=%s "
                "temporal_hints=%s global_flow=no input=%s\n",
                profile_name,
                bidirectional ? "both" : "forward",
                output_cost ? "uint8" : "off",
                temporal_hints ? "on" : "off",
                use_nv12 ? "nv12" : "gray8");
    return true;
}

int main(int argc, char **argv) {
    std::setvbuf(stdout, nullptr, _IONBF, 0);
    std::setvbuf(stderr, nullptr, _IONBF, 0);
    if (argc != 3 ||
        (std::strcmp(argv[1], "--input=gray8") != 0 &&
         std::strcmp(argv[1], "--input=nv12") != 0) ||
        (std::strcmp(argv[2], "--profile=strict-memc") != 0 &&
         std::strcmp(argv[2], "--profile=official-sample") != 0 &&
         std::strcmp(argv[2], "--profile=forward-cost") != 0 &&
         std::strcmp(argv[2], "--profile=both-no-cost") != 0)) {
        std::fprintf(stderr,
                     "NVOF_MEMC_PROBE_ERROR operation=arguments "
                     "expected=--input=gray8|--input=nv12 "
                     "--profile=strict-memc|--profile=official-sample|"
                     "--profile=forward-cost|--profile=both-no-cost\n");
        return 2;
    }
    bool use_nv12 = std::strcmp(argv[1], "--input=nv12") == 0;
    bool official_sample_profile =
        std::strcmp(argv[2], "--profile=official-sample") == 0;
    bool forward_cost_profile =
        std::strcmp(argv[2], "--profile=forward-cost") == 0;
    bool both_no_cost_profile =
        std::strcmp(argv[2], "--profile=both-no-cost") == 0;
    const char *profile_name = official_sample_profile
        ? "official-sample"
        : forward_cost_profile
            ? "forward-cost"
            : both_no_cost_profile ? "both-no-cost" : "strict-memc";
    bool bidirectional = both_no_cost_profile ||
                         std::strcmp(profile_name, "strict-memc") == 0;
    bool output_cost = forward_cost_profile ||
                       std::strcmp(profile_name, "strict-memc") == 0;
    bool temporal_hints = official_sample_profile;
    std::fprintf(stderr, "NVOF_MEMC_PROBE_STAGE stage=create_device\n");
    ComPtr<IDXGIAdapter1> adapter;
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    std::string adapter_name;
    if (!create_nvidia_device(adapter, device, context, adapter_name))
        return 2;

    std::fprintf(stderr, "NVOF_MEMC_PROBE_STAGE stage=load_api\n");
    OfSession session;
    session.module = LoadLibraryW(L"nvofapi64.dll");
    if (!session.module) {
        std::fprintf(stderr,
                     "NVOF_MEMC_PROBE_ERROR operation=LoadLibrary "
                     "detail=win32_%lu\n",
                     static_cast<unsigned long>(GetLastError()));
        return 2;
    }
    auto get_max_version = reinterpret_cast<GetMaxVersionFn>(
        GetProcAddress(session.module, "NvOFGetMaxSupportedApiVersion"));
    auto create_instance = reinterpret_cast<CreateInstanceD3D11Fn>(
        GetProcAddress(session.module, "NvOFAPICreateInstanceD3D11"));
    if (!get_max_version || !create_instance) {
        std::fprintf(stderr,
                     "NVOF_MEMC_PROBE_ERROR operation=GetProcAddress "
                     "detail=entrypoint_missing\n");
        return 2;
    }

    uint32_t driver_api_version = 0;
    if (!check_status("NvOFGetMaxSupportedApiVersion",
                      get_max_version(&driver_api_version)) ||
        driver_api_version < NV_OF_API_VERSION ||
        !check_status("NvOFAPICreateInstanceD3D11",
                      create_instance(NV_OF_API_VERSION, &session.api)) ||
        !check_status("nvCreateOpticalFlowD3D11",
                      session.api.nvCreateOpticalFlowD3D11(
                          device.get(), context.get(), &session.handle)))
        return 2;

    std::fprintf(stderr, "NVOF_MEMC_PROBE_STAGE stage=query_caps\n");
    std::vector<uint32_t> grids;
    std::vector<uint32_t> width_max;
    std::vector<uint32_t> height_max;
    std::vector<DXGI_FORMAT> input_formats;
    std::vector<DXGI_FORMAT> output_formats;
    std::vector<DXGI_FORMAT> cost_formats;
    std::vector<DXGI_FORMAT> global_formats;
    if (!query_cap(session, NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES, grids) ||
        !query_cap(session, NV_OF_CAPS_WIDTH_MAX, width_max) ||
        !query_cap(session, NV_OF_CAPS_HEIGHT_MAX, height_max))
        return 2;
    std::fprintf(stderr, "NVOF_MEMC_PROBE_STAGE stage=query_input_formats\n");
    if (!query_formats(session, NV_OF_BUFFER_USAGE_INPUT, input_formats))
        return 2;
    std::fprintf(stderr, "NVOF_MEMC_PROBE_STAGE stage=query_output_formats\n");
    if (!query_formats(session, NV_OF_BUFFER_USAGE_OUTPUT, output_formats))
        return 2;
    std::fprintf(stderr, "NVOF_MEMC_PROBE_STAGE stage=query_cost_formats\n");
    if (!query_formats(session, NV_OF_BUFFER_USAGE_COST, cost_formats))
        return 2;
    std::fprintf(stderr, "NVOF_MEMC_PROBE_STAGE stage=query_global_formats\n");
    if (!query_formats(session, NV_OF_BUFFER_USAGE_GLOBAL_FLOW,
                       global_formats))
        return 2;

    std::printf("NVOF_MEMC_PROBE_CAPS adapter=\"%s\" api=%u.%u "
                "grids=%s max_width=%s max_height=%s input_formats=%s "
                "output_formats=%s cost_formats=%s global_formats=%s\n",
                adapter_name.c_str(), driver_api_version >> 4,
                driver_api_version & 0xf, join_values(grids).c_str(),
                join_values(width_max).c_str(), join_values(height_max).c_str(),
                join_formats(input_formats).c_str(),
                join_formats(output_formats).c_str(),
                join_formats(cost_formats).c_str(),
                join_formats(global_formats).c_str());

    std::fprintf(stderr, "NVOF_MEMC_PROBE_STAGE stage=test_p010_views\n");
    if (!test_p010_views(device.get()))
        return 3;
    std::fprintf(stderr, "NVOF_MEMC_PROBE_STAGE stage=test_nvof_session\n");
    if (
        !test_nvof_session(session, device.get(), context.get(), use_nv12,
                           profile_name, bidirectional, output_cost,
                           temporal_hints,
                           grids, width_max, height_max, input_formats,
                           output_formats, cost_formats, global_formats))
        return 3;

    std::printf("NVOF_MEMC_PROBE_OK quality_path=P010 profile=%s\n",
                profile_name);
    return 0;
}
