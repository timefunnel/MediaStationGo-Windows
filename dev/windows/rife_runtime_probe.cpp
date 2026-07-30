#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>

#include "mpv/rife_runtime.h"

#include <d3d11.h>
#include <dxgi1_2.h>
#include <wrl/client.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <vector>

using Microsoft::WRL::ComPtr;

namespace {

using AbiVersionFn = uint32_t(__cdecl *)(void);
using CreateFn = rife_runtime *(__cdecl *)(
    const rife_runtime_config *, char *, size_t);
using ProcessFn = int(__cdecl *)(
    rife_runtime *, ID3D11Texture2D *, uint32_t, ID3D11Texture2D *, uint32_t,
    ID3D11Texture2D *, uint32_t, double, double,
    rife_frame_diagnostics *, char *, size_t);
using GetStatsFn = int(__cdecl *)(const rife_runtime *, rife_runtime_stats *);
using DestroyFn = void(__cdecl *)(rife_runtime *);

template <typename T>
bool load_symbol(HMODULE module, const char *name, T &output)
{
    output = reinterpret_cast<T>(GetProcAddress(module, name));
    if (output)
        return true;
    std::fprintf(stderr, "RIFE_RUNTIME_PROBE_SYMBOL_MISSING name=%s\n", name);
    return false;
}

bool find_nvidia_adapter(ComPtr<IDXGIAdapter1> &adapter,
                         std::wstring &description)
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
        if (desc.VendorId == 0x10de
            && !(desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) {
            adapter = candidate;
            description = desc.Description;
            return true;
        }
    }
    return false;
}

bool create_p010_texture(ID3D11Device *device, uint32_t width,
                         uint32_t height, UINT bind_flags,
                         ComPtr<ID3D11Texture2D> &texture)
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

bool initialize_p010_gradient(ID3D11DeviceContext *context,
                              ID3D11Texture2D *texture, uint32_t width,
                              uint32_t height, uint32_t horizontal_shift)
{
    const size_t luma_samples = static_cast<size_t>(width) * height;
    const size_t chroma_samples = luma_samples / 2;
    std::vector<uint16_t> samples(luma_samples + chroma_samples);
    for (uint32_t y = 0; y < height; ++y) {
        for (uint32_t x = 0; x < width; ++x) {
            const uint32_t shifted = (x + horizontal_shift) % width;
            const uint32_t code = 64 + static_cast<uint32_t>(
                876.0 * shifted / (width - 1) + 0.5);
            samples[static_cast<size_t>(y) * width + x] =
                static_cast<uint16_t>(code << 6);
        }
    }
    std::fill(samples.begin() + static_cast<ptrdiff_t>(luma_samples),
              samples.end(), static_cast<uint16_t>(512 << 6));
    context->UpdateSubresource(texture, 0, nullptr, samples.data(), width * 2,
                               static_cast<UINT>(samples.size()
                                                 * sizeof(uint16_t)));
    return true;
}

struct OutputValidation {
    uint32_t unique_luma_codes = 0;
    uint64_t non_8bit_luma_samples = 0;
    bool p010_packed = false;
};

bool validate_p010_output(ID3D11Device *device, ID3D11DeviceContext *context,
                          ID3D11Texture2D *texture, uint32_t width,
                          uint32_t height, OutputValidation &validation)
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
        const auto *row = reinterpret_cast<const uint16_t *>(
            static_cast<const uint8_t *>(mapped.pData)
            + static_cast<size_t>(mapped.RowPitch) * y);
        for (uint32_t x = 0; x < width; ++x) {
            const uint16_t stored = row[x];
            packed = packed && (stored & 0x3f) == 0;
            const uint32_t code = stored >> 6;
            seen[code] = true;
            if ((code & 0x3) != 0)
                non_8bit++;
        }
    }
    const auto *chroma = static_cast<const uint8_t *>(mapped.pData)
                       + static_cast<size_t>(mapped.RowPitch) * height;
    for (uint32_t y = 0; y < height / 2; ++y) {
        const auto *row = reinterpret_cast<const uint16_t *>(
            chroma + static_cast<size_t>(mapped.RowPitch) * y);
        for (uint32_t x = 0; x < width; ++x)
            packed = packed && (row[x] & 0x3f) == 0;
    }
    context->Unmap(staging.Get(), 0);
    validation.unique_luma_codes = static_cast<uint32_t>(
        std::count(seen.begin(), seen.end(), true));
    validation.non_8bit_luma_samples = non_8bit;
    validation.p010_packed = packed;
    return packed && validation.unique_luma_codes > 256 && non_8bit > 0;
}

} // namespace

