// Explicit diagnostic proxy for the unchanged native video bridge. Never packaged.
// Capture private SDK inputs/results with asynchronous GPU readback; no media URLs.
#define NVVIDEO_BUILD
#define NOMINMAX
#include "video_enhancement.h"
#include <d3d11_1.h>
#include <d3d11_4.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
#include <array>
#include <atomic>
#include <cstdio>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>
#include <chrono>
#include <thread>
using Microsoft::WRL::ComPtr;

namespace {
constexpr unsigned width = 320, height = 180, max_frames = 400;
std::wstring environment(const wchar_t *key) {
    const DWORD length = GetEnvironmentVariableW(key, nullptr, 0);
    if (!length) throw std::runtime_error("Missing VFG capture environment variable");
    std::wstring value(length, L'\0');
    if (!GetEnvironmentVariableW(key, value.data(), length)) throw std::runtime_error("Cannot read VFG capture environment");
    value.resize(length - 1); return value;
}
void checked(HRESULT hr, const char *operation) {
    if (FAILED(hr)) { char message[192]; std::snprintf(message, sizeof(message), "%s: 0x%08lx", operation, static_cast<unsigned long>(hr)); throw std::runtime_error(message); }
}
int64_t counter() { LARGE_INTEGER qpc; QueryPerformanceCounter(&qpc); return qpc.QuadPart; }
struct RealBridge {
    HMODULE module = nullptr;
    decltype(&nvvideo_create) create = nullptr;
    decltype(&nvvideo_process) process = nullptr;
    decltype(&nvvideo_process_timestep) timestep = nullptr;
    decltype(&nvvideo_destroy) destroy = nullptr;
    RealBridge() {
        module = LoadLibraryExW(environment(L"VFG_CAPTURE_BRIDGE").c_str(), nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
        if (!module) throw std::runtime_error("Cannot load the original video bridge for capture");
        auto abi = reinterpret_cast<decltype(&nvvideo_abi)>(GetProcAddress(module, "nvvideo_abi"));
        create = reinterpret_cast<decltype(create)>(GetProcAddress(module, "nvvideo_create"));
        process = reinterpret_cast<decltype(process)>(GetProcAddress(module, "nvvideo_process"));
        timestep = reinterpret_cast<decltype(timestep)>(GetProcAddress(module, "nvvideo_process_timestep"));
        destroy = reinterpret_cast<decltype(destroy)>(GetProcAddress(module, "nvvideo_destroy"));
        if (!abi || abi() != NVVIDEO_ABI || !create || !process || !timestep || !destroy) throw std::runtime_error("Original capture bridge ABI mismatch");
    }
    ~RealBridge() { if (module) FreeLibrary(module); }
};
RealBridge &bridge() { static RealBridge value; return value; }
struct Guard {
    ID3D11Multithread *lock;
    ID3D11DeviceContext1 *context;
    ComPtr<ID3DDeviceContextState> caller;
    Guard(ID3D11Multithread *l, ID3D11DeviceContext1 *c, ID3DDeviceContextState *state) : lock(l), context(c) {
        lock->Enter(); context->SwapDeviceContextState(state, &caller);
    }
    ~Guard() { context->SwapDeviceContextState(caller.Get(), nullptr); lock->Leave(); }
};
struct Capture {
    struct Slot {
        ComPtr<ID3D11Texture2D> target, staging;
        ComPtr<ID3D11UnorderedAccessView> uav;
        ComPtr<ID3D11Query> ready;
        bool pending = false;
        uint64_t sequence = 0, pair = 0;
        unsigned index = 0;
        float timestep = 0;
        double pts = 0;
        int64_t qpc = 0;
        const char *kind = "";
    };
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext1> context;
    ComPtr<ID3D11Multithread> lock;
    ComPtr<ID3DDeviceContextState> state;
    ComPtr<ID3D11ComputeShader> shader;
    ComPtr<ID3D11Buffer> crop_buffer;
    std::array<Slot, 16> slots;
    std::wstring root, request;
    FILE *csv = nullptr, *pixels = nullptr;
    int64_t frequency = 0, until = 0, last_check = 0;
    uint64_t pair = 0, sequence = 0, written = 0, dropped = 0;
    unsigned generated = 0;
    bool hdr = false, rendered = false;
    std::array<unsigned, 4> crop{};
    double pts = 0;

