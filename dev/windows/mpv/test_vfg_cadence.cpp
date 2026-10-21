// Visible, local-media benchmark. Presentation timestamps come from a separate
// DXGI/PresentMon trace; polling time-pos is only for progress and A/V telemetry.
#define NOMINMAX
#include <windows.h>
#include <mpv/client.h>
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <functional>
#include <string>
#include <utility>
#include <vector>

static std::string utf8(const wchar_t *value)
{
    int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, -1,
                                  nullptr, 0, nullptr, nullptr);
    if (!size) return {};
    std::string result(size, '\0');
    if (!WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, -1,
                             result.data(), size, nullptr, nullptr)) return {};
    result.pop_back();
    return result;
}

static std::string quote(const std::string &text)
{
    return "%" + std::to_string(text.size()) + "%" + text;
}

static double number(mpv_handle *mpv, const char *name)
{
    double value = NAN;
    mpv_get_property(mpv, name, MPV_FORMAT_DOUBLE, &value);
    return value;
}

static double meta(mpv_handle *mpv, const char *field)
{
    std::string name = std::string("vf-metadata/enhance_vfg/") + field;
    char *text = mpv_get_property_string(mpv, name.c_str());
    double value = text ? std::strtod(text, nullptr) : NAN;
    mpv_free(text);
    return value;
}

static double percentile(std::vector<double> values, double fraction)
{
    if (values.empty()) return NAN;
    std::sort(values.begin(), values.end());
    return values[static_cast<size_t>(std::ceil(fraction * values.size())) - 1];
}

static LRESULT CALLBACK window_proc(HWND window, UINT message, WPARAM w, LPARAM l)
{
    if (message == WM_CLOSE) { PostQuitMessage(0); return 0; }
    return DefWindowProcW(window, message, w, l);
}

static bool wait_for(mpv_handle *mpv, const std::function<bool()> &ready, double seconds = 10)
{
    const auto deadline = std::chrono::steady_clock::now() +
        std::chrono::milliseconds(static_cast<int>(seconds * 1000));
    while (std::chrono::steady_clock::now() < deadline) {
        MSG message;
        while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
            if (message.message == WM_QUIT) return false;
            TranslateMessage(&message); DispatchMessageW(&message);
        }
        const auto *event = mpv_wait_event(mpv, .01);
        if (event->event_id == MPV_EVENT_LOG_MESSAGE) {
            const auto *log = static_cast<mpv_event_log_message *>(event->data);
            std::fprintf(stderr, "%s: %s", log->prefix, log->text);
        }
        if (event->event_id == MPV_EVENT_END_FILE &&
            static_cast<mpv_event_end_file *>(event->data)->reason == MPV_END_FILE_REASON_ERROR)
            return false;
        if (ready()) return true;
    }
    return false;
}

static bool property_is(mpv_handle *mpv, const char *name, const char *expected)
{
    char *value = mpv_get_property_string(mpv, name);
    bool matches = value && std::string(value) == expected;
    mpv_free(value);
    return matches;
}

