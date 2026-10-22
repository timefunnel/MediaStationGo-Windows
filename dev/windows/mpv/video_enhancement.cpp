// Public RTX Video SDK and Maxine native APIs. No Veyra implementation code.
#define NVVIDEO_BUILD
#include "video_enhancement.h"
#include "hdr_pipeline.h"
#include <nvsdk_ngx.h>
#include <nvsdk_ngx_helpers_vsr.h>
#include <wrl/client.h>
#include <d3d11_1.h>
#include <d3d11_4.h>
#include <dxgi.h>
#include <cstdio>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>
#include <cmath>
using Microsoft::WRL::ComPtr;

namespace {
void check(long long code, const char *operation, bool success) {
    if (!success) { char detail[160]; std::snprintf(detail, sizeof(detail), "%s failed: 0x%llx", operation, code); throw std::runtime_error(detail); }
}
void report(char *error, size_t size, const char *detail) { if (error && size) std::snprintf(error, size, "%s", detail); }
void ngx(NVSDK_NGX_Result result, const char *operation) { check(result, operation, NVSDK_NGX_SUCCEED(result)); }
void cuda(int result, const char *operation) { check(result, operation, result == 0); }
void cleanup_result(long long result, const char *operation, bool success) {
    if (!success) std::fprintf(stderr, "Native video cleanup: %s failed: 0x%llx\n", operation, result);
}
struct Image {
    uint32_t width, height; int32_t pitch, format, type;
    uint8_t pixel_bytes, component_bytes, components, planar, gpu, colorspace, reserved[2];
    void *pixels, *delete_ptr; void (*delete_proc)(void *); uint64_t buffer_bytes;
};
static_assert(sizeof(Image) == 64);
}

// Keep NGX create/evaluate/release state out of the context shared with mpv.
struct ContextGuard {
    ID3D11Multithread *multithread; ID3D11DeviceContext1 *context;
    ComPtr<ID3DDeviceContextState> caller;
    ContextGuard(ID3D11Multithread *lock, ID3D11DeviceContext1 *ctx, ID3DDeviceContextState *isolated) : multithread(lock), context(ctx) {
        multithread->Enter(); context->SwapDeviceContextState(isolated, &caller);
    }
    ~ContextGuard() { context->SwapDeviceContextState(caller.Get(), nullptr); multithread->Leave(); }
};
struct nvvideo_session {
    nvvideo_config config{};
    std::vector<HMODULE> modules;
    ComPtr<ID3D11Device1> device1;
    ComPtr<ID3D11DeviceContext1> context1;
    ComPtr<ID3DDeviceContextState> isolated;
    ComPtr<ID3D11Multithread> multithread;
    ComPtr<ID3D11Texture2D> output;
    ComPtr<ID3D11Texture2D> vfg_previous, vfg_current;
    void *vfg_resources[3]{};
    std::unique_ptr<HdrPipeline> hdr;
    NVSDK_NGX_Handle *feature = nullptr;
    NVSDK_NGX_Parameter *params = nullptr;
    bool ngx_initialized = false;
    void *nr = nullptr;
    void *(__cdecl *nr_create)(const nvvideo_config *, char *, size_t) = nullptr;
    int (__cdecl *nr_process)(void *, ID3D11Texture2D *, ID3D11Texture2D *, char *, size_t) = nullptr;
    void (__cdecl *nr_destroy)(void *) = nullptr;
    void *effect = nullptr, *stream = nullptr;
    Image images[3]{};
    int (__cdecl *set_device)(int) = nullptr;
    int (__cdecl *get_device)(int *, IDXGIAdapter *) = nullptr;
    int (__cdecl *stream_create)(void **, unsigned) = nullptr;
    int (__cdecl *stream_sync)(void *) = nullptr;
    int (__cdecl *stream_destroy)(void *) = nullptr;
    int (__cdecl *register_texture)(void **, ID3D11Resource *, unsigned) = nullptr;
    int (__cdecl *unregister_texture)(void *) = nullptr;
    int (__cdecl *map)(int, void **, void *) = nullptr;
    int (__cdecl *unmap)(int, void **, void *) = nullptr;
    int (__cdecl *mapped_array)(void **, void *, unsigned, unsigned) = nullptr;
    int (__cdecl *copy_from_array)(void *, size_t, void *, size_t, size_t, size_t, size_t, int, void *) = nullptr;
    int (__cdecl *copy_to_array)(void *, size_t, size_t, const void *, size_t, size_t, size_t, int, void *) = nullptr;
    int (__cdecl *create_effect)(const char *, void **) = nullptr;
    void (__cdecl *destroy_effect)(void *) = nullptr;
    int (__cdecl *set_u32)(void *, const char *, unsigned) = nullptr;
    int (__cdecl *set_f32)(void *, const char *, float) = nullptr;
    int (__cdecl *set_image)(void *, const char *, Image *) = nullptr;
    int (__cdecl *set_stream)(void *, const char *, void *) = nullptr;
    int (__cdecl *load)(void *) = nullptr;
    int (__cdecl *run)(void *, int) = nullptr;
    int (__cdecl *alloc)(Image *, unsigned, unsigned, int, int, unsigned, unsigned, unsigned) = nullptr;
    void (__cdecl *dealloc)(Image *) = nullptr;

