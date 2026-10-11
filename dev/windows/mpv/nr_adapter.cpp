// Independent Feature 18 adapter. The runtime requires the caller's actual
// module name nvngx.dll; build this source with that name, without API hooks.
#define NVVIDEO_BUILD
#define NOMINMAX
#include "video_enhancement.h"
#include <nvsdk_ngx.h>
#include <d3d12.h>
#include <d3d11_4.h>
#include <d3dcompiler.h>
#include <dxgi1_2.h>
#include <wrl/client.h>
#include <cstdio>
#include <stdexcept>
#include <memory>
#include <string>
#include <algorithm>
using Microsoft::WRL::ComPtr;
namespace {
void hr(HRESULT r, const char *stage) {
    if (FAILED(r)) {
        char b[160];
        sprintf_s(b, "DLSSNR %s failed: 0x%08lx", stage, static_cast<unsigned long>(r));
        throw std::runtime_error(b);
    }
}
void ngx(NVSDK_NGX_Result r, const char *stage) {
    if (!NVSDK_NGX_SUCCEED(r)) {
        char b[160];
        sprintf_s(b, "DLSSNR %s failed: 0x%x", stage, r);
        throw std::runtime_error(b);
    }
}
NVSDK_NGX_Result NVSDK_CONV scaling(NVSDK_NGX_Parameter *p) {
    p->Set("DLSSNR.ScalingRatio", 1.f);
    return NVSDK_NGX_Result_Success;
}
using Init = NVSDK_NGX_Result(NVSDK_CONV *)(unsigned long long, const wchar_t *, ID3D12Device *,
                                            NVSDK_NGX_Version, const NVSDK_NGX_Parameter *);
using Create = NVSDK_NGX_Result(NVSDK_CONV *)(ID3D12GraphicsCommandList *, NVSDK_NGX_Feature,
                                              NVSDK_NGX_Parameter *, NVSDK_NGX_Handle **);
using Eval = NVSDK_NGX_Result(NVSDK_CONV *)(ID3D12GraphicsCommandList *, const NVSDK_NGX_Handle *,
                                            const NVSDK_NGX_Parameter *, PFN_NVSDK_NGX_ProgressCallback);
using Release = NVSDK_NGX_Result(NVSDK_CONV *)(NVSDK_NGX_Handle *);
using Shutdown = NVSDK_NGX_Result(NVSDK_CONV *)(ID3D12Device *);
template <class T> T symbol(HMODULE m, const char *name) {
    auto p = reinterpret_cast<T>(GetProcAddress(m, name));
    if (!p)
        throw std::runtime_error(name);
    return p;
}
template <class F, class... Args>
__declspec(noinline) NVSDK_NGX_Result provider_call(F function, Args... args) {
    __try {
        return function(args...);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        fprintf(stderr, "DLSSNR provider exception: 0x%08lx\n", GetExceptionCode());
        return NVSDK_NGX_Result_FAIL_PlatformError;
    }
}
void cleanup(NVSDK_NGX_Result r, const char *s) {
    if (!NVSDK_NGX_SUCCEED(r))
        fprintf(stderr, "DLSSNR cleanup %s: 0x%x\n", s, r);
}
} // namespace
struct nr_session {
    nvvideo_config c{};
    HMODULE runtime = nullptr;
    bool core = false, initialized = false, history = false;
    Init init = nullptr;
    Create create = nullptr;
    Eval evaluate = nullptr;
    Release release = nullptr;
    Shutdown shutdown = nullptr;
    NVSDK_NGX_Parameter *parameters = nullptr;
    NVSDK_NGX_Handle *feature = nullptr;
    ComPtr<ID3D12Device> device;
    ComPtr<ID3D12CommandQueue> queue;
    ComPtr<ID3D12CommandAllocator> allocator;
    ComPtr<ID3D12GraphicsCommandList> list;
    ComPtr<ID3D12Fence> fence;
    ComPtr<ID3D11Fence> fence11;
    ComPtr<ID3D11Device1> device11;
    ComPtr<ID3D11DeviceContext4> context;
    ComPtr<ID3D12Resource> color, output, motion, depth;
    ComPtr<ID3D11Texture2D> color11, output11;
    ComPtr<ID3D11UnorderedAccessView> colorUav;
    ComPtr<ID3D11ShaderResourceView> outputSrv, colorSrv;
    ComPtr<ID3D11ComputeShader> copyShader, compositeShader;
    ComPtr<ID3D11SamplerState> sampler;
    HANDLE event = nullptr;
    UINT64 sequence = 0;
    uint32_t width = 0, height = 0;
    ~nr_session() {
        if (context && fence11 && queue && fence) {
            auto r = context->Signal(fence11.Get(), ++sequence);
            context->Flush();
            if (SUCCEEDED(r))
                r = queue->Wait(fence.Get(), sequence);
            if (SUCCEEDED(r))
                r = queue->Signal(fence.Get(), ++sequence);
            if (SUCCEEDED(r)) {
                try {
                    wait();
                } catch (const std::exception &e) {
                    fprintf(stderr, "DLSSNR cleanup drain: %s\n", e.what());
                }
            } else
                fprintf(stderr, "DLSSNR cleanup drain: 0x%08lx\n", static_cast<unsigned long>(r));
        }
        if (feature && release)
            cleanup(provider_call(release, feature), "release");
        if (parameters)
            cleanup(NVSDK_NGX_D3D12_DestroyParameters(parameters), "parameters");
        if (initialized && shutdown)
            cleanup(provider_call(shutdown, device.Get()), "shutdown");
        if (core)
            cleanup(NVSDK_NGX_D3D12_Shutdown1(device.Get()), "core shutdown");
        if (runtime)
            FreeLibrary(runtime);
        if (event)
            CloseHandle(event);
    }
    void wait() {
        if (fence->GetCompletedValue() < sequence) {
            hr(fence->SetEventOnCompletion(sequence, event), "fence event");
            if (WaitForSingleObject(event, 30000) != WAIT_OBJECT_0)
                throw std::runtime_error("DLSSNR GPU fence timed out");
        }
        hr(device->GetDeviceRemovedReason(), "device removed");
    }
    void submit() {
        hr(list->Close(), "close list");
        ID3D12CommandList *lists[] = {list.Get()};
        queue->ExecuteCommandLists(1, lists);
        hr(queue->Signal(fence.Get(), ++sequence), "queue signal");
        wait();
    }
    void begin() {
        hr(allocator->Reset(), "allocator reset");
        hr(list->Reset(allocator.Get(), nullptr), "list reset");
    }
    ComPtr<ID3D12Resource> texture(DXGI_FORMAT format, bool shared) {
        D3D12_HEAP_PROPERTIES heap{};
        heap.Type = D3D12_HEAP_TYPE_DEFAULT;
        heap.CreationNodeMask = heap.VisibleNodeMask = 1;
        D3D12_RESOURCE_DESC d{};
        d.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
        d.Width = width;
        d.Height = height;
        d.DepthOrArraySize = d.MipLevels = 1;
        d.Format = format;
        d.SampleDesc.Count = 1;
        d.Flags = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
        if (shared)
            d.Flags |= D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS;
        ComPtr<ID3D12Resource> r;
        hr(device->CreateCommittedResource(&heap, shared ? D3D12_HEAP_FLAG_SHARED : D3D12_HEAP_FLAG_NONE, &d,
                                           D3D12_RESOURCE_STATE_COMMON, nullptr, IID_PPV_ARGS(&r)),
           "texture");
        return r;
    }
    void shared_texture(ComPtr<ID3D11Texture2D> &t, ComPtr<ID3D12Resource> &r) {
        D3D11_TEXTURE2D_DESC d{};
        d.Width = width;
        d.Height = height;
        d.MipLevels = d.ArraySize = 1;
        d.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        d.SampleDesc.Count = 1;
        d.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_UNORDERED_ACCESS;
        d.MiscFlags = D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED;
        hr(c.device->CreateTexture2D(&d, nullptr, &t), "shared D3D11 texture");
        ComPtr<IDXGIResource1> dxgi;
        hr(t.As(&dxgi), "shared DXGI resource");
        HANDLE h = nullptr;
        hr(dxgi->CreateSharedHandle(nullptr, DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE, nullptr,
                                    &h),
           "D3D11 texture handle");
        auto result = device->OpenSharedHandle(h, IID_PPV_ARGS(&r));
        CloseHandle(h);
        hr(result, "open D3D12 texture");
    }
    void barrier(ID3D12Resource *r, D3D12_RESOURCE_STATES from, D3D12_RESOURCE_STATES to) {
        D3D12_RESOURCE_BARRIER b{};
        b.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
        b.Transition.pResource = r;
        b.Transition.StateBefore = from;
        b.Transition.StateAfter = to;
        b.Transition.Subresource = D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES;
        list->ResourceBarrier(1, &b);
    }
    void initialize() {
        width = c.width;
        height = c.height;
        if (c.nr_target) {
            const double scale =
                std::min({1.0, c.nr_target * 16.0 / 9.0 / c.width, double(c.nr_target) / c.height});
            if (scale < 1.0) {
                width = std::max(2u, static_cast<uint32_t>(c.width * scale) & ~1u);
                height = std::max(2u, static_cast<uint32_t>(c.height * scale) & ~1u);
            }
        }
        ComPtr<IDXGIDevice> dxgi;
        ComPtr<IDXGIAdapter> adapter;
        hr(c.device->QueryInterface(IID_PPV_ARGS(&dxgi)), "DXGI device");
        hr(dxgi->GetAdapter(&adapter), "adapter");
        hr(D3D12CreateDevice(adapter.Get(), D3D_FEATURE_LEVEL_12_0, IID_PPV_ARGS(&device)), "device");
        hr(c.device->QueryInterface(IID_PPV_ARGS(&device11)), "D3D11 device1");
        hr(c.context->QueryInterface(IID_PPV_ARGS(&context)), "D3D11 context4");
        D3D12_COMMAND_QUEUE_DESC q{};
        hr(device->CreateCommandQueue(&q, IID_PPV_ARGS(&queue)), "queue");
        hr(device->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT, IID_PPV_ARGS(&allocator)),
           "allocator");
        hr(device->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, allocator.Get(), nullptr,
                                     IID_PPV_ARGS(&list)),
           "list");
        hr(device->CreateFence(0, D3D12_FENCE_FLAG_SHARED, IID_PPV_ARGS(&fence)), "fence");
        HANDLE h = nullptr;
        hr(device->CreateSharedHandle(fence.Get(), nullptr, GENERIC_ALL, nullptr, &h), "fence handle");
        ComPtr<ID3D11Device5> d5;
        hr(c.device->QueryInterface(IID_PPV_ARGS(&d5)), "D3D11 device5");
        auto result = d5->OpenSharedFence(h, IID_PPV_ARGS(&fence11));
        CloseHandle(h);
        hr(result, "open fence");
        event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
        if (!event)
            throw std::runtime_error("DLSSNR fence event creation failed");
        const wchar_t *paths[] = {c.runtime_directory};
        NVSDK_NGX_FeatureCommonInfo common{};
        common.PathListInfo.Path = paths;
        common.PathListInfo.Length = 1;
        ngx(NVSDK_NGX_D3D12_Init(0, c.cache_directory, device.Get(), &common), "core init");
        core = true;
        ngx(NVSDK_NGX_D3D12_AllocateParameters(&parameters), "allocate parameters");
        auto path = std::wstring(c.runtime_directory) + L"\\nvngx_dlssnr.dll";
        runtime = LoadLibraryExW(path.c_str(), nullptr,
                                 LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
        if (!runtime)
            throw std::runtime_error("DLSSNR runtime load failed");
        init = symbol<Init>(runtime, "NVSDK_NGX_D3D12_Init_Ext");
        create = symbol<Create>(runtime, "NVSDK_NGX_D3D12_CreateFeature");
        evaluate = symbol<Eval>(runtime, "NVSDK_NGX_D3D12_EvaluateFeature");
        release = symbol<Release>(runtime, "NVSDK_NGX_D3D12_ReleaseFeature");
        shutdown = symbol<Shutdown>(runtime, "NVSDK_NGX_D3D12_Shutdown1");
        ngx(provider_call(init, 0x0876232Cull, c.cache_directory, device.Get(), NVSDK_NGX_Version_API,
                          nullptr),
            "Init_Ext");
        initialized = true;
        for (const char *key :
             {"DLSSNR.Width", "DLSSNR.InputWidth", "DLSSNR.OutputWidth", "DLSSNR.Output.Width", "Width"})
            parameters->Set(key, width);
        for (const char *key :
             {"DLSSNR.Height", "DLSSNR.InputHeight", "DLSSNR.OutputHeight", "DLSSNR.Output.Height", "Height"})
            parameters->Set(key, height);
        parameters->Set("DLSSNR.Upscaling", 0u);
        parameters->Set("DLSSNR.Scale", 1.f);
        parameters->Set("DLSSNR.ScalingRatio", 1.f);
        parameters->Set("DLSSNRComputeScalingRatioCallback", reinterpret_cast<void *>(&scaling));
        parameters->Set("DLSSNR.Hint.Render.Preset", 0);
        parameters->Set("PerfQualityValue", 1);
        parameters->Set("CreationNodeMask", 1u);
        parameters->Set("VisibilityNodeMask", 1u);
        shared_texture(color11, color);
        shared_texture(output11, output);
        motion = texture(DXGI_FORMAT_R16G16_FLOAT, false);
        depth = texture(DXGI_FORMAT_R32_FLOAT, false);
        ngx(provider_call(create, list.Get(), static_cast<NVSDK_NGX_Feature>(18), parameters, &feature),
            "CreateFeature");
        if (!feature)
            throw std::runtime_error("DLSSNR created a null feature");
        submit();
        hr(c.device->CreateUnorderedAccessView(color11.Get(), nullptr, &colorUav), "input UAV");
        hr(c.device->CreateShaderResourceView(output11.Get(), nullptr, &outputSrv), "output SRV");
        hr(c.device->CreateShaderResourceView(color11.Get(), nullptr, &colorSrv), "proxy SRV");
        D3D11_SAMPLER_DESC sd{};
        sd.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
        sd.AddressU = sd.AddressV = sd.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
        sd.MaxLOD = D3D11_FLOAT32_MAX;
        hr(c.device->CreateSamplerState(&sd, &sampler), "resize sampler");
        const char shader[] =
            "Texture2D<float4> s:register(t0); SamplerState linearClamp:register(s0); RWTexture2D<float4> "
            "d:register(u0); [numthreads(8,8,1)] void main(uint3 p:SV_DispatchThreadID){uint "
            "w,h;d.GetDimensions(w,h);if(p.x<w&&p.y<h)d[p.xy]=s.SampleLevel(linearClamp,(float2(p.xy)+0.5)/"
            "float2(w,h),0);}";
        ComPtr<ID3DBlob> code, errors;
        hr(D3DCompile(shader, sizeof(shader) - 1, nullptr, nullptr, nullptr, "main", "cs_5_0",
                      D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &code, &errors),
           "copy shader compile");
        hr(c.device->CreateComputeShader(code->GetBufferPointer(), code->GetBufferSize(), nullptr,
                                         &copyShader),
           "copy shader");
        const char composite[] =
            "Texture2D<float4> neural:register(t0),proxy:register(t1),source:register(t2); SamplerState "
            "linearClamp:register(s0); RWTexture2D<float4> d:register(u0); [numthreads(8,8,1)] void "
            "main(uint3 p:SV_DispatchThreadID){uint w,h;d.GetDimensions(w,h);if(p.x>=w||p.y>=h)return;float2 "
            "uv=(float2(p.xy)+0.5)/float2(w,h);float4 s=source.Load(int3(p.xy,0));float3 "
            "delta=neural.SampleLevel(linearClamp,uv,0).rgb-proxy.SampleLevel(linearClamp,uv,0).rgb;d[p.xy]="
            "float4(saturate(s.rgb+delta),s.a);}";
        code.Reset();
        errors.Reset();
        hr(D3DCompile(composite, sizeof(composite) - 1, nullptr, nullptr, nullptr, "main", "cs_5_0",
                      D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &code, &errors),
           "residual shader compile");
        hr(c.device->CreateComputeShader(code->GetBufferPointer(), code->GetBufferSize(), nullptr,
                                         &compositeShader),
           "residual shader");
        // Deterministic zero motion/depth, uploaded once. No frame readback.
        begin();
        D3D12_DESCRIPTOR_HEAP_DESC hd{};
        hd.Type = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV;
        hd.NumDescriptors = 2;
        hd.Flags = D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE;
        ComPtr<ID3D12DescriptorHeap> heap;
        hr(device->CreateDescriptorHeap(&hd, IID_PPV_ARGS(&heap)), "zero descriptor heap");
        ID3D12DescriptorHeap *heaps[] = {heap.Get()};
        list->SetDescriptorHeaps(1, heaps);
        auto cpu = heap->GetCPUDescriptorHandleForHeapStart();
        auto gpu = heap->GetGPUDescriptorHandleForHeapStart();
        auto stride = device->GetDescriptorHandleIncrementSize(hd.Type);
        const float zero[4] = {};
        for (auto *resource : {motion.Get(), depth.Get()}) {
            D3D12_UNORDERED_ACCESS_VIEW_DESC vd{};
            vd.ViewDimension = D3D12_UAV_DIMENSION_TEXTURE2D;
            vd.Format = resource->GetDesc().Format;
            device->CreateUnorderedAccessView(resource, nullptr, &vd, cpu);
            barrier(resource, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
            list->ClearUnorderedAccessViewFloat(gpu, cpu, resource, zero, 0, nullptr);
            barrier(resource, D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                    D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
            cpu.ptr += stride;
            gpu.ptr += stride;
        }
        submit();
        fprintf(stderr, "DLSSNR_GPU_INIT_OK source=%ux%u internal=%ux%u style=%d intensity=%.3f\n",
                c.width, c.height, width, height, c.nr_style, c.nr_intensity);
    }
    void copy(ID3D11ShaderResourceView *src, ID3D11UnorderedAccessView *dst, bool composite = false) {
        ID3D11ShaderResourceView *srvs[] = {composite ? outputSrv.Get() : src,
                                            composite ? colorSrv.Get() : nullptr, composite ? src : nullptr};
        ID3D11ShaderResourceView *nullSrvs[3] = {};
        ID3D11UnorderedAccessView *nullUav = nullptr;
        ID3D11SamplerState *samplers[] = {sampler.Get()};
        context->CSSetSamplers(0, 1, samplers);
        context->CSSetShader(composite ? compositeShader.Get() : copyShader.Get(), nullptr, 0);
        context->CSSetShaderResources(0, 3, srvs);
        context->CSSetUnorderedAccessViews(0, 1, &dst, nullptr);
        context->Dispatch(((composite ? c.width : width) + 7) / 8, ((composite ? c.height : height) + 7) / 8,
                          1);
        context->CSSetShaderResources(0, 3, nullSrvs);
        context->CSSetUnorderedAccessViews(0, 1, &nullUav, nullptr);
    }
    void process(ID3D11Texture2D *input, ID3D11Texture2D *destination) {
        ComPtr<ID3D11ShaderResourceView> src;
        ComPtr<ID3D11UnorderedAccessView> dst;
        hr(c.device->CreateShaderResourceView(input, nullptr, &src), "frame SRV");
        hr(c.device->CreateUnorderedAccessView(destination, nullptr, &dst), "frame UAV");
        copy(src.Get(), colorUav.Get());
        hr(context->Signal(fence11.Get(), ++sequence), "producer signal");
        context->Flush();
        hr(queue->Wait(fence.Get(), sequence), "producer wait");
        begin();
        barrier(color.Get(), D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE);
        barrier(output.Get(), D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_UNORDERED_ACCESS);
        parameters->Set("DLSSNR.Color", color.Get());
        parameters->Set("DLSSNR.Output", output.Get());
        parameters->Set("DLSSNR.MVec", motion.Get());
        parameters->Set("DLSSNR.Depth", depth.Get());
        for (const char *prefix : {"Color", "Output", "MVec", "Depth"}) {
            auto base = std::string("DLSSNR.") + prefix;
            parameters->Set((base + "SubrectBaseX").c_str(), 0u);
            parameters->Set((base + "SubrectBaseY").c_str(), 0u);
            parameters->Set((base + "SubrectWidth").c_str(), width);
            parameters->Set((base + "SubrectHeight").c_str(), height);
        }
        parameters->Set("DLSSNR.MVecScaleX", 1.f);
        parameters->Set("DLSSNR.MVecScaleY", 1.f);
        parameters->Set("DLSSNR.DepthInverted", 1);
        parameters->Set("DLSS.Indicator.Invert.X.Axis", 0);
        parameters->Set("DLSS.Indicator.Invert.Y.Axis", 0);
        parameters->Set("DLSSNR.Enabled", 1);
        parameters->Set("DLSSNR.Reset", history ? 0 : 1);
        parameters->Set("DLSSNR.Style", c.nr_style);
        parameters->Set("DLSSNR.Intensity", c.nr_intensity);
        parameters->Set("DLSSNR.LocalToneStrength", c.nr_tone);
        parameters->Set("DLSSNR.LocalStructureStrength", c.nr_structure);
        parameters->Set("DLSSNR.SkinStructureStrength", c.nr_skin);
        parameters->Set("DLSSNR.UseAutoMask", c.nr_auto_mask);
        parameters->Set("DLSSNR.UICorrection", c.nr_ui_correction);
        // Runtime evaluation owns its descriptor state and starts on a fresh list.
        submit();
        begin();
        ngx(provider_call(evaluate, list.Get(), feature, parameters, nullptr), "EvaluateFeature");
        barrier(color.Get(), D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE, D3D12_RESOURCE_STATE_COMMON);
        barrier(output.Get(), D3D12_RESOURCE_STATE_UNORDERED_ACCESS, D3D12_RESOURCE_STATE_COMMON);
        submit();
        hr(context->Wait(fence11.Get(), sequence), "consumer wait");
        copy(src.Get(), dst.Get(), true);
        context->Flush();
        history = true;
        hr(c.device->GetDeviceRemovedReason(), "D3D11 device removed");
    }
};
extern "C" __declspec(dllexport) nr_session *__cdecl nr_create(const nvvideo_config *c, char *error,
                                                               size_t size) {
    try {
        auto s = std::make_unique<nr_session>();
        s->c = *c;
        s->initialize();
        return s.release();
    } catch (const std::exception &e) {
        snprintf(error, size, "%s", e.what());
        return nullptr;
    }
}
extern "C" __declspec(dllexport) int __cdecl nr_process(nr_session *s, ID3D11Texture2D *input,
                                                        ID3D11Texture2D *output, char *error, size_t size) {
    try {
        s->process(input, output);
        return 1;
    } catch (const std::exception &e) {
        snprintf(error, size, "%s", e.what());
        return 0;
    }
}
extern "C" __declspec(dllexport) void __cdecl nr_destroy(nr_session *s) { delete s; }