static bool exercise_controls(mpv_handle *mpv, const std::string &filename,
                              const std::string &sync)
{
    if (mpv_set_property_string(mpv, "pause", "yes") < 0) return false;
    const double paused_at = number(mpv, "time-pos");
    const auto paused_until = std::chrono::steady_clock::now() + std::chrono::milliseconds(300);
    if (!wait_for(mpv, [&] { return std::chrono::steady_clock::now() >= paused_until; }) ||
        std::abs(number(mpv, "time-pos") - paused_at) > .001) return false;
    const char *seek[] = {"seek", "1", "absolute+exact", nullptr};
    if (mpv_command(mpv, seek) < 0 || !wait_for(mpv, [&] {
        return property_is(mpv, "seeking", "no") &&
            property_is(mpv, "pause", "yes") && std::abs(number(mpv, "time-pos") - 1) < .1;
    })) return false;
    std::puts("VFG_CONTROL_OK paused-exact-seek");
    if (mpv_set_property_string(mpv, "pause", "no") < 0 || !wait_for(mpv, [&] {
        return number(mpv, "time-pos") > 1.5 && property_is(mpv, "aid", "1") &&
            property_is(mpv, "sid", "1") && property_is(mpv, "sub-text", "VFG cadence comparison - original subtitle timeline");
    })) return false;
    const double before_switch = number(mpv, "time-pos");
    if (mpv_set_property_string(mpv, "aid", "2") < 0 || !wait_for(mpv, [&] {
        return property_is(mpv, "aid", "2") && number(mpv, "time-pos") > before_switch + .5 &&
            (sync != "display-vdrop" || property_is(mpv, "display-sync-active", "yes"));
    })) return false;
    std::puts("VFG_CONTROL_OK audio-switch-progress");
    if (mpv_set_property_string(mpv, "sid", "no") < 0 || !wait_for(mpv, [&] {
        char *text = mpv_get_property_string(mpv, "sub-text");
        // mpv makes sub-text unavailable when no subtitle track is selected.
        const bool empty = !text || !*text;
        mpv_free(text);
        return property_is(mpv, "sid", "no") && empty;
    })) return false;
    const std::string subtitle = filename.substr(0, filename.find_last_of("/\\") + 1) + "cadence-timed.srt";
    const char *add[] = {"sub-add", subtitle.c_str(), "select", nullptr};
    if (mpv_command(mpv, add) < 0 || !wait_for(mpv, [&] {
        const double position = number(mpv, "time-pos");
        return property_is(mpv, "sid", "2") && position >= 2.3 && position <= 3.8 &&
            property_is(mpv, "sub-text", "VFG timed subtitle B");
    })) return false;
    if (mpv_set_property_string(mpv, "pause", "yes") < 0) return false;
    const char *subtitle_seek[] = {"seek", "1.5", "absolute+exact", nullptr};
    if (mpv_command(mpv, subtitle_seek) < 0 || !wait_for(mpv, [&] {
        return property_is(mpv, "seeking", "no") &&
            std::abs(number(mpv, "time-pos") - 1.5) < .1 &&
            property_is(mpv, "sub-text", "VFG timed subtitle A");
    })) return false;
    if (mpv_set_property_string(mpv, "pause", "no") < 0 || !wait_for(mpv, [&] {
        const double position = number(mpv, "time-pos");
        char *text = mpv_get_property_string(mpv, "sub-text");
        const bool empty = !text || !*text;
        mpv_free(text);
        return position >= 3.9 && position < 4.3 && empty;
    })) return false;
    std::puts("VFG_CONTROL_OK subtitle-disable-external-select/timed-cues/paused-seek/clear-after-end");
    const std::string end_position = std::to_string(number(mpv, "duration") - .5);
    const char *end_seek[] = {"seek", end_position.c_str(), "absolute+exact", nullptr};
    if (mpv_command(mpv, end_seek) < 0 || !wait_for(mpv, [&] {
        return property_is(mpv, "idle-active", "yes");
    })) return false;
    std::puts("VFG_CONTROL_OK filtered-eof");
    // Clear the VFG chain for the next file. The sync override must restore
    // itself even though no video-sync option is supplied on this load.
    const char *plain[] = {"loadfile", filename.c_str(), "replace", "-1", "vf=,start=0", nullptr};
    if (mpv_command(mpv, plain) < 0 || !wait_for(mpv, [&] {
        return number(mpv, "time-pos") > .5 && number(mpv, "time-pos") < 1.5 &&
            property_is(mpv, "video-sync", "audio") && !std::isfinite(meta(mpv, "target-fps"));
    })) return false;
    std::puts("VFG_CONTROL_OK next-file-restores-configured-audio-sync");
    if (mpv_command(mpv, end_seek) < 0 || !wait_for(mpv, [&] {
        return property_is(mpv, "idle-active", "yes");
    })) return false;
    std::puts("VFG_CONTROL_OK eof");
    return true;
}