int wmain(int argc, wchar_t **argv)
{
    if (argc != 8) {
        std::fprintf(stderr,
                     "usage: rife_runtime_probe.exe <runtime-dll> <engine> "
                     "<cudart> <width> <height> <warmup> <iterations>\n");
        return 2;
    }
    const uint32_t width = static_cast<uint32_t>(_wtoi(argv[4]));
    const uint32_t height = static_cast<uint32_t>(_wtoi(argv[5]));
    const int warmup = _wtoi(argv[6]);
    const int iterations = _wtoi(argv[7]);
    if (!width || !height || warmup < 1 || iterations < 1)
        return 2;

    HMODULE module = LoadLibraryExW(
        argv[1], nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
    if (!module) {
        std::fprintf(stderr,
                     "RIFE_RUNTIME_PROBE_LOAD_FAILED win32=%lu\n",
                     GetLastError());
        return 3;
    }
    AbiVersionFn abi_version = nullptr;
    CreateFn create = nullptr;
    ProcessFn process = nullptr;
    GetStatsFn get_stats = nullptr;
    DestroyFn destroy = nullptr;
    if (!load_symbol(module, "rife_runtime_abi_version", abi_version)
        || !load_symbol(module, "rife_runtime_create", create)
        || !load_symbol(module, "rife_runtime_process", process)
        || !load_symbol(module, "rife_runtime_get_stats", get_stats)
        || !load_symbol(module, "rife_runtime_destroy", destroy)) {
        FreeLibrary(module);
        return 4;
    }
    if (abi_version() != RIFE_RUNTIME_ABI_VERSION) {
        std::fprintf(stderr, "RIFE_RUNTIME_PROBE_ABI_MISMATCH\n");
        FreeLibrary(module);
        return 5;
    }

    ComPtr<IDXGIAdapter1> adapter;
    std::wstring adapter_name;
    if (!find_nvidia_adapter(adapter, adapter_name)) {
        std::fprintf(stderr, "RIFE_RUNTIME_PROBE_NVIDIA_ADAPTER_MISSING\n");
        FreeLibrary(module);
        return 6;
    }
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    D3D_FEATURE_LEVEL feature_level{};
    const D3D_FEATURE_LEVEL requested[] = {
        D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0,
    };
    const HRESULT device_status = D3D11CreateDevice(
        adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, requested,
        static_cast<UINT>(std::size(requested)), D3D11_SDK_VERSION, &device,
        &feature_level, &context);
    if (FAILED(device_status)) {
        std::fprintf(stderr,
                     "RIFE_RUNTIME_PROBE_DEVICE_FAILED hr=0x%08lx\n",
                     static_cast<unsigned long>(device_status));
        FreeLibrary(module);
        return 7;
    }

    ComPtr<ID3D11Texture2D> frame0;
    ComPtr<ID3D11Texture2D> frame1;
    ComPtr<ID3D11Texture2D> output;
    if (!create_p010_texture(device.Get(), width, height,
                             D3D11_BIND_SHADER_RESOURCE, frame0)
        || !create_p010_texture(device.Get(), width, height,
                                D3D11_BIND_SHADER_RESOURCE, frame1)
        || !create_p010_texture(device.Get(), width, height,
                                D3D11_BIND_UNORDERED_ACCESS, output)
        || !initialize_p010_gradient(context.Get(), frame0.Get(), width,
                                     height, 0)
        || !initialize_p010_gradient(context.Get(), frame1.Get(), width,
                                     height, 64)) {
        std::fprintf(stderr, "RIFE_RUNTIME_PROBE_TEXTURE_FAILED\n");
        FreeLibrary(module);
        return 8;
    }

    const rife_runtime_config config{
        RIFE_RUNTIME_ABI_VERSION,
        device.Get(),
        context.Get(),
        argv[2],
        argv[3],
        width,
        height,
        RIFE_COLOR_MATRIX_BT709,
        1,
        8,
        32,
        24.0f,
        0.42f,
    };
    char error[1024]{};
    rife_runtime *runtime = create(&config, error, sizeof(error));
    if (!runtime) {
        std::fprintf(stderr, "RIFE_RUNTIME_PROBE_CREATE_FAILED detail=%s\n",
                     error);
        FreeLibrary(module);
        return 9;
    }

    bool ok = true;
    rife_frame_diagnostics diagnostics{};
    for (int index = 0; index < warmup && ok; ++index) {
        ok = process(runtime, frame0.Get(), 0, frame1.Get(), 0,
                     output.Get(), 0, index / 24.0, (index + 1) / 24.0,
                     &diagnostics, error, sizeof(error))
             == RIFE_RUNTIME_OK && !diagnostics.scene_cut;
    }
    std::vector<double> samples;
    samples.reserve(static_cast<size_t>(iterations));
    for (int index = 0; index < iterations && ok; ++index) {
        const auto started = std::chrono::steady_clock::now();
        ok = process(runtime, frame0.Get(), 0, frame1.Get(), 0,
                     output.Get(), 0, index / 24.0, (index + 1) / 24.0,
                     &diagnostics, error, sizeof(error))
             == RIFE_RUNTIME_OK && !diagnostics.scene_cut;
        const auto ended = std::chrono::steady_clock::now();
        samples.push_back(std::chrono::duration<double, std::milli>(
            ended - started).count());
    }
    rife_runtime_stats stats{};
    ok = ok && get_stats(runtime, &stats) == RIFE_RUNTIME_OK;
    OutputValidation validation;
    ok = ok && validate_p010_output(device.Get(), context.Get(), output.Get(),
                                    width, height, validation);
    destroy(runtime);
    runtime = create(&config, error, sizeof(error));
    rife_runtime_stats cached_stats{};
    ok = ok && runtime
        && get_stats(runtime, &cached_stats) == RIFE_RUNTIME_OK
        && cached_stats.runtime_cache_hit == 1;
    if (runtime)
        destroy(runtime);
    FreeLibrary(module);
    if (!ok || samples.size() != static_cast<size_t>(iterations)) {
        std::fprintf(stderr,
                     "RIFE_RUNTIME_PROBE_FAILED detail=%s scene-cut=%u\n",
                     error, diagnostics.scene_cut);
        return 10;
    }

    double total = 0;
    for (double value : samples)
        total += value;
    const double mean = total / samples.size();
    std::sort(samples.begin(), samples.end());
    const double p95 = samples[static_cast<size_t>(
        (samples.size() - 1) * 0.95 + 0.5)];
    std::printf(
        "RIFE_RUNTIME_PROBE_OK adapter=%ls source=%ux%u warmup=%d "
        "iterations=%d throughput=%.2fqps mean=%.2fms p95=%.2fms "
        "runtime-p95=%.2fms scene-mean=%.3fms inferred=%llu cuts=%llu "
        "cache-reopen=%s cold-init=%.2fms cached-init=%.2fms "
        "scene-class=%u average-delta=%.3f changed-ratio=%.4f "
        "average-kl=%.5f "
        "unique-luma=%u non-8bit-luma=%llu p010-packed=%s\n",
        adapter_name.c_str(), width, height, warmup, iterations, 1000.0 / mean,
        mean, p95, stats.inference_p95_ms,
        stats.pairs ? stats.scene_total_ms / stats.pairs : 0,
        static_cast<unsigned long long>(stats.inferred_pairs),
        static_cast<unsigned long long>(stats.scene_cuts),
        cached_stats.runtime_cache_hit ? "hit" : "miss",
        stats.runtime_initialization_ms,
        cached_stats.runtime_initialization_ms,
        diagnostics.classification, diagnostics.average_delta,
        diagnostics.changed_ratio, diagnostics.average_kl,
        validation.unique_luma_codes,
        static_cast<unsigned long long>(validation.non_8bit_luma_samples),
        validation.p010_packed ? "yes" : "no");
    return 0;
}