    HMODULE library(const wchar_t *name) {
        const auto path = std::wstring(config.runtime_directory) + L"\\" + name;
        HMODULE module = LoadLibraryExW(path.c_str(), nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
        check(GetLastError(), "LoadLibraryEx(native component)", module != nullptr);
        modules.push_back(module); return module;
    }
    template<class T> void symbol(HMODULE module, const char *name, T &function) {
        function = reinterpret_cast<T>(GetProcAddress(module, name));
        check(GetLastError(), name, function != nullptr);
    }
    ~nvvideo_session() {
        std::unique_ptr<ContextGuard> guard;
        if (multithread && context1 && isolated) guard = std::make_unique<ContextGuard>(multithread.Get(), context1.Get(), isolated.Get());
        if (stream && stream_sync) { int result = stream_sync(stream); cleanup_result(result, "CUDA stream synchronize", result == 0); }
        for (auto &resource : vfg_resources) if (resource) {
            int result = unregister_texture(resource);
            cleanup_result(result, "CUDA VFG unregister", result == 0);
            resource = nullptr;
        }
        if (nr && nr_destroy) nr_destroy(nr);
        if (effect && destroy_effect) destroy_effect(effect);
        if (dealloc) for (auto &image : images) if (image.pixels) dealloc(&image);
        if (stream && stream_destroy) { int result = stream_destroy(stream); cleanup_result(result, "CUDA stream destroy", result == 0); }
        if (feature) { auto result = NVSDK_NGX_D3D11_ReleaseFeature(feature); cleanup_result(result, "VSR ReleaseFeature", NVSDK_NGX_SUCCEED(result)); }
        if (params) { auto result = NVSDK_NGX_D3D11_DestroyParameters(params); cleanup_result(result, "VSR DestroyParameters", NVSDK_NGX_SUCCEED(result)); }
        if (ngx_initialized) { auto result = NVSDK_NGX_D3D11_Shutdown1(config.device); cleanup_result(result, "VSR Shutdown", NVSDK_NGX_SUCCEED(result)); }
        for (auto it = modules.rbegin(); it != modules.rend(); ++it) FreeLibrary(*it);
    }
    void initialize() {
        HRESULT hr = config.device->QueryInterface(IID_PPV_ARGS(&device1)); check(hr, "D3D11 device1", SUCCEEDED(hr));
        hr = config.context->QueryInterface(IID_PPV_ARGS(&context1)); check(hr, "D3D11 context1", SUCCEEDED(hr));
        hr = config.context->QueryInterface(IID_PPV_ARGS(&multithread)); check(hr, "D3D11 multithread", SUCCEEDED(hr));
        multithread->SetMultithreadProtected(TRUE);
        auto level = config.device->GetFeatureLevel(); D3D_FEATURE_LEVEL chosen{};
        hr = device1->CreateDeviceContextState(0, &level, 1, D3D11_SDK_VERSION, __uuidof(ID3D11Device), &chosen, &isolated);
        check(hr, "D3D11 context isolation", SUCCEEDED(hr));
        ContextGuard guard(multithread.Get(), context1.Get(), isolated.Get());
        if (config.transfer != NVVIDEO_SDR) { hdr = std::make_unique<HdrPipeline>(); hdr->initialize(config); }
        D3D11_TEXTURE2D_DESC desc{};
        desc.Width = config.out_width; desc.Height = config.out_height; desc.MipLevels = desc.ArraySize = 1;
        desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM; desc.SampleDesc.Count = 1;
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE | (config.mode != 2 ? D3D11_BIND_UNORDERED_ACCESS : 0);
        hr = config.device->CreateTexture2D(&desc, nullptr, &output); check(hr, "D3D11 output texture", SUCCEEDED(hr));
        if (config.mode == 1) {
            library(L"nvngx_vsr.dll");
            const wchar_t *paths[] = {config.runtime_directory};
            NVSDK_NGX_FeatureCommonInfo common{}; common.PathListInfo.Path = paths; common.PathListInfo.Length = 1;
            ngx(NVSDK_NGX_D3D11_Init(0, config.cache_directory, config.device, &common), "VSR Init"); ngx_initialized = true;
            ngx(NVSDK_NGX_D3D11_GetCapabilityParameters(&params), "VSR capability");
            int available = 0; ngx(params->Get(NVSDK_NGX_Parameter_VSR_Available, &available), "VSR available");
            check(available, "VSR not supported by this GPU/driver", available == 1);
            ngx(NVSDK_NGX_D3D11_CreateFeature(config.context, NVSDK_NGX_Feature_VSR, params, &feature), "VSR CreateFeature");
        } else if (config.mode == 3) {
            HMODULE self = nullptr;
            check(GetLastError(), "Native bridge module", GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, reinterpret_cast<LPCWSTR>(&nvvideo_abi), &self) != 0);
            wchar_t path[32768]; DWORD length = GetModuleFileNameW(self, path, 32768);
            check(GetLastError(), "Native bridge path", length && length < 32768);
            std::wstring adapter_path(path); adapter_path.resize(adapter_path.find_last_of(L"\\/") + 1); adapter_path += L"nvngx.dll";
            auto module = LoadLibraryExW(adapter_path.c_str(), nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
            check(GetLastError(), "DLSSNR adapter load", module != nullptr); modules.push_back(module);
            symbol(module, "nr_create", nr_create); symbol(module, "nr_process", nr_process); symbol(module, "nr_destroy", nr_destroy);
            char detail[512]{}; nr = nr_create(&config, detail, sizeof(detail));
            if (!nr) throw std::runtime_error(detail);
        } else {
            auto runtime = library(L"cudart64_12.dll");
            library(L"nvngxruntime.dll"); auto image = library(L"NVCVImage.dll");
            library(L"nvVFXVideoFrameGeneration.dll"); auto vfx = library(L"NVVideoEffects.dll");
#define CUDA_SYMBOL(field, name) symbol(runtime, name, field)
            CUDA_SYMBOL(get_device, "cudaD3D11GetDevice"); CUDA_SYMBOL(set_device, "cudaSetDevice");
            CUDA_SYMBOL(stream_create, "cudaStreamCreateWithFlags"); CUDA_SYMBOL(stream_sync, "cudaStreamSynchronize"); CUDA_SYMBOL(stream_destroy, "cudaStreamDestroy");
            CUDA_SYMBOL(register_texture, "cudaGraphicsD3D11RegisterResource"); CUDA_SYMBOL(unregister_texture, "cudaGraphicsUnregisterResource");
            CUDA_SYMBOL(map, "cudaGraphicsMapResources"); CUDA_SYMBOL(unmap, "cudaGraphicsUnmapResources"); CUDA_SYMBOL(mapped_array, "cudaGraphicsSubResourceGetMappedArray");
            CUDA_SYMBOL(copy_from_array, "cudaMemcpy2DFromArrayAsync"); CUDA_SYMBOL(copy_to_array, "cudaMemcpy2DToArrayAsync");
#undef CUDA_SYMBOL
            symbol(vfx, "NvVFX_CreateEffect", create_effect); symbol(vfx, "NvVFX_DestroyEffect", destroy_effect);
            symbol(vfx, "NvVFX_SetU32", set_u32); symbol(vfx, "NvVFX_SetImage", set_image); symbol(vfx, "NvVFX_SetCudaStream", set_stream);
            symbol(vfx, "NvVFX_SetF32", set_f32);
            symbol(vfx, "NvVFX_Load", load); symbol(vfx, "NvVFX_Run", run);
            symbol(image, "NvCVImage_Alloc", alloc); symbol(image, "NvCVImage_Dealloc", dealloc);
            ComPtr<IDXGIDevice> dxgi_device; ComPtr<IDXGIAdapter> adapter;
            hr = config.device->QueryInterface(IID_PPV_ARGS(&dxgi_device)); check(hr, "VFG DXGI device", SUCCEEDED(hr));
            hr = dxgi_device->GetAdapter(&adapter); check(hr, "VFG DXGI adapter", SUCCEEDED(hr));
            int device = -1; cuda(get_device(&device, adapter.Get()), "cudaD3D11GetDevice"); cuda(set_device(device), "cudaSetDevice");
            cuda(stream_create(&stream, 1), "cudaStreamCreate");
            cuda(create_effect("VideoFrameGeneration", &effect), "VFG CreateEffect");
            for (auto &im : images) cuda(alloc(&im, config.width, config.height, hdr ? 13 : 7, hdr ? 11 : 1, 0, 1, 32), "VFG image allocation");
            cuda(set_stream(effect, "CudaStream", stream), "VFG stream");
            cuda(set_u32(effect, "InputWidth", config.width), "VFG width"); cuda(set_u32(effect, "InputHeight", config.height), "VFG height");
            cuda(set_u32(effect, "Mode", config.vfg_quality), "VFG quality");
            cuda(set_u32(effect, "FrameMultiplier", config.multiplier), "VFG multiplier");
            cuda(set_u32(effect, "AutomaticShotChangeDetectionEnabled", config.scene_detection), "VFG scene detection");
            cuda(set_image(effect, "SrcImage0", &images[0]), "VFG input0"); cuda(set_image(effect, "SrcImage1", &images[1]), "VFG input1"); cuda(set_image(effect, "DstImage0", &images[2]), "VFG output");
            cuda(load(effect), "VFG Load");
            // Registration is expensive; keep it for the lifetime of the session
            // and never register decoder/VO-owned allocations with CUDA.
            if (!hdr) {
                hr = config.device->CreateTexture2D(&desc, nullptr, &vfg_previous); check(hr, "VFG previous texture", SUCCEEDED(hr));
                hr = config.device->CreateTexture2D(&desc, nullptr, &vfg_current); check(hr, "VFG current texture", SUCCEEDED(hr));
            }
            ID3D11Texture2D *textures[] = {
                hdr ? hdr->previous.Get() : vfg_previous.Get(),
                hdr ? hdr->current.Get() : vfg_current.Get(),
                hdr ? hdr->generated.Get() : output.Get(),
            };
            for (unsigned i = 0; i < 3; i++)
                cuda(register_texture(&vfg_resources[i], textures[i], 0), "CUDA VFG register");
        }
    }
    void transfer(void *resource, Image &image, bool to_image) {
        void *array = nullptr;
        bool mapped = false;
        try {
            cuda(map(1, &resource, stream), "CUDA D3D11 map"); mapped = true;
            cuda(mapped_array(&array, resource, 0, 0), "CUDA D3D11 array");
            if (to_image) cuda(copy_from_array(image.pixels, static_cast<size_t>(image.pitch), array, 0, 0, config.width * 4ull, config.height, 3, stream), "CUDA input copy");
            else cuda(copy_to_array(array, 0, 0, image.pixels, static_cast<size_t>(image.pitch), config.width * 4ull, config.height, 3, stream), "CUDA output copy");
            cuda(unmap(1, &resource, stream), "CUDA D3D11 unmap"); mapped = false;
        } catch (...) {
            if (mapped) { int result = unmap(1, &resource, stream); cleanup_result(result, "CUDA failed-transfer unmap", result == 0); }
            throw;
        }
    }
    void process(ID3D11Texture2D *previous, ID3D11Texture2D *current, ID3D11Texture2D *destination, uint32_t index, float timestep = 0.5f) {
        for (auto *texture : {current, destination, previous}) {
            if (!texture) continue;
            D3D11_TEXTURE2D_DESC desc{}; texture->GetDesc(&desc);
            bool dest = texture == destination;
            check(0, "Native frame format/dimensions mismatch", desc.ArraySize == 1 && desc.MipLevels == 1 && desc.Format == (hdr ? DXGI_FORMAT_P010 : DXGI_FORMAT_B8G8R8A8_UNORM) && desc.Width == (dest ? config.out_width : config.width) && desc.Height == (dest ? config.out_height : config.height));
        }
        if (config.mode == 2) {
            check(0, "VFG previous frame missing", previous != nullptr);
            if (index == 1) {
                {
                    // CUDA/D3D11 map/unmap also use the immediate context.
                    // Keep them isolated, and finish unmapping before unlock.
                    ContextGuard guard(multithread.Get(), context1.Get(), isolated.Get());
                    if (hdr) { hdr->input(previous, hdr->previous.Get()); hdr->input(current, hdr->current.Get()); }
                    else { config.context->CopyResource(vfg_previous.Get(), previous); config.context->CopyResource(vfg_current.Get(), current); }
                    transfer(vfg_resources[0], images[0], true);
                    transfer(vfg_resources[1], images[1], true);
                    cuda(stream_sync(stream), "VFG inputs synchronize");
                }
                // Both bindings signal a new source pair to VFG even when the
                // allocations are reused. Keep them bound for its later samples.
                cuda(set_image(effect, "SrcImage0", &images[0]), "VFG new pair input0");
                cuda(set_image(effect, "SrcImage1", &images[1]), "VFG new pair input1");
                cuda(set_u32(effect, "ShotChange", 0), "VFG new pair shot change");
            }
            if (config.multiplier == 0) {
                check(index, "VFG timestep/index", index > 0 && std::isfinite(timestep) && timestep > 0.f && timestep < 1.f);
                cuda(set_f32(effect, "Timestep", timestep), "VFG timestep");
            } else {
                check(index, "VFG frame index", index > 0 && index < config.multiplier);
                cuda(set_u32(effect, "FrameIndex", index), "VFG frame index");
            }
            cuda(run(effect, 0), "VFG Run");
            // Inference uses only private CUDA images, with no mapped D3D11
            // resources. Let decoder and VO use their context during this wait.
            cuda(stream_sync(stream), "VFG inference synchronize");
            ContextGuard guard(multithread.Get(), context1.Get(), isolated.Get());
            transfer(vfg_resources[2], images[2], false);
            cuda(stream_sync(stream), "VFG result synchronize");
            if (hdr) hdr->finish(hdr->generated.Get(), true);
            config.context->CopyResource(destination, hdr ? hdr->output.Get() : output.Get());
        } else {
            ContextGuard guard(multithread.Get(), context1.Get(), isolated.Get());
            if (hdr) { hdr->input(current); current = hdr->proxy.Get(); }
            if (config.mode == 1) {
                NVSDK_NGX_D3D11_VSR_Eval_Params eval{}; eval.pInput = current; eval.pOutput = output.Get();
                eval.InputSubrectSize = {config.width, config.height}; eval.OutputSubrectSize = {config.out_width, config.out_height}; eval.QualityLevel = static_cast<NVSDK_NGX_VSR_QualityLevel>(config.vsr_quality);
                ngx(NGX_D3D11_EVALUATE_VSR_EXT(config.context, feature, params, &eval), "VSR EvaluateFeature");
            } else {
                char detail[512]{};
                if (!nr_process(nr, current, output.Get(), detail, sizeof(detail))) throw std::runtime_error(detail);
            }
            if (hdr) hdr->finish(output.Get(), false);
            config.context->CopyResource(destination, hdr ? hdr->output.Get() : output.Get());
        }
        const HRESULT reason = config.device->GetDeviceRemovedReason();
        check(reason, "Native D3D11 device removed", SUCCEEDED(reason));
    }
};

uint32_t __cdecl nvvideo_abi() { return NVVIDEO_ABI; }
nvvideo_session *__cdecl nvvideo_create(const nvvideo_config *config, char *error, size_t size) {
    try {
        if (!config || config->abi != NVVIDEO_ABI || config->mode < 1 || config->mode > 3 || !config->device || !config->context || !config->runtime_directory || !config->cache_directory || !config->width || !config->height || !config->out_width || !config->out_height || config->out_width > 16384 || config->out_height > 16384 || (config->mode != 1 && (config->width != config->out_width || config->height != config->out_height)) || config->vsr_quality < 1 || config->vsr_quality > 4 || config->vfg_quality > 2 || config->multiplier == 1 || config->multiplier > 8 || config->scene_detection > 1)
            throw std::runtime_error("Invalid native enhancement configuration");
        if (config->transfer > NVVIDEO_HLG || config->full_range > 1 || config->chroma_location > 5 || (config->transfer != NVVIDEO_SDR && ((config->width | config->height | config->out_width | config->out_height) & 1))) throw std::runtime_error("Invalid PQ/HLG format/range/chroma configuration");
        if (config->nr_style < 0 || config->nr_style > 2 || config->nr_auto_mask < 0 || config->nr_auto_mask > 1 || config->nr_ui_correction < 0 || config->nr_ui_correction > 1 || !std::isfinite(config->nr_skin) || !(config->nr_skin == -1.f || (config->nr_skin >= 0.f && config->nr_skin <= 2.f))) throw std::runtime_error("Invalid DLSSNR configuration");
        if (config->nr_target != 0 && config->nr_target != 480 && config->nr_target != 720 && config->nr_target != 900 && config->nr_target != 1080 && config->nr_target != 1440) throw std::runtime_error("Invalid DLSSNR processing size");
        for (float value : {config->nr_intensity, config->nr_tone, config->nr_structure}) if (!std::isfinite(value) || value < 0.f || value > 1.f) throw std::runtime_error("Invalid DLSSNR strength");
        auto session = std::make_unique<nvvideo_session>(); session->config = *config; session->initialize(); return session.release();
    } catch (const std::exception &exception) { report(error, size, exception.what()); return nullptr; }
}
int __cdecl nvvideo_process(nvvideo_session *session, ID3D11Texture2D *previous, ID3D11Texture2D *current, ID3D11Texture2D *output, uint32_t index, char *error, size_t size) {
    try {
        if (!session || !current || !output) throw std::runtime_error("Missing native enhancement frame");
        if (session->config.mode == 2 && session->config.multiplier == 0) throw std::runtime_error("Automatic VFG requires the explicit timestep entry point");
        session->process(previous, current, output, index); return 1;
    } catch (const std::exception &exception) { report(error, size, exception.what()); return 0; }
}
int __cdecl nvvideo_process_timestep(nvvideo_session *session, ID3D11Texture2D *previous, ID3D11Texture2D *current, ID3D11Texture2D *output, uint32_t index, float timestep, char *error, size_t size) {
    try {
        if (!session || !current || !output || session->config.mode != 2 || session->config.multiplier != 0)
            throw std::runtime_error("Explicit VFG timestep requires an automatic VFG session");
        if (!index || !std::isfinite(timestep) || timestep <= 0.f || timestep >= 1.f)
            throw std::runtime_error("VFG timestep must be strictly between zero and one, with a positive pair index");
        session->process(previous, current, output, index, timestep); return 1;
    } catch (const std::exception &exception) { report(error, size, exception.what()); return 0; }
}
void __cdecl nvvideo_destroy(nvvideo_session *session) { delete session; }