int wmain(int argc, wchar_t **argv)
{
    if (argc != 11) return 2;
    const int divisor = _wtoi(argv[5]), quality = _wtoi(argv[6]);
    const double seconds = _wtof(argv[7]);
    if (divisor < 1 || quality < 0 || quality > 2 || seconds < 1) return 2;
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    WNDCLASSW cls = {};
    cls.lpfnWndProc = window_proc;
    cls.hInstance = GetModuleHandleW(nullptr);
    cls.lpszClassName = L"VfgCadenceBenchmark";
    cls.hCursor = LoadCursorW(nullptr, MAKEINTRESOURCEW(32512));
    if (!RegisterClassW(&cls)) return 3;
    std::wstring title = L"VFG cadence comparison - display / " + std::to_wstring(divisor);
    HWND window = CreateWindowExW(0, cls.lpszClassName, title.c_str(), WS_OVERLAPPEDWINDOW,
        100, 100, 1920, 1080, nullptr, nullptr, cls.hInstance, nullptr);
    if (!window) return 3;
    ShowWindow(window, SW_SHOW);
    UpdateWindow(window);

    mpv_handle *mpv = mpv_create();
    if (!mpv) return 4;
    struct Cleanup {
        mpv_handle *mpv;
        HWND window;
        ~Cleanup() { mpv_terminate_destroy(mpv); DestroyWindow(window); }
    } cleanup{mpv, window};
    int64_t wid = reinterpret_cast<int64_t>(window);
    if (mpv_set_option(mpv, "wid", MPV_FORMAT_INT64, &wid) < 0) return 5;
    for (const auto &option : {
        std::pair{"config", "no"}, {"load-scripts", "no"}, {"vo", "gpu-next"},
        {"gpu-api", "d3d11"}, {"gpu-context", "d3d11"}, {"hwdec", "d3d11va"},
        {"ao", "wasapi"}, {"volume", "0"}, {"terminal", "no"},
        {"input-default-bindings", "no"}, {"sid", "1"}, {"keep-open", "no"}, {"idle", "yes"}}) {
        if (mpv_set_option_string(mpv, option.first, option.second) < 0) return 6;
    }
    const std::string sync = utf8(argv[9]);
    const std::string raw_log = utf8(argv[8]) + "-mpv.log";
    if (mpv_set_option_string(mpv, "log-file", raw_log.c_str()) < 0) return 6;
    // Match the application: VFG overrides sync per file, leaving the configured
    // mode intact for subsequent plain/RIFE playback.
    if (mpv_set_option_string(mpv, "video-sync", "audio") < 0) return 6;
    const std::string filter = "@nvconvert:d3d11vpp=format=bgra,"
        "@enhance_vfg:nvvideo=mode=2:multiplier=0:display-divisor=" +
        std::to_string(divisor) + ":quality=" + std::to_string(quality) +
        ":bridge=" + quote(utf8(argv[2])) + ":runtime=" + quote(utf8(argv[3])) +
        ":cache=" + quote(utf8(argv[4])) + ",@nvoutput:d3d11vpp=format=nv12";
    if (mpv_set_option_string(mpv, "vf", filter.c_str()) < 0 || mpv_initialize(mpv) < 0)
        return 7;
    mpv_request_log_messages(mpv, "info");
    const std::string filename = utf8(argv[1]);
    const std::string load_options = "video-sync=" + sync;
    const char *load[] = {"loadfile", filename.c_str(), "replace", "-1", load_options.c_str(), nullptr};
    if (mpv_command(mpv, load) < 0) return 8;
    FILE *csv = nullptr;
    std::wstring prefix = argv[8];
    if (_wfopen_s(&csv, (prefix + L"-telemetry.csv").c_str(), L"wb") || !csv) return 9;
    std::fprintf(csv, "QpcTimeMs,PositionSeconds,AVSyncMs,FrameDrops,DecoderDrops,MistimedFrames,DelayedFrames,DisplayHz,EstimatedDisplayHz,VsyncJitter,TargetFps,SDKMeanMs,SDKMaxMs\n");
    const char *counters[] = {"frame-drop-count", "decoder-frame-drop-count",
                              "mistimed-frame-count", "vo-delayed-frame-count"};
    double baseline[4] = {}, final[4] = {};
    std::vector<double> av_samples;
    bool measuring = false, finished = false, failed = false;
    bool audio_verified = false, subtitle_seen = false;
    double last_pos = 0, next_sample = 0;
    double start_qpc_ms = 0, end_qpc_ms = 0;
    double audio_speed_error = 0, video_speed_error = 0;
    unsigned sync_samples = 0, sync_inactive_samples = 0;
    LARGE_INTEGER qpc_frequency;
    QueryPerformanceFrequency(&qpc_frequency);
    unsigned reconfigs = 0;
    auto next_poll = std::chrono::steady_clock::now();
    const auto deadline = std::chrono::steady_clock::now() +
        std::chrono::seconds(static_cast<int>(seconds) + 50);
    while (std::chrono::steady_clock::now() < deadline) {
        MSG message;
        while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
            if (message.message == WM_QUIT) { failed = true; break; }
            TranslateMessage(&message); DispatchMessageW(&message);
        }
        if (failed) break;
        const auto *event = mpv_wait_event(mpv, 0.01);
        if (event->event_id == MPV_EVENT_LOG_MESSAGE) {
            const auto *log = static_cast<mpv_event_log_message *>(event->data);
            if (std::string(log->prefix) == "cplayer" && std::string(log->text).find("VO: [") == 0)
                reconfigs++;
            if (std::string(log->text).find("VFG_DXGI_STATS") != 0 &&
                std::string(log->text).find("VFG_CONTENT_PRESENT") != 0)
                std::fprintf(stderr, "%s: %s", log->prefix, log->text);
        }
        if (event->event_id == MPV_EVENT_QUEUE_OVERFLOW) {
            std::fprintf(stderr, "Measurement event queue overflow\n");
            failed = true; break;
        }
        if (event->event_id == MPV_EVENT_END_FILE) {
            const auto *end = static_cast<mpv_event_end_file *>(event->data);
            if (end->reason == MPV_END_FILE_REASON_ERROR) failed = true;
            break;
        }
        if (std::chrono::steady_clock::now() < next_poll) continue;
        next_poll = std::chrono::steady_clock::now() + std::chrono::milliseconds(10);
        double pos = number(mpv, "time-pos");
        if (!std::isfinite(pos)) continue;
        last_pos = pos;
        if (!audio_verified && pos > 1) {
            char *ao = mpv_get_property_string(mpv, "current-ao");
            audio_verified = ao && std::string(ao) == "wasapi";
            mpv_free(ao);
            if (!audio_verified) { failed = true; break; }
        }
        if (pos > 2 && !subtitle_seen) {
            char *text = mpv_get_property_string(mpv, "sub-text");
            subtitle_seen = text && std::string(text).find("VFG cadence comparison") != std::string::npos;
            mpv_free(text);
        }
        if (!measuring && pos >= 5) {
            for (unsigned i = 0; i < 4; i++) baseline[i] = number(mpv, counters[i]);
            LARGE_INTEGER now; QueryPerformanceCounter(&now);
            start_qpc_ms = now.QuadPart * 1000.0 / qpc_frequency.QuadPart;
            measuring = true;
        }
        if (measuring && pos >= next_sample) {
            int active = 0;
            if (mpv_get_property(mpv, "display-sync-active", MPV_FORMAT_FLAG, &active) < 0)
                return 11;
            sync_samples++;
            if (!active) sync_inactive_samples++;
            const double audio_speed = number(mpv, "audio-speed-correction");
            const double video_speed = number(mpv, "video-speed-correction");
            if (!std::isfinite(audio_speed) || !std::isfinite(video_speed)) return 11;
            audio_speed_error = std::max(audio_speed_error, std::abs(audio_speed - 1));
            video_speed_error = std::max(video_speed_error, std::abs(video_speed - 1));
            double av = number(mpv, "avsync");
            for (unsigned i = 0; i < 4; i++) final[i] = number(mpv, counters[i]);
            if (std::isfinite(av)) av_samples.push_back(std::abs(av) * 1000);
            LARGE_INTEGER now; QueryPerformanceCounter(&now);
            end_qpc_ms = now.QuadPart * 1000.0 / qpc_frequency.QuadPart;
            std::fprintf(csv, "%.6f,%.6f,%.6f,%.0f,%.0f,%.0f,%.0f,%.6f,%.6f,%.6f,%.9f,%.6f,%.6f\n",
                end_qpc_ms, pos, av * 1000, final[0], final[1], final[2], final[3],
                number(mpv, "display-fps"), number(mpv, "estimated-display-fps"),
                number(mpv, "vsync-jitter"), meta(mpv, "target-fps"),
                meta(mpv, "avg-process-ms"), meta(mpv, "max-process-ms"));
            next_sample = pos + 0.02;
        }
        if (pos >= seconds + 5) { finished = true; break; }
    }
    std::fclose(csv);
    double target = meta(mpv, "target-fps"), display = meta(mpv, "display-fps");
    if (!finished || failed || av_samples.empty() || !audio_verified || !subtitle_seen ||
        (sync == "display-vdrop" && sync_inactive_samples > 0) ||
        audio_speed_error > .000001 || video_speed_error > .000001 ||
        !std::isfinite(target) || !std::isfinite(display) ||
        std::abs(target * divisor - display) > .00001) {
        std::fprintf(stderr, "CADENCE_MEASUREMENT_FAILED finished=%d failed=%d pos=%.3f audio=%d subtitle=%d target=%.6f\n",
            finished, failed, last_pos, audio_verified, subtitle_seen, target);
        return 10;
    }
    FILE *json = nullptr;
    if (_wfopen_s(&json, (prefix + L"-summary.json").c_str(), L"wb") || !json) return 9;
    // mpv exposes mistimed-frame-count only in display-sync mode. Keep the
    // missing metric explicit instead of treating it as zero in audio mode.
    const std::string mistimed = std::isfinite(final[2]) && std::isfinite(baseline[2])
        ? std::to_string(final[2] - baseline[2]) : "null";
    std::fprintf(json, "{\n\"processId\":%lu,\"divisor\":%d,\"quality\":%d,\"seconds\":%.6f,"
        "\"startQpcMs\":%.6f,\"endQpcMs\":%.6f,"
        "\"displayHz\":%.9f,\"targetFps\":%.9f,\"sourceFps\":%.9f,"
        "\"sdkMeanMs\":%.6f,\"sdkMaxMs\":%.6f,\"avP95Ms\":%.6f,\"avP99Ms\":%.6f,\"avMaxMs\":%.6f,"
        "\"frameDrops\":%.0f,\"decoderDrops\":%.0f,\"mistimedFrames\":%s,\"delayedFrames\":%.0f,"
        "\"voReconfigs\":%u,\"audio\":\"wasapi-muted\",\"subtitleVerified\":true,\"videoSync\":\"%s\","
        "\"syncSamples\":%u,\"syncInactiveSamples\":%u,\"maxAudioSpeedError\":%.9f,\"maxVideoSpeedError\":%.9f,"
        "\"presentationTiming\":\"requires-separate-DXGI-content-or-PresentMon-trace\"\n}\n",
        GetCurrentProcessId(), divisor, quality, last_pos - 5, start_qpc_ms, end_qpc_ms,
        display, target, meta(mpv, "source-fps"),
        meta(mpv, "avg-process-ms"), meta(mpv, "max-process-ms"), percentile(av_samples, .95),
        percentile(av_samples, .99), *std::max_element(av_samples.begin(), av_samples.end()),
        final[0] - baseline[0], final[1] - baseline[1], mistimed.c_str(),
        final[3] - baseline[3], reconfigs, sync.c_str(), sync_samples,
        sync_inactive_samples, audio_speed_error, video_speed_error);
    std::fclose(json);
    if (_wtoi(argv[10]) && !exercise_controls(mpv, filename, sync)) {
        std::fprintf(stderr, "VFG_CONTROL_FAILED pos=%.6f\n", number(mpv, "time-pos"));
        return 12;
    }
    std::printf("CADENCE_MEASUREMENT_COMPLETE divisor=%d target=%.6f av-p99-ms=%.3f drops=%.0f\n",
        divisor, target, percentile(av_samples, .99), final[0] - baseline[0]);
    return 0;
}
