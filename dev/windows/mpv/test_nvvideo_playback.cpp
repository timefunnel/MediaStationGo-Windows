// Headless native-window mpv playback test. Runs only against generated local media.
#include <windows.h>
#include <mpv/client.h>
#include <chrono>
#include <cstdio>
#include <string>
#include <cstdlib>
#include <cstring>
#include <cmath>
static std::string utf8(const wchar_t *value) {
    int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, -1, nullptr, 0, nullptr, nullptr);
    std::string text(size, '\0'); WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value, -1, text.data(), size, nullptr, nullptr); text.pop_back(); return text;
}
static std::string quote(const std::string &text) { return "%" + std::to_string(text.size()) + "%" + text; }
static unsigned count(mpv_handle *mpv, const char *name) { char *value = mpv_get_property_string(mpv, name); unsigned result = value ? static_cast<unsigned>(std::strtoul(value, nullptr, 10)) : 0; mpv_free(value); return result; }
static double metadata(mpv_handle *mpv, const char *field) {
    char *value=mpv_get_property_string(mpv,(std::string("vf-metadata/enhance_vfg/")+field).c_str());
    double number=value?std::strtod(value,nullptr):NAN;mpv_free(value);return number;
}
int wmain(int argc, wchar_t **argv) {
    if (argc != 15) return 2; // SDR, bridge, VSR, VFG, cache, NR, SDR HD, HDR10, HDR10 HD, HDR-only, HLG, HLG HD, SRT, auto-only
    HWND window = CreateWindowExW(0, L"STATIC", L"MediaStationGo native video test", WS_POPUP, 0, 0, 640, 360, nullptr, nullptr, GetModuleHandleW(nullptr), nullptr);
    if (!window) return 3; // Never shown. The D3D11 output context still uses this native surface.
    for (unsigned transfer : {0u, 1u, 2u}) for (unsigned mode : {1u, 2u, 3u, 4u, 7u, 8u, 12u, 16u}) {
        const bool hdr=transfer!=0;
        const bool automatic=mode==16, vfg_enabled=(mode&2)||mode==8||automatic;
        if (std::wstring(argv[14]) == L"1" && !automatic) continue;
        if (!hdr && std::wstring(argv[10]) == L"1") continue;
        if (hdr && mode == 8) continue; // High 8x is already known to exceed real-time budget.
        mpv_handle *mpv = mpv_create(); if (!mpv) return 4;
        int64_t wid = reinterpret_cast<int64_t>(window);
        if (mpv_set_option(mpv, "wid", MPV_FORMAT_INT64, &wid) < 0) return 5;
        for (auto pair : {std::pair{"vo", "gpu-next"}, {"gpu-api", "d3d11"}, {"gpu-context", "d3d11"}, {"hwdec", "d3d11va"}, {"ao", "null"}, {"terminal", "no"}, {"keep-open", "yes"}, {"input-default-bindings", "no"}}) if (mpv_set_option_string(mpv, pair.first, pair.second) < 0) return 6;
        if(automatic&&!hdr&&(mpv_set_option_string(mpv,"aid","2")<0||mpv_set_option_string(mpv,"sid","1")<0))return 6;
        const auto bridge = quote(utf8(argv[2])), cache = quote(utf8(argv[5]));
        std::string filter = hdr ? "" : "@nvconvert:d3d11vpp=format=bgra";
        auto append=[&](const std::string &value){if(!filter.empty())filter+=",";filter+=value;};
        const std::string vfg_filter="@enhance_vfg:nvvideo=mode=2:quality=" + std::string(automatic?"0:multiplier=0":hdr?"0:multiplier=2":mode == 8 ? "2:multiplier=8" : "1:multiplier=3") + ":bridge=" + bridge + ":runtime=" + quote(utf8(argv[4])) + ":cache=" + cache;
        if (mode & 4) append("@enhance_nr:nvvideo=mode=3:nr-target=" + std::string(mode == 12 ? "720" : "1080") + ":nr-style=1:nr-intensity=0.7:nr-tone=0.6:nr-structure=0.8:nr-skin=1:nr-auto-mask=1:nr-ui-correction=1:bridge=" + bridge + ":runtime=" + quote(utf8(argv[6])) + ":cache=" + cache);
        if (!hdr && vfg_enabled) append(vfg_filter);
        if (mode & 1) append("@enhance_vsr:nvvideo=mode=1:quality=3:target-height=" + std::string(hdr?"720":"1440") + ":bridge=" + bridge + ":runtime=" + quote(utf8(argv[3])) + ":cache=" + cache);
        if (hdr && vfg_enabled) append(vfg_filter);
        if (!hdr) append("@nvoutput:d3d11vpp=format=nv12");
        if (mpv_set_option_string(mpv, "vf", filter.c_str()) < 0 || mpv_initialize(mpv) < 0) return 7;
        mpv_request_log_messages(mpv, automatic ? "info" : "warn"); auto filename = utf8(argv[transfer==2 ? mode==12?12:11 : hdr ? mode==12?9:8 : mode == 12 ? 7 : 1]); const char *load[] = {"loadfile", filename.c_str(), nullptr};
        if (mpv_command(mpv, load) < 0) return 8;
        auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(35);
        bool seeked = false, finished = false; unsigned vsr = 0, vfg = 0, nr = 0, resets = 0, reconfigs = 0;
        bool audio_switched=false, paused_seek=false, embedded_seen=false, external_added=false, external_seen=false;
        auto next_report=std::chrono::steady_clock::now()+std::chrono::seconds(1);
        double max_av = 0;
        while (std::chrono::steady_clock::now() < deadline) {
            MSG message; while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) { TranslateMessage(&message); DispatchMessageW(&message); }
            auto event = mpv_wait_event(mpv, 0.02);
            if (event->event_id == MPV_EVENT_LOG_MESSAGE) {
                auto log = static_cast<mpv_event_log_message *>(event->data);
                // VIDEO_RECONFIG also reports decoder/filter format events;
                // count the actual VO recreation that stalls presentation.
                if(!std::strcmp(log->prefix,"cplayer")&&!std::strncmp(log->text,"VO: [",5)) reconfigs++;
                std::fprintf(stderr, "mode=%u %s: %s",mode, log->prefix, log->text);
            }
            double av = 0;if(mpv_get_property(mpv,"avsync",MPV_FORMAT_DOUBLE,&av)>=0 && seeked && std::abs(av)>max_av)max_av=std::abs(av);
            vsr = count(mpv, "vf-metadata/enhance_vsr/sdk-frames"); vfg = count(mpv, "vf-metadata/enhance_vfg/sdk-frames");
            nr = count(mpv, "vf-metadata/enhance_nr/sdk-frames");
            resets = count(mpv, (mode & 4) ? "vf-metadata/enhance_nr/resets" : vfg_enabled ? "vf-metadata/enhance_vfg/resets" : "vf-metadata/enhance_vsr/resets");
            if(automatic&&!hdr&&!audio_switched&&vfg>=12) {
                int64_t aid=0,sid=0;
                if(mpv_get_property(mpv,"aid",MPV_FORMAT_INT64,&aid)<0||mpv_get_property(mpv,"sid",MPV_FORMAT_INT64,&sid)<0||aid!=2||sid!=1)return 12;
                if(mpv_set_property_string(mpv,"aid","1")<0)return 12;
                audio_switched=true;
            }
            // A track change queues its own demux refresh. Wait for video to
            // advance before separately exercising a paused exact seek.
            if (!seeked && (!(automatic&&!hdr)||vfg>=36) && (vsr >= 12 || vfg >= 12 || nr >= 12)) {
                if(automatic&&!hdr) {
                    int64_t aid=0,sid=0;
                    if(mpv_get_property(mpv,"aid",MPV_FORMAT_INT64,&aid)<0||mpv_get_property(mpv,"sid",MPV_FORMAT_INT64,&sid)<0||aid!=1||sid!=1)return 12;
                    if(mpv_set_property_string(mpv,"pause","yes")<0)return 12;
                    paused_seek=true;
                }
                const char *seek[] = {"seek", "1", "absolute+exact", nullptr}; if (mpv_command(mpv, seek) < 0) return 9; seeked = true;
            }
            if(automatic&&!hdr) {
                double pos=0;mpv_get_property(mpv,"time-pos",MPV_FORMAT_DOUBLE,&pos);
                if(paused_seek&&event->event_id==MPV_EVENT_PLAYBACK_RESTART&&std::abs(pos-1)<.1) {
                    int paused=0;int64_t aid=0,sid=0;
                    if(mpv_get_property(mpv,"pause",MPV_FORMAT_FLAG,&paused)<0||!paused||mpv_get_property(mpv,"aid",MPV_FORMAT_INT64,&aid)<0||aid!=1||mpv_get_property(mpv,"sid",MPV_FORMAT_INT64,&sid)<0||sid!=1)return 13;
                    if(mpv_set_property_string(mpv,"pause","no")<0)return 13;paused_seek=false;
                }
                char *text=mpv_get_property_string(mpv,"sub-text");
                if(text&&pos>=1.3&&pos<=2.1&&std::string(text).find("VFG sync check A")!=std::string::npos)embedded_seen=true;
                if(text&&external_added&&pos>=2.3&&pos<=3.8&&std::string(text).find("VFG sync check B")!=std::string::npos)external_seen=true;
                mpv_free(text);
                if(embedded_seen&&!external_added&&pos>2.15) {
                    auto subtitle=utf8(argv[13]);const char *add[]={"sub-add",subtitle.c_str(),"select",nullptr};
                    if(mpv_command(mpv,add)<0)return 14;external_added=true;
                }
            }
            if(automatic&&std::chrono::steady_clock::now()>=next_report) {
                double pos=0;int seeking=0,paused=0,eof=0;
                mpv_get_property(mpv,"time-pos",MPV_FORMAT_DOUBLE,&pos);mpv_get_property(mpv,"seeking",MPV_FORMAT_FLAG,&seeking);mpv_get_property(mpv,"pause",MPV_FORMAT_FLAG,&paused);mpv_get_property(mpv,"eof-reached",MPV_FORMAT_FLAG,&eof);
                std::fprintf(stderr,"VFG_AUTO_PROGRESS transfer=%u pos=%.3f source-end=%.3f output-end=%.3f sdk=%u seeking=%d paused=%d eof=%d\n",transfer,pos,metadata(mpv,"source-end-pts"),metadata(mpv,"output-end-pts"),vfg,seeking,paused,eof);
                next_report=std::chrono::steady_clock::now()+std::chrono::seconds(1);
            }
            // keep-open can seek again to retain the final picture. EOF status
            // alone can already be true while that seek is still decoding.
            int eof = 0, seeking = 0;
            if (mpv_get_property(mpv, "eof-reached", MPV_FORMAT_FLAG, &eof) >= 0 && eof &&
                mpv_get_property(mpv, "seeking", MPV_FORMAT_FLAG, &seeking) >= 0 && !seeking) { finished = true; break; }
            if (event->event_id == MPV_EVENT_END_FILE) { auto end = static_cast<mpv_event_end_file *>(event->data); if (end->reason == MPV_END_FILE_REASON_ERROR) { std::fprintf(stderr, "mpv end-file error %d\n", end->error); break; } }
        }
        int64_t width = 0, height = 0, source_width = 0, source_height = 0;
        mpv_get_property(mpv, "video-out-params/w", MPV_FORMAT_INT64, &width); mpv_get_property(mpv, "video-out-params/h", MPV_FORMAT_INT64, &height);
        mpv_get_property(mpv, "video-params/w", MPV_FORMAT_INT64, &source_width); mpv_get_property(mpv, "video-params/h", MPV_FORMAT_INT64, &source_height);
        int64_t expected_width=automatic?source_width:(mode&1)?hdr?1280:2560:mode==12?1920:640;
        int64_t expected_height=automatic?source_height:(mode&1)?hdr?720:1440:mode==12?1080:360;
        const char* label=(mode&4)?"enhance_nr":vfg_enabled?"enhance_vfg":"enhance_vsr";
        char* elapsed=mpv_get_property_string(mpv,(std::string("vf-metadata/")+label+"/avg-process-ms").c_str());
        if ((mode == 4 || mode == 12) && max_av > 0.25) { std::fprintf(stderr,"DLSSNR_REALTIME_FAIL mode=%u max-avsync=%.4f\n",mode,max_av);mpv_free(elapsed);mpv_terminate_destroy(mpv);DestroyWindow(window);return 11; }
        char *gamma=mpv_get_property_string(mpv,"video-out-params/gamma"),*primaries=mpv_get_property_string(mpv,"video-out-params/primaries"),*encoding=mpv_get_property_string(mpv,(std::string("vf-metadata/")+label+"/sdk-encoding").c_str());
        bool color_ok=!hdr||(gamma&&std::string(gamma)==(transfer==2?"hlg":"pq")&&primaries&&std::string(primaries)=="bt.2020");
        if(hdr) {
            char *format=mpv_get_property_string(mpv,"video-out-params/hw-pixelformat");
            color_ok=color_ok&&format&&std::string(format)=="p010";mpv_free(format);
            for(const char *field:{"min-luma","max-luma","max-cll","max-fall"}) {
                double before=0,after=0;
                int source=mpv_get_property(mpv,(std::string("video-params/")+field).c_str(),MPV_FORMAT_DOUBLE,&before),output=mpv_get_property(mpv,(std::string("video-out-params/")+field).c_str(),MPV_FORMAT_DOUBLE,&after);
                color_ok=color_ok&&(transfer==1 ? source>=0&&output>=0&&std::abs(before-after)<.00001 : ((source>=0)==(output>=0))&&(source<0||std::abs(before-after)<.00001));
            }
            if(vfg_enabled) {
                char *sdk=mpv_get_property_string(mpv,"vf-metadata/enhance_vfg/sdk-encoding");
                color_ok=color_ok&&sdk&&std::string(sdk)=="rgb10a2-pq";mpv_free(sdk);
            }
            if(transfer==2) {
                char *range=mpv_get_property_string(mpv,(std::string("vf-metadata/")+label+"/dynamic-range").c_str()),*reference=mpv_get_property_string(mpv,(std::string("vf-metadata/")+label+"/hlg-reference").c_str());
                color_ok=color_ok&&range&&std::string(range)=="hlg"&&reference&&std::string(reference)=="1000-nits-gamma-1.2";mpv_free(range);mpv_free(reference);
            }
        }
        if (!finished || !seeked || !color_ok || ((mode & 1) && vsr <= 12) || (vfg_enabled && vfg <= 12) || ((mode & 4) && nr <= 12) || resets == 0 || width <= 0 || height <= 0 || width != expected_width || height != expected_height) { std::fprintf(stderr, "PLAYBACK_FAIL hdr=%d mode=%u finished=%d seek=%d vsr=%u vfg=%u nr=%u resets=%u output=%lldx%lld gamma=%s primaries=%s\n",hdr, mode, finished, seeked, vsr, vfg,nr, resets, width, height,gamma?gamma:"null",primaries?primaries:"null"); DestroyWindow(window); return 10; }
        if(automatic) {
            double target=metadata(mpv,"target-fps"),display=metadata(mpv,"display-fps"),divisor=metadata(mpv,"display-divisor"),error=metadata(mpv,"max-timing-error-ms"),source_end=metadata(mpv,"source-end-pts"),output_end=metadata(mpv,"output-end-pts");
            if(!std::isfinite(target)||!std::isfinite(display)||!std::isfinite(divisor)||!std::isfinite(error)||!std::isfinite(source_end)||!std::isfinite(output_end)||target<=24||divisor<1||std::abs(target*divisor-display)>.00001||error>.001||std::abs(source_end-output_end)>.00001||max_av>.1||reconfigs>8||(!hdr&&(paused_seek||!embedded_seen||!external_seen))) {
                std::fprintf(stderr,"VFG_AUTO_FAIL transfer=%u target=%.9f display=%.9f divisor=%.0f timing-error=%.6f source-end=%.9f output-end=%.9f av=%.4f paused=%d embedded=%d external=%d reconfigs=%u\n",transfer,target,display,divisor,error,source_end,output_end,max_av,paused_seek,embedded_seen,external_seen,reconfigs);return 15;
            }
            std::printf("VFG_AUTO_TIMELINE_OK transfer=%u target=%.9f display=%.9f divisor=%.0f timing-error-ms=%.6f source-end=%.9f output-end=%.9f av=%.4f reconfigs=%u tracks/subtitles=%s\n",transfer,target,display,divisor,error,source_end,output_end,max_av,reconfigs,hdr?"not-in-HDR-fixture":"audio-switch/paused-seek/embedded/external");
        }
        std::printf("NVVIDEO_MPV_PLAYBACK_OK transfer=%u mode=%u vsr-sdk-frames=%u vfg-sdk-frames=%u nr-sdk-frames=%u resets=%u output=%lldx%lld seek=1 eof=1 avg-process-ms=%s max-avsync=%.4f gamma=%s primaries=%s sdk-encoding=%s\n",transfer, mode, vsr, vfg,nr, resets, width, height,elapsed?elapsed:"unavailable",max_av,gamma?gamma:"unknown",primaries?primaries:"unknown",encoding?encoding:"unknown");
        mpv_free(gamma);mpv_free(primaries);mpv_free(encoding);
        mpv_free(elapsed);mpv_terminate_destroy(mpv);
    }
    DestroyWindow(window); return 0;
}
