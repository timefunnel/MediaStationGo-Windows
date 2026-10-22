// Real GPU smoke test of the same native ABI loaded by mpv; no account or media access.
#include "video_enhancement.h"
#include <wrl/client.h>
#include <dxgi1_2.h>
#include <cstdio>
#include <vector>
#include <algorithm>
using Microsoft::WRL::ComPtr;
static std::vector<unsigned char> pixels(unsigned shift) {
    std::vector<unsigned char> data(640 * 360 * 4);
    for (unsigned y = 0; y < 360; ++y) for (unsigned x = 0; x < 640; ++x) {
        const unsigned offset = (y * 640 + x) * 4, sx = (x + 640 - shift) % 640;
        data[offset] = static_cast<unsigned char>((sx / 12 + y / 12) % 2 * 180 + 30);
        data[offset + 1] = static_cast<unsigned char>((sx * 3 + y) % 256);
        data[offset + 2] = static_cast<unsigned char>((sx / 80) % 2 * 200 + 20); data[offset + 3] = 255;
    }
    return data;
}
static std::vector<unsigned char> read(ID3D11Device *device, ID3D11DeviceContext *context, ID3D11Texture2D *texture) {
    D3D11_TEXTURE2D_DESC desc{}; texture->GetDesc(&desc); desc.BindFlags = desc.MiscFlags = 0; desc.Usage = D3D11_USAGE_STAGING; desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ComPtr<ID3D11Texture2D> staging; if (FAILED(device->CreateTexture2D(&desc, nullptr, &staging))) return {};
    context->CopyResource(staging.Get(), texture); D3D11_MAPPED_SUBRESOURCE mapped{};
    if (FAILED(context->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped))) return {};
    std::vector<unsigned char> result(desc.Width * desc.Height * 4);
    for (unsigned y = 0; y < desc.Height; ++y) memcpy(result.data() + y * desc.Width * 4, static_cast<unsigned char *>(mapped.pData) + y * mapped.RowPitch, desc.Width * 4);
    context->Unmap(staging.Get(), 0); return result;
}
// Reuse the same input allocations across changing pairs, as the playback bridge
// does. A fresh session for each pair provides an independent cache baseline.
static bool vfg_pairs(nvvideo_config config, ID3D11Texture2D *first,
                      ID3D11Texture2D *second, ID3D11Texture2D *output) {
    config.multiplier = config.multiplier ? 4 : 0;
    const unsigned shifts[][2] = {{0, 12}, {12, 48}, {48, 54}, {54, 30}};
    for (unsigned quality : {0u, 1u, 2u}) {
        config.vfg_quality = quality;
        char error[512]{};
        std::vector<std::vector<unsigned char>> expected;
        // Finish all reference sessions before the uninterrupted playback run.
        for (unsigned phase = 0; phase < 2; phase++) {
            auto session = phase ? nvvideo_create(&config, error, sizeof(error)) : nullptr;
            if (phase && !session) { std::fprintf(stderr, "VFG pair create: %s\n", error); return false; }
            bool ok = true;
            unsigned pair = 0;
            for (const auto &shift : shifts) {
                if (!phase) session = nvvideo_create(&config, error, sizeof(error));
                if (!session) { std::fprintf(stderr, "VFG reference create: %s\n", error); ok = false; break; }
                auto a = pixels(shift[0]), b = pixels(shift[1]);
                config.context->UpdateSubresource(first, 0, nullptr, a.data(), 640 * 4, 0);
                config.context->UpdateSubresource(second, 0, nullptr, b.data(), 640 * 4, 0);
                for (unsigned index = 1; index <= 3; index++) {
                    int ran = config.multiplier
                        ? nvvideo_process(session, first, second, output, index, error, sizeof(error))
                        : nvvideo_process_timestep(session, first, second, output, index, 0.25f * index, error, sizeof(error));
                    if (!ran) { std::fprintf(stderr, "VFG pair run: %s\n", error); ok = false; break; }
                    auto actual = read(config.device, config.context, output);
                    if (actual.empty()) { std::fprintf(stderr, "VFG pair readback failed\n"); ok = false; break; }
                    if (!phase) { expected.push_back(std::move(actual)); continue; }
                    const auto &baseline = expected[pair * 3 + index - 1];
                    if (actual.size() != baseline.size()) { std::fprintf(stderr, "VFG pair dimensions changed\n"); ok = false; break; }
                    unsigned max_error = 0;
                    for (size_t j = 0; j < actual.size(); j++)
                        max_error = (std::max)(max_error, unsigned(std::abs(int(actual[j]) - int(baseline[j]))));
                    if (max_error > 1) {
                        std::fprintf(stderr, "VFG_PAIR_CACHE_FAIL multiplier=%u quality=%u pair=%u index=%u max-error=%u\n",
                            config.multiplier, quality, pair, index, max_error);
                        ok = false; break;
                    }
                }
                if (!phase) { nvvideo_destroy(session); session = nullptr; }
                if (!ok) break;
                pair++;
            }
            nvvideo_destroy(session);
            if (!ok) return false;
        }
        std::printf("VFG_PAIR_CACHE_OK multiplier=%u quality=%u pairs=4 outputs=12 fresh-session-max-error<=1\n",
            config.multiplier, quality);
    }
    return true;
}
int wmain(int argc, wchar_t **argv) {
    if (argc != 5) { std::fprintf(stderr, "Usage: test_video_enhancement vsr-dir vfg-dir cache-dir nr-dir\n"); return 2; }
    ComPtr<IDXGIFactory1> factory; if (FAILED(CreateDXGIFactory1(IID_PPV_ARGS(&factory)))) return 3;
    ComPtr<IDXGIAdapter1> adapter;
    for (unsigned i = 0; factory->EnumAdapters1(i, &adapter) != DXGI_ERROR_NOT_FOUND; ++i) { DXGI_ADAPTER_DESC1 desc{}; adapter->GetDesc1(&desc); if (desc.VendorId == 0x10de) break; adapter.Reset(); }
    if (!adapter) return 4;
    ComPtr<ID3D11Device> device; ComPtr<ID3D11DeviceContext> context;
    if (FAILED(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0, D3D11_SDK_VERSION, &device, nullptr, &context))) return 5;
    auto a = pixels(0), b = pixels(12);
    D3D11_TEXTURE2D_DESC desc{}; desc.Width = 640; desc.Height = 360; desc.MipLevels = desc.ArraySize = 1; desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM; desc.SampleDesc.Count = 1; desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    D3D11_SUBRESOURCE_DATA sourceA{a.data(), 640 * 4, 0}, sourceB{b.data(), 640 * 4, 0};
    ComPtr<ID3D11Texture2D> first, second; if (FAILED(device->CreateTexture2D(&desc, &sourceA, &first)) || FAILED(device->CreateTexture2D(&desc, &sourceB, &second))) return 6;
    for (unsigned test_mode : {1u, 2u, 3u, 0u}) {
        const unsigned mode = test_mode ? test_mode : 2u;
        const unsigned scale = mode == 1 ? 2u : 1u; desc.Width = 640 * scale; desc.Height = 360 * scale;
        ComPtr<ID3D11Texture2D> output; if (FAILED(device->CreateTexture2D(&desc, nullptr, &output))) return 7;
        nvvideo_config config{NVVIDEO_ABI, mode, 640, 360, desc.Width, desc.Height, device.Get(), context.Get(), argv[mode == 3 ? 4 : mode], argv[3],4,0,test_mode ? 2u : 0u,1,0,0,0,1.f,1.f,1.f,-1.f,1080,0,0,0}; char error[512]{};
        auto session = nvvideo_create(&config, error, sizeof(error)); if (!session) { std::fprintf(stderr, "mode=%u create: %s\n", mode, error); return 8; }
        if (!test_mode && (nvvideo_process_timestep(session,first.Get(),second.Get(),output.Get(),1,0.f,error,sizeof(error)) ||
            nvvideo_process_timestep(session,first.Get(),second.Get(),output.Get(),1,1.f,error,sizeof(error)))) {
            std::fprintf(stderr,"VFG accepted an endpoint timestep\n");nvvideo_destroy(session);return 16;
        }
        bool ok = true;
        std::vector<unsigned char> early;
        for (unsigned i = 0; i < 6; ++i) {
            const unsigned index = i % 3 + 1;
            const int ran = test_mode
                ? nvvideo_process(session, first.Get(), second.Get(), output.Get(),1, error, sizeof(error))
                : nvvideo_process_timestep(session, first.Get(), second.Get(), output.Get(), index, 0.25f * index, error, sizeof(error));
            if (!ran) { ok = false; break; }
            if (!test_mode && i == 0) early = read(device.Get(), context.Get(), output.Get());
        }
        auto result = read(device.Get(), context.Get(), output.Get()); nvvideo_destroy(session);
        if (!ok) { std::fprintf(stderr, "mode=%u process: %s\n", mode, error); return 9; }
        if (result.empty() || !std::any_of(result.begin(), result.end(), [](unsigned char value) { return value != 0; }) || (mode != 1 && (result == a || result == b))) { std::fprintf(stderr, "mode=%u output is empty, zero or equals an input\n", mode); return 10; }
        if (!test_mode && result == early) { std::fprintf(stderr, "VFG explicit timesteps produced identical outputs\n"); return 15; }
        if (!test_mode) std::puts("VFG_TIMESTEP_GPU_OK t=0.25/0.5/0.75 distinct-output");
        std::printf("NATIVE_VIDEO_GPU_OK mode=%u output=%ux%u bytes=%zu real-sdk-runs=6\n", mode, desc.Width, desc.Height, result.size());
        if (mode == 2) {
            if (!vfg_pairs(config, first.Get(), second.Get(), output.Get())) return 17;
            context->UpdateSubresource(first.Get(), 0, nullptr, a.data(), 640 * 4, 0);
            context->UpdateSubresource(second.Get(), 0, nullptr, b.data(), 640 * 4, 0);
        }
        if (mode == 3) {
            size_t changed = 0; unsigned long long difference = 0;
            for (size_t j = 0; j < result.size(); j++) if (j % 4 != 3) { const auto delta = static_cast<unsigned>(std::abs(int(result[j]) - int(b[j]))); changed += delta != 0; difference += delta; }
            if (changed < 100) { std::fprintf(stderr, "DLSSNR did not change RGB pixels\n"); return 11; }
            std::printf("DLSSNR_RGB_OUTPUT_OK changed-components=%zu mean-absolute-delta=%.4f\n", changed, double(difference) / (640 * 360 * 3));
            for (unsigned variant : {0u, 1u, 2u}) {
                config.nr_style = static_cast<int>(variant); config.nr_intensity = variant == 0 ? 0.f : 0.7f;
                config.nr_tone = 0.6f; config.nr_structure = 0.8f; config.nr_skin = 1.f; config.nr_auto_mask = config.nr_ui_correction = 1;
                session = nvvideo_create(&config, error, sizeof(error)); if (!session) { std::fprintf(stderr,"DLSSNR variant create: %s\n",error);return 12; }
                ok = true;
                for (unsigned i = 0; i < 6; i++) if (!nvvideo_process(session, first.Get(), second.Get(), output.Get(),1,error,sizeof(error))) { ok = false; break; }
                auto variant_output = read(device.Get(), context.Get(), output.Get());nvvideo_destroy(session);
                if (!ok || variant_output.size() != result.size()) { std::fprintf(stderr,"DLSSNR variant process: %s\n",error);return 13; }
                size_t parameter_changes = 0;
                for (size_t j = 0; j < result.size(); j++) if (j % 4 != 3) parameter_changes += variant_output[j] != result[j];
                if (parameter_changes < 100) { std::fprintf(stderr,"DLSSNR parameters had no RGB effect\n");return 14; }
                std::printf("DLSSNR_PARAMETER_GPU_OK style=%u intensity=%.2f changed-components=%zu runs=6\n",variant,config.nr_intensity,parameter_changes);
            }
        }
    }
    return 0;
}
