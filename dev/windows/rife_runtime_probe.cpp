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
#include <cstdlib>
#include <cwchar>
#include <filesystem>
#include <fstream>
#include <iomanip>
#include <string>
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
using QueuePrewarmFn = int(__cdecl *)(const wchar_t *, const wchar_t *,
                                      uint32_t, uint32_t, ID3D11Device *,
                                      ID3D11DeviceContext *, char *, size_t);

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

struct SequenceOutputMetrics {
    double delta_from_frame0 = 0;
    double delta_from_frame1 = 0;
    double temporal_imbalance = 0;
    double outside_endpoints_ratio = 0;
};

enum class FrameReadResult {
    frame,
    end,
    partial,
};

size_t p010_frame_bytes(uint32_t width, uint32_t height)
{
    return static_cast<size_t>(width) * height * 3;
}

FrameReadResult read_p010_frame(std::ifstream &input,
                                std::vector<uint8_t> &frame)
{
    input.read(reinterpret_cast<char *>(frame.data()),
               static_cast<std::streamsize>(frame.size()));
    const std::streamsize bytes_read = input.gcount();
    if (bytes_read == static_cast<std::streamsize>(frame.size()))
        return FrameReadResult::frame;
    if (bytes_read == 0 && input.eof())
        return FrameReadResult::end;
    return FrameReadResult::partial;
}

bool upload_p010_frame(ID3D11DeviceContext *context,
                       ID3D11Texture2D *texture,
                       const std::vector<uint8_t> &frame,
                       uint32_t width)
{
    context->UpdateSubresource(texture, 0, nullptr, frame.data(), width * 2,
                               static_cast<UINT>(frame.size()));
    return true;
}

bool create_readback_texture(ID3D11Device *device,
                             ID3D11Texture2D *source,
                             ComPtr<ID3D11Texture2D> &readback)
{
    D3D11_TEXTURE2D_DESC desc{};
    source->GetDesc(&desc);
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    desc.MiscFlags = 0;
    return SUCCEEDED(device->CreateTexture2D(&desc, nullptr, &readback));
}

bool write_p010_texture(ID3D11DeviceContext *context,
                        ID3D11Texture2D *source,
                        ID3D11Texture2D *readback,
                        const std::vector<uint8_t> &frame0,
                        const std::vector<uint8_t> &frame1,
                        uint32_t width,
                        uint32_t height,
                        std::ofstream &output,
                        SequenceOutputMetrics &metrics)
{
    context->CopyResource(readback, source);
    D3D11_MAPPED_SUBRESOURCE mapped{};
    if (FAILED(context->Map(readback, 0, D3D11_MAP_READ, 0, &mapped)))
        return false;

    const size_t row_bytes = static_cast<size_t>(width) * 2;
    const uint32_t rows = height + height / 2;
    for (uint32_t row = 0; row < rows; ++row) {
        const auto *mapped_row = static_cast<const uint8_t *>(mapped.pData)
                               + static_cast<size_t>(mapped.RowPitch) * row;
        output.write(reinterpret_cast<const char *>(mapped_row),
                     static_cast<std::streamsize>(row_bytes));
    }

    double delta0 = 0;
    double delta1 = 0;
    uint64_t outside = 0;
    const uint64_t luma_samples = static_cast<uint64_t>(width) * height;
    const auto *input0 = reinterpret_cast<const uint16_t *>(frame0.data());
    const auto *input1 = reinterpret_cast<const uint16_t *>(frame1.data());
    for (uint32_t y = 0; y < height; ++y) {
        const auto *output_row = reinterpret_cast<const uint16_t *>(
            static_cast<const uint8_t *>(mapped.pData)
            + static_cast<size_t>(mapped.RowPitch) * y);
        const size_t row_offset = static_cast<size_t>(y) * width;
        for (uint32_t x = 0; x < width; ++x) {
            const uint16_t value0 = input0[row_offset + x] >> 6;
            const uint16_t value1 = input1[row_offset + x] >> 6;
            const uint16_t value = output_row[x] >> 6;
            delta0 += std::abs(static_cast<int>(value)
                               - static_cast<int>(value0));
            delta1 += std::abs(static_cast<int>(value)
                               - static_cast<int>(value1));
            const uint16_t minimum = std::min(value0, value1);
            const uint16_t maximum = std::max(value0, value1);
            if (value + 2 < minimum || value > maximum + 2)
                outside++;
        }
    }
    context->Unmap(readback, 0);
    if (!output)
        return false;

    metrics.delta_from_frame0 = delta0 / luma_samples;
    metrics.delta_from_frame1 = delta1 / luma_samples;
    const double total_delta = metrics.delta_from_frame0
                             + metrics.delta_from_frame1;
    metrics.temporal_imbalance = total_delta > 0
        ? std::abs(metrics.delta_from_frame0 - metrics.delta_from_frame1)
          / total_delta
        : 0;
    metrics.outside_endpoints_ratio = static_cast<double>(outside)
                                    / luma_samples;
    return true;
}