    explicit Capture(const nvvideo_config &config, bool final_render = false) : device(config.device), hdr(config.transfer != NVVIDEO_SDR), rendered(final_render) {
        root = environment(L"VFG_CAPTURE_DIRECTORY"); request = root + (rendered ? L"\\render.request" : L"\\capture.request");
        LARGE_INTEGER f; QueryPerformanceFrequency(&f); frequency = f.QuadPart;
        checked(config.context->QueryInterface(IID_PPV_ARGS(&context)), "Capture context1");
        checked(config.context->QueryInterface(IID_PPV_ARGS(&lock)), "Capture context lock");
        ComPtr<ID3D11Device1> device1; checked(device.As(&device1), "Capture device1");
        auto level = device->GetFeatureLevel(); D3D_FEATURE_LEVEL chosen{};
        checked(device1->CreateDeviceContextState(0, &level, 1, D3D11_SDK_VERSION, __uuidof(ID3D11Device), &chosen, &state), "Capture isolated state");
        const char *source = R"(
            Texture2D<float4> source : register(t0);
            RWTexture2D<float4> target : register(u0);
            [numthreads(8,8,1)] void main(uint3 id : SV_DispatchThreadID) {
                if (id.x >= 320 || id.y >= 180) return;
                uint w,h; source.GetDimensions(w,h);
                float4 p = source.Load(int3((id.xy * uint2(w,h)) / uint2(320,180),0));
                target[id.xy] = float4(p.rgb,1);
            })";
        std::string code(source);
        if (hdr) code.replace(code.find("float4(p.rgb,1)"), 15, "float4(p.rrr,1)");
        if (rendered) {
            code = "cbuffer CaptureCrop : register(b0) { uint4 crop; };\n" + code;
            const std::string from = "(id.xy * uint2(w,h)) / uint2(320,180)";
            code.replace(code.find(from), from.size(), "crop.xy + (id.xy * crop.zw) / uint2(320,180)");
            D3D11_BUFFER_DESC desc{}; desc.ByteWidth = 16; desc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
            checked(device->CreateBuffer(&desc, nullptr, &crop_buffer), "Capture crop buffer");
        }
        ComPtr<ID3DBlob> bytecode, errors;
        const auto hr = D3DCompile(code.data(), code.size(), nullptr, nullptr, nullptr, "main", "cs_5_0", D3DCOMPILE_ENABLE_STRICTNESS, 0, &bytecode, &errors);
        if (FAILED(hr) && errors) std::fprintf(stderr, "VFG capture shader: %s\n", static_cast<const char *>(errors->GetBufferPointer()));
        checked(hr, "Capture shader compile");
        checked(device->CreateComputeShader(bytecode->GetBufferPointer(), bytecode->GetBufferSize(), nullptr, &shader), "Capture shader");
        for (auto &slot : slots) {
            D3D11_TEXTURE2D_DESC desc{}; desc.Width = width; desc.Height = height; desc.MipLevels = desc.ArraySize = 1;
            desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM; desc.SampleDesc.Count = 1; desc.BindFlags = D3D11_BIND_UNORDERED_ACCESS;
            checked(device->CreateTexture2D(&desc, nullptr, &slot.target), "Capture target");
            checked(device->CreateUnorderedAccessView(slot.target.Get(), nullptr, &slot.uav), "Capture UAV");
            desc.BindFlags = 0; desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
            checked(device->CreateTexture2D(&desc, nullptr, &slot.staging), "Capture readback");
            D3D11_QUERY_DESC query{D3D11_QUERY_EVENT, 0}; checked(device->CreateQuery(&query, &slot.ready), "Capture completion query");
        }
    }
    void start(const nvvideo_config &config) {
        static std::atomic<unsigned> serial{0};
        generated = 0; written = dropped = sequence = 0;
        const auto name = std::wstring(rendered ? L"\\render-" : L"\\session-") + std::to_wstring(GetCurrentProcessId()) + L"-" + std::to_wstring(counter()) + L"-" + std::to_wstring(++serial);
        if (_wfopen_s(&csv, (root + name + L".csv").c_str(), L"wb") || !csv) throw std::runtime_error("Cannot create VFG capture CSV");
        if (_wfopen_s(&pixels, (root + name + L".rgba").c_str(), L"wb") || !pixels) throw std::runtime_error("Cannot create VFG capture pixels");
        std::fprintf(csv, "# frequency=%lld source=%ux%u thumbnail=%ux%u multiplier=%u quality=%u encoding=%s\n", static_cast<long long>(frequency), config.width, config.height, width, height, config.multiplier, config.vfg_quality, rendered ? "display-rgba" : hdr ? "p010-luma" : "sdr-rgba");
        std::fprintf(csv, "sequence,pair,index,timestep,qpc,kind,offset,pts\n");
        until = counter() + frequency * 8;
        std::fprintf(stderr, "VFG_CAPTURE_STARTED kind=%s duration=8s max-generated=%u size=%ux%u\n", rendered ? "rendered" : "sdk", rendered ? 1600 : max_frames, width, height);
    }
    void drain() {
        for (auto &slot : slots) {
            if (!slot.pending) continue;
            const auto ready = context->GetData(slot.ready.Get(), nullptr, 0, D3D11_ASYNC_GETDATA_DONOTFLUSH);
            checked(ready, "Capture query"); if (ready == S_FALSE) continue;
            D3D11_MAPPED_SUBRESOURCE mapped{};
            const auto result = context->Map(slot.staging.Get(), 0, D3D11_MAP_READ, D3D11_MAP_FLAG_DO_NOT_WAIT, &mapped);
            if (result == DXGI_ERROR_WAS_STILL_DRAWING) continue;
            checked(result, "Capture map");
            const auto offset = _ftelli64(pixels);
            bool ok = offset >= 0;
            for (unsigned y = 0; y < height && ok; y++) ok = std::fwrite(static_cast<unsigned char *>(mapped.pData) + y * mapped.RowPitch, width * 4, 1, pixels) == 1;
            context->Unmap(slot.staging.Get(), 0);
            if (!ok) throw std::runtime_error("Cannot write VFG capture pixels");
            if (std::fprintf(csv, "%llu,%llu,%u,%.9f,%lld,%s,%lld,%.9f\n", static_cast<unsigned long long>(slot.sequence), static_cast<unsigned long long>(slot.pair), slot.index, slot.timestep, static_cast<long long>(slot.qpc), slot.kind, static_cast<long long>(offset), slot.pts) < 0) throw std::runtime_error("Cannot write VFG capture CSV");
            slot.pending = false; written++;
        }
    }
    void submit(ID3D11Texture2D *texture, const char *kind, unsigned index, float timestep) {
        Slot *slot = nullptr;
        for (auto &candidate : slots) if (!candidate.pending) { slot = &candidate; break; }
        if (!slot) { dropped++; return; }
        D3D11_SHADER_RESOURCE_VIEW_DESC desc{}; desc.Format = hdr ? DXGI_FORMAT_R16_UNORM : DXGI_FORMAT_B8G8R8A8_UNORM;
        if (rendered) { D3D11_TEXTURE2D_DESC texture_desc{}; texture->GetDesc(&texture_desc); desc.Format = texture_desc.Format; }
        desc.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D; desc.Texture2D.MipLevels = 1;
        ComPtr<ID3D11ShaderResourceView> view; checked(device->CreateShaderResourceView(texture, &desc, &view), "Capture input SRV");
        auto *srv = view.Get(); auto *uav = slot->uav.Get();
        context->CSSetShader(shader.Get(), nullptr, 0); context->CSSetShaderResources(0, 1, &srv); context->CSSetUnorderedAccessViews(0, 1, &uav, nullptr);
        if (rendered) {
            context->UpdateSubresource(crop_buffer.Get(), 0, nullptr, crop.data(), 0, 0);
            auto *buffer = crop_buffer.Get(); context->CSSetConstantBuffers(0, 1, &buffer);
        }
        context->Dispatch((width + 7) / 8, (height + 7) / 8, 1);
        srv = nullptr; uav = nullptr; context->CSSetShaderResources(0, 1, &srv); context->CSSetUnorderedAccessViews(0, 1, &uav, nullptr);
        context->CopyResource(slot->staging.Get(), slot->target.Get()); context->End(slot->ready.Get());
        slot->sequence = sequence++; slot->pair = pair; slot->index = index; slot->timestep = timestep; slot->qpc = counter(); slot->kind = kind; slot->pending = true; slot->pts = pts;
    }
    void after_render(const nvvideo_config &config, ID3D11Texture2D *texture, double timestamp, unsigned present, unsigned mix, const std::array<unsigned, 4> &rect) {
        if (!csv) {
            const auto now = counter(); if (now - last_check < frequency / 5) return; last_check = now;
            if (GetFileAttributesW(request.c_str()) == INVALID_FILE_ATTRIBUTES) return;
            if (!DeleteFileW(request.c_str())) throw std::runtime_error("Cannot consume rendered capture request");
            start(config);
        }
        Guard guard(lock.Get(), context.Get(), state.Get()); drain();
        if (counter() < until && generated < 1600) {
            pts = timestamp; pair = present; crop = rect;
            submit(texture, "rendered", mix, 0); generated++; context->Flush();
        } else {
            bool pending = false; for (const auto &slot : slots) pending |= slot.pending;
            if (!pending) finish();
        }
    }
    void after(const nvvideo_config &config, ID3D11Texture2D *previous, ID3D11Texture2D *current, ID3D11Texture2D *result, unsigned index, float timestep) {
        if (index == 1) pair++;
        if (!csv) {
            const auto now = counter(); if (now - last_check < frequency / 5) return; last_check = now;
            if (GetFileAttributesW(request.c_str()) == INVALID_FILE_ATTRIBUTES) return;
            if (!DeleteFileW(request.c_str())) throw std::runtime_error("Cannot consume VFG capture request");
            start(config);
        }
        Guard guard(lock.Get(), context.Get(), state.Get()); drain();
        if (counter() < until && generated < max_frames) {
            if (index == 1) { submit(previous, "previous", index, timestep); submit(current, "current", index, timestep); }
            submit(result, "generated", index, timestep); generated++; context->Flush();
        } else {
            bool pending = false; for (const auto &slot : slots) pending |= slot.pending;
            if (!pending) finish();
        }
    }
    void finish() {
        if (!csv) return;
        std::fprintf(csv, "# complete written=%llu dropped=%llu generated=%u\n", static_cast<unsigned long long>(written), static_cast<unsigned long long>(dropped), generated);
        std::fprintf(stderr, "VFG_CAPTURE_FINISHED written=%llu dropped=%llu generated=%u\n", static_cast<unsigned long long>(written), static_cast<unsigned long long>(dropped), generated);
        std::fclose(csv); std::fclose(pixels); csv = pixels = nullptr;
    }
    ~Capture() {
        if (csv && pixels) {
            try {
                Guard guard(lock.Get(), context.Get(), state.Get()); context->Flush();
                const auto end = counter() + frequency;
                for (;;) {
                    drain(); bool pending = false; for (const auto &slot : slots) pending |= slot.pending;
                    if (!pending || counter() >= end) break;
                    std::this_thread::sleep_for(std::chrono::milliseconds(1));
                }
                for (const auto &slot : slots) if (slot.pending) dropped++;
            } catch (const std::exception &e) { std::fprintf(stderr, "VFG_CAPTURE_ERROR cleanup=%s\n", e.what()); }
        }
        if (csv && pixels) finish();
        else { if (csv) std::fclose(csv); if (pixels) std::fclose(pixels); }
    }
};
void report(char *error, size_t size, const char *message) { if (error && size) std::snprintf(error, size, "%s", message); }
}
struct nvvideo_session {
    nvvideo_session *real = nullptr;
    nvvideo_config config{};
    std::unique_ptr<Capture> capture;
};
extern "C" uint32_t __cdecl nvvideo_abi() { return NVVIDEO_ABI; }
extern "C" nvvideo_session *__cdecl nvvideo_create(const nvvideo_config *config, char *error, size_t size) {
    try {
        if (!config) throw std::runtime_error("Missing capture config");
        auto session = std::make_unique<nvvideo_session>(); session->config = *config;
        session->real = bridge().create(config, error, size); if (!session->real) return nullptr;
        try { if (config->mode == 2) session->capture = std::make_unique<Capture>(*config); }
        catch (...) { bridge().destroy(session->real); throw; }
        return session.release();
    } catch (const std::exception &e) { report(error, size, e.what()); return nullptr; }
}
static int process(nvvideo_session *session, ID3D11Texture2D *previous, ID3D11Texture2D *current, ID3D11Texture2D *output, uint32_t index, float timestep, bool automatic, char *error, size_t size) {
    if (!session) { report(error, size, "Missing capture session"); return 0; }
    const int ok = automatic ? bridge().timestep(session->real, previous, current, output, index, timestep, error, size) : bridge().process(session->real, previous, current, output, index, error, size);
    if (!ok) return ok;
    try { if (session->capture) session->capture->after(session->config, previous, current, output, index, timestep); }
    catch (const std::exception &e) { std::fprintf(stderr, "VFG_CAPTURE_ERROR %s\n", e.what()); session->capture.reset(); }
    return ok;
}
extern "C" int __cdecl nvvideo_process(nvvideo_session *session, ID3D11Texture2D *previous, ID3D11Texture2D *current, ID3D11Texture2D *output, uint32_t index, char *error, size_t size) {
    const float timestep = session && session->config.multiplier ? float(index) / session->config.multiplier : 0;
    return process(session, previous, current, output, index, timestep, false, error, size);
}
extern "C" int __cdecl nvvideo_process_timestep(nvvideo_session *session, ID3D11Texture2D *previous, ID3D11Texture2D *current, ID3D11Texture2D *output, uint32_t index, float timestep, char *error, size_t size) {
    return process(session, previous, current, output, index, timestep, true, error, size);
}
extern "C" void __cdecl nvvideo_destroy(nvvideo_session *session) {
    if (!session) return;
    session->capture.reset(); bridge().destroy(session->real); delete session;
}

