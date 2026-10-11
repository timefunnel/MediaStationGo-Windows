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
int wmain(int argc, wchar_t **argv) {
    if (argc != 4) { std::fprintf(stderr, "Usage: test_video_enhancement vsr-dir vfg-dir cache-dir\n"); return 2; }
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
    for (unsigned mode : {1u, 2u}) {
        const unsigned scale = mode == 1 ? 2u : 1u; desc.Width = 640 * scale; desc.Height = 360 * scale;
        ComPtr<ID3D11Texture2D> output; if (FAILED(device->CreateTexture2D(&desc, nullptr, &output))) return 7;
        nvvideo_config config{1, mode, 640, 360, desc.Width, desc.Height, device.Get(), context.Get(), argv[mode], argv[3]}; char error[512]{};
        auto session = nvvideo_create(&config, error, sizeof(error)); if (!session) { std::fprintf(stderr, "mode=%u create: %s\n", mode, error); return 8; }
        bool ok = true;
        for (unsigned i = 0; i < 6; ++i) if (!nvvideo_process(session, first.Get(), second.Get(), output.Get(), error, sizeof(error))) { ok = false; break; }
        auto result = read(device.Get(), context.Get(), output.Get()); nvvideo_destroy(session);
        if (!ok) { std::fprintf(stderr, "mode=%u process: %s\n", mode, error); return 9; }
        if (result.empty() || !std::any_of(result.begin(), result.end(), [](unsigned char value) { return value != 0; }) || (mode == 2 && (result == a || result == b))) { std::fprintf(stderr, "mode=%u output is empty, zero or equals an input\n", mode); return 10; }
        std::printf("NATIVE_VIDEO_GPU_OK mode=%u output=%ux%u bytes=%zu real-sdk-runs=6\n", mode, desc.Width, desc.Height, result.size());
    }
    return 0;
}
