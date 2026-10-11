// Headless native-window mpv playback test. Runs only against generated local media.
#include <windows.h>
#include <mpv/client.h>
#include <chrono>
#include <cstdio>
#include <string>
#include <cstdlib>
static std::string utf8(const wchar_t *value) {
    int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, -1, nullptr, 0, nullptr, nullptr);
    std::string text(size, '\0'); WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, -1, text.data(), size, nullptr, nullptr); text.pop_back(); return text;
}
static std::string quote(const std::string &text) { return "%" + std::to_string(text.size()) + "%" + text; }
static unsigned count(mpv_handle *mpv, const char *name) { char *value = mpv_get_property_string(mpv, name); unsigned result = value ? static_cast<unsigned>(std::strtoul(value, nullptr, 10)) : 0; mpv_free(value); return result; }
int wmain(int argc, wchar_t **argv) {
    if (argc != 6) return 2; // source, bridge, VSR dir, VFG dir, cache
    HWND window = CreateWindowExW(0, L"STATIC", L"MediaStationGo native video test", WS_POPUP, 0, 0, 640, 360, nullptr, nullptr, GetModuleHandleW(nullptr), nullptr);
    if (!window) return 3; // Never shown. The D3D11 output context still uses this native surface.
    for (unsigned mode : {1u, 2u, 3u}) {
        mpv_handle *mpv = mpv_create(); if (!mpv) return 4;
        int64_t wid = reinterpret_cast<int64_t>(window);
        if (mpv_set_option(mpv, "wid", MPV_FORMAT_INT64, &wid) < 0) return 5;
        for (auto pair : {std::pair{"vo", "gpu-next"}, {"gpu-api", "d3d11"}, {"gpu-context", "d3d11"}, {"hwdec", "d3d11va"}, {"ao", "null"}, {"terminal", "no"}, {"keep-open", "yes"}, {"input-default-bindings", "no"}}) if (mpv_set_option_string(mpv, pair.first, pair.second) < 0) return 6;
        const auto bridge = quote(utf8(argv[2])), cache = quote(utf8(argv[5]));
        std::string filter = "@nvconvert:d3d11vpp=format=bgra";
        if (mode & 2) filter += ",@enhance_vfg:nvvideo=mode=2:bridge=" + bridge + ":runtime=" + quote(utf8(argv[4])) + ":cache=" + cache;
        if (mode & 1) filter += ",@enhance_vsr:nvvideo=mode=1:bridge=" + bridge + ":runtime=" + quote(utf8(argv[3])) + ":cache=" + cache;
        filter += ",@nvoutput:d3d11vpp=format=nv12";
        if (mpv_set_option_string(mpv, "vf", filter.c_str()) < 0 || mpv_initialize(mpv) < 0) return 7;
        mpv_request_log_messages(mpv, "warn"); auto filename = utf8(argv[1]); const char *load[] = {"loadfile", filename.c_str(), nullptr};
        if (mpv_command(mpv, load) < 0) return 8;
        auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(35);
        bool seeked = false, finished = false; unsigned vsr = 0, vfg = 0, resets = 0;
        while (std::chrono::steady_clock::now() < deadline) {
            MSG message; while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) { TranslateMessage(&message); DispatchMessageW(&message); }
            auto event = mpv_wait_event(mpv, 0.02);
            if (event->event_id == MPV_EVENT_LOG_MESSAGE) { auto log = static_cast<mpv_event_log_message *>(event->data); std::fprintf(stderr, "%s: %s", log->prefix, log->text); }
            vsr = count(mpv, "vf-metadata/enhance_vsr/sdk-frames"); vfg = count(mpv, "vf-metadata/enhance_vfg/sdk-frames");
            resets = count(mpv, mode & 2 ? "vf-metadata/enhance_vfg/resets" : "vf-metadata/enhance_vsr/resets");
            if (!seeked && (vsr >= 12 || vfg >= 12)) { const char *seek[] = {"seek", "1", "absolute+exact", nullptr}; if (mpv_command(mpv, seek) < 0) return 9; seeked = true; }
            int eof = 0; if (mpv_get_property(mpv, "eof-reached", MPV_FORMAT_FLAG, &eof) >= 0 && eof) { finished = true; break; }
            if (event->event_id == MPV_EVENT_END_FILE) { auto end = static_cast<mpv_event_end_file *>(event->data); if (end->reason == MPV_END_FILE_REASON_ERROR) { std::fprintf(stderr, "mpv end-file error %d\n", end->error); break; } }
        }
        int64_t width = 0, height = 0; mpv_get_property(mpv, "video-out-params/w", MPV_FORMAT_INT64, &width); mpv_get_property(mpv, "video-out-params/h", MPV_FORMAT_INT64, &height);
        mpv_terminate_destroy(mpv);
        if (!finished || !seeked || ((mode & 1) && vsr <= 12) || ((mode & 2) && vfg <= 12) || resets == 0 || width != ((mode & 1) ? 1280 : 640) || height != ((mode & 1) ? 720 : 360)) { std::fprintf(stderr, "PLAYBACK_FAIL mode=%u finished=%d seek=%d vsr=%u vfg=%u resets=%u output=%lldx%lld\n", mode, finished, seeked, vsr, vfg, resets, width, height); DestroyWindow(window); return 10; }
        std::printf("NVVIDEO_MPV_PLAYBACK_OK mode=%u vsr-sdk-frames=%u vfg-sdk-frames=%u resets=%u output=%lldx%lld seek=1 eof=1\n", mode, vsr, vfg, resets, width, height);
    }
    DestroyWindow(window); return 0;
}