// Called only by the explicitly instrumented mpv, immediately before Present.
// Read back the actual back buffer, independently of libplacebo's PTS metadata.
namespace {
struct RenderState { std::unique_ptr<Capture> capture; bool failed = false; };
RenderState &render_state() { static RenderState state; return state; }
}
extern "C" __declspec(dllexport) void __cdecl vfg_capture_rendered_destroy() {
    render_state().capture.reset();
}
extern "C" __declspec(dllexport) void __cdecl vfg_capture_rendered(ID3D11Texture2D *texture, double pts, unsigned present, unsigned mix, unsigned x, unsigned y, unsigned w, unsigned h) {
    auto &capture = render_state().capture;
    auto &failed = render_state().failed;
    if (failed || !texture) return;
    try {
        ComPtr<ID3D11Device> device; texture->GetDevice(&device);
        ComPtr<ID3D11DeviceContext> context; device->GetImmediateContext(&context);
        D3D11_TEXTURE2D_DESC desc{}; texture->GetDesc(&desc);
        if (!w || !h || x > desc.Width || y > desc.Height || w > desc.Width - x || h > desc.Height - y) throw std::runtime_error("Rendered capture crop exceeds back buffer");
        nvvideo_config config{}; config.device = device.Get(); config.context = context.Get(); config.width = w; config.height = h;
        if (!capture || capture->device.Get() != device.Get()) capture = std::make_unique<Capture>(config, true);
        capture->after_render(config, texture, pts, present, mix, {x, y, w, h});
    } catch (const std::exception &e) {
        std::fprintf(stderr, "VFG_RENDER_CAPTURE_ERROR %s\n", e.what()); capture.reset(); failed = true;
    }
}