const char *scene_class_name(uint32_t classification)
{
    switch (classification) {
    case RIFE_SCENE_NORMAL: return "normal";
    case RIFE_SCENE_FLASH: return "flash";
    case RIFE_SCENE_FADE_DISSOLVE: return "fade-dissolve";
    case RIFE_SCENE_HARD_CUT: return "hard-cut";
    case RIFE_SCENE_UNCERTAIN: return "uncertain";
    default: return "invalid";
    }
}

int run_sequence_probe(rife_runtime *runtime,
                       ProcessFn process,
                       GetStatsFn get_stats,
                       ID3D11Device *device,
                       ID3D11DeviceContext *context,
                       ID3D11Texture2D *frame0,
                       ID3D11Texture2D *frame1,
                       ID3D11Texture2D *output_texture,
                       uint32_t width,
                       uint32_t height,
                       uint32_t fps_numerator,
                       uint32_t fps_denominator,
                       double start_pts,
                       const wchar_t *input_path,
                       const wchar_t *output_path,
                       const wchar_t *csv_path)
{
    std::ifstream input(std::filesystem::path(input_path), std::ios::binary);
    std::ofstream output(std::filesystem::path(output_path),
                         std::ios::binary | std::ios::trunc);
    std::ofstream csv(std::filesystem::path(csv_path), std::ios::trunc);
    if (!input || !output || !csv) {
        std::fprintf(stderr, "RIFE_SEQUENCE_FILE_OPEN_FAILED\n");
        return 11;
    }

    const size_t frame_bytes = p010_frame_bytes(width, height);
    std::vector<uint8_t> bytes0(frame_bytes);
    std::vector<uint8_t> bytes1(frame_bytes);
    if (read_p010_frame(input, bytes0) != FrameReadResult::frame) {
        std::fprintf(stderr, "RIFE_SEQUENCE_FIRST_FRAME_MISSING\n");
        return 12;
    }
    ComPtr<ID3D11Texture2D> readback;
    if (!create_readback_texture(device, output_texture, readback)) {
        std::fprintf(stderr, "RIFE_SEQUENCE_READBACK_CREATE_FAILED\n");
        return 13;
    }

    csv << "pair,source0_pts,source1_pts,midpoint_pts,class,policy,"
           "average_delta,changed_ratio,average_kl,regional_kl_max,"
           "chroma_delta,edge_delta,exposure_delta,exposure_spread,"
           "scene_ms,inference_ms,output_delta_f0,output_delta_f1,"
           "temporal_imbalance,outside_endpoints_ratio\n";
    csv << std::fixed << std::setprecision(6);

    char error[1024]{};
    uint64_t pairs = 0;
    while (true) {
        const FrameReadResult read = read_p010_frame(input, bytes1);
        if (read == FrameReadResult::end)
            break;
        if (read == FrameReadResult::partial) {
            std::fprintf(stderr,
                         "RIFE_SEQUENCE_PARTIAL_FRAME pair=%llu\n",
                         static_cast<unsigned long long>(pairs));
            return 14;
        }
        upload_p010_frame(context, frame0, bytes0, width);
        upload_p010_frame(context, frame1, bytes1, width);
        const double source0_pts = start_pts
            + static_cast<double>(pairs) * fps_denominator / fps_numerator;
        const double source1_pts = start_pts
            + static_cast<double>(pairs + 1) * fps_denominator / fps_numerator;
        rife_frame_diagnostics diagnostics{};
        const int status = process(runtime, frame0, 0, frame1, 0,
                                   output_texture, 0, source0_pts,
                                   source1_pts, &diagnostics, error,
                                   sizeof(error));
        if (status != RIFE_RUNTIME_OK) {
            std::fprintf(stderr,
                         "RIFE_SEQUENCE_PROCESS_FAILED pair=%llu status=%d "
                         "detail=%s\n",
                         static_cast<unsigned long long>(pairs), status,
                         error);
            return 15;
        }
        SequenceOutputMetrics output_metrics;
        if (!write_p010_texture(context, output_texture, readback.Get(),
                                bytes0, bytes1, width, height, output,
                                output_metrics)) {
            std::fprintf(stderr,
                         "RIFE_SEQUENCE_READBACK_FAILED pair=%llu\n",
                         static_cast<unsigned long long>(pairs));
            return 16;
        }
        csv << pairs << ',' << diagnostics.source0_pts << ','
            << diagnostics.source1_pts << ',' << diagnostics.midpoint_pts
            << ',' << scene_class_name(diagnostics.classification) << ','
            << (diagnostics.scene_cut ? "copy-f0-hard-cut"
                                      : "rife-midpoint")
            << ',' << diagnostics.average_delta << ','
            << diagnostics.changed_ratio << ',' << diagnostics.average_kl
            << ',' << diagnostics.regional_kl_max << ','
            << diagnostics.chroma_delta << ',' << diagnostics.edge_delta
            << ',' << diagnostics.exposure_delta << ','
            << diagnostics.exposure_spread << ',' << diagnostics.scene_ms
            << ',' << diagnostics.inference_ms << ','
            << output_metrics.delta_from_frame0 << ','
            << output_metrics.delta_from_frame1 << ','
            << output_metrics.temporal_imbalance << ','
            << output_metrics.outside_endpoints_ratio << '\n';
        if (!csv) {
            std::fprintf(stderr,
                         "RIFE_SEQUENCE_CSV_WRITE_FAILED pair=%llu\n",
                         static_cast<unsigned long long>(pairs));
            return 17;
        }
        pairs++;
        bytes0.swap(bytes1);
    }
    output.flush();
    csv.flush();
    if (pairs == 0 || !output || !csv) {
        std::fprintf(stderr, "RIFE_SEQUENCE_OUTPUT_INCOMPLETE\n");
        return 19;
    }
    rife_runtime_stats stats{};
    if (get_stats(runtime, &stats) != RIFE_RUNTIME_OK) {
        std::fprintf(stderr, "RIFE_SEQUENCE_STATS_FAILED\n");
        return 20;
    }
    std::printf(
        "RIFE_SEQUENCE_OK frames=%llu pairs=%llu inferred=%llu cuts=%llu "
        "failures=%llu inference-p95=%.3fms scene-max=%.3fms\n",
        static_cast<unsigned long long>(pairs + 1),
        static_cast<unsigned long long>(pairs),
        static_cast<unsigned long long>(stats.inferred_pairs),
        static_cast<unsigned long long>(stats.scene_cuts),
        static_cast<unsigned long long>(stats.failures),
        stats.inference_p95_ms, stats.scene_max_ms);
    return 0;
}

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
    const bool sequence_mode = argc == 13
        && std::wcscmp(argv[1], L"--sequence") == 0;
    if (argc != 8 && !sequence_mode) {
        std::fprintf(stderr,
                     "usage: rife_runtime_probe.exe <runtime-dll> <engine> "
                     "<cudart> <width> <height> <warmup> <iterations>\n"
                     "   or: rife_runtime_probe.exe --sequence "
                     "<runtime-dll> <engine> <cudart> <width> <height> "
                     "<fps-num> <fps-den> <start-pts> <input-p010> "
                     "<midpoints-p010> <diagnostics-csv>\n");
        return 2;
    }
    const int runtime_argument = sequence_mode ? 2 : 1;
    const int engine_argument = sequence_mode ? 3 : 2;
    const int cudart_argument = sequence_mode ? 4 : 3;
    const int width_argument = sequence_mode ? 5 : 4;
    const int height_argument = sequence_mode ? 6 : 5;
    const uint32_t width = static_cast<uint32_t>(
        _wtoi(argv[width_argument]));
    const uint32_t height = static_cast<uint32_t>(
        _wtoi(argv[height_argument]));
    const int warmup = sequence_mode ? 0 : _wtoi(argv[6]);
    const int iterations = sequence_mode ? 0 : _wtoi(argv[7]);
    const uint32_t fps_numerator = sequence_mode
        ? static_cast<uint32_t>(_wtoi(argv[7])) : 0;
    const uint32_t fps_denominator = sequence_mode
        ? static_cast<uint32_t>(_wtoi(argv[8])) : 0;
    const double start_pts = sequence_mode ? std::wcstod(argv[9], nullptr)
                                           : 0;
    if (!width || !height
        || (sequence_mode
            ? !fps_numerator || !fps_denominator || start_pts < 0
            : warmup < 1 || iterations < 1))
        return 2;

    HMODULE module = LoadLibraryExW(
        argv[runtime_argument], nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
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
    QueuePrewarmFn queue_prewarm = nullptr;
    if (!load_symbol(module, "rife_runtime_abi_version", abi_version)
        || !load_symbol(module, "rife_runtime_create", create)
        || !load_symbol(module, "rife_runtime_process", process)
        || !load_symbol(module, "rife_runtime_get_stats", get_stats)
        || !load_symbol(module, "rife_runtime_destroy", destroy)
        || !load_symbol(module, "rife_runtime_queue_prewarm_with_device",
                        queue_prewarm)) {
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

    char prewarm_error[1024]{};
    const auto prewarm_started = std::chrono::steady_clock::now();
    const int prewarm_status = queue_prewarm(
        argv[engine_argument], argv[cudart_argument], width, height,
        device.Get(), context.Get(), prewarm_error, sizeof(prewarm_error));
    const double prewarm_ms = std::chrono::duration<double, std::milli>(
        std::chrono::steady_clock::now() - prewarm_started).count();
    if (prewarm_status != RIFE_RUNTIME_OK) {
        std::fprintf(stderr,
                     "RIFE_RUNTIME_PROBE_PREWARM_FAILED status=%d detail=%s\n",
                     prewarm_status, prewarm_error);
        FreeLibrary(module);
        return 8;
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
        return 9;
    }

    const rife_runtime_config config{
        RIFE_RUNTIME_ABI_VERSION,
        device.Get(),
        context.Get(),
        argv[engine_argument],
        argv[cudart_argument],
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
        return 10;
    }

    if (sequence_mode) {
        const int result = run_sequence_probe(
            runtime, process, get_stats, device.Get(), context.Get(),
            frame0.Get(), frame1.Get(), output.Get(), width, height,
            fps_numerator, fps_denominator, start_pts, argv[10], argv[11],
            argv[12]);
        destroy(runtime);
        FreeLibrary(module);
        return result;
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
        return 11;
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
        "prewarm=%.2fms cache-reopen=%s cold-init=%.2fms cached-init=%.2fms "
        "stages=cuda:%.2f,bind:%.2f,read:%.2f,trt:%.2f,deserialize:%.2f,"
        "context:%.2f,validate:%.2f,d3d:%.2f "
        "scene-class=%u average-delta=%.3f changed-ratio=%.4f "
        "average-kl=%.5f "
        "unique-luma=%u non-8bit-luma=%llu p010-packed=%s\n",
        adapter_name.c_str(), width, height, warmup, iterations, 1000.0 / mean,
        mean, p95, stats.inference_p95_ms,
        stats.pairs ? stats.scene_total_ms / stats.pairs : 0,
        static_cast<unsigned long long>(stats.inferred_pairs),
        static_cast<unsigned long long>(stats.scene_cuts),
        prewarm_ms,
        cached_stats.runtime_cache_hit ? "hit" : "miss",
        stats.runtime_initialization_ms,
        cached_stats.runtime_initialization_ms,
        stats.runtime_cuda_load_ms,
        stats.runtime_cuda_bind_ms,
        stats.runtime_engine_read_ms,
        stats.runtime_trt_runtime_ms,
        stats.runtime_engine_deserialize_ms,
        stats.runtime_execution_context_ms,
        stats.runtime_engine_validate_ms,
        stats.runtime_d3d_resources_ms,
        diagnostics.classification, diagnostics.average_delta,
        diagnostics.changed_ratio, diagnostics.average_kl,
        validation.unique_luma_codes,
        static_cast<unsigned long long>(validation.non_8bit_luma_samples),
        validation.p010_packed ? "yes" : "no");
    return 0;
}
