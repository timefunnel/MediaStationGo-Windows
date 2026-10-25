// Actual decoder -> native providers -> gpu-next, using locally generated clips.
#include <windows.h>
#include <mpv/client.h>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <string>
#include <vector>
#include <stdexcept>
#include <iostream>
static std::string utf8(const wchar_t *value) {
    int size=WideCharToMultiByte(CP_UTF8,WC_ERR_INVALID_CHARS,value,-1,nullptr,0,nullptr,nullptr);
    std::string text(size,'\0');WideCharToMultiByte(CP_UTF8,WC_ERR_INVALID_CHARS,value,-1,text.data(),size,nullptr,nullptr);text.pop_back();return text;
}
static std::string quote(const std::string &s){return "%"+std::to_string(s.size())+"%"+s;}
static double field(mpv_handle *mpv,const std::string &label,const char *name) {
    char *s=mpv_get_property_string(mpv,("vf-metadata/"+label+"/"+name).c_str());
    double result=s?std::strtod(s,nullptr):NAN;mpv_free(s);return result;
}
static void require(bool ok,const char *message){if(!ok)throw std::runtime_error(message);}
int wmain(int argc,wchar_t **argv) {
    bool live=argc==10&&std::wstring(argv[9])==L"live";
    if(argc!=9&&!live)return 2;
    std::string live_url;
    if(live){std::getline(std::cin,live_url);if(live_url.empty())return 2;}
    HWND window=CreateWindowExW(0,L"STATIC",L"Dynamic HDR playback test",WS_POPUP,0,0,640,360,nullptr,nullptr,GetModuleHandleW(nullptr),nullptr);
    if(!window)return 3;
    mpv_handle *mpv=nullptr;
    try {
        for(unsigned source:live?std::vector<unsigned>{4}:std::vector<unsigned>{1,2,3})for(unsigned mode:{1u,2u,4u,3u,7u,16u}) {
            mpv=mpv_create();require(mpv!=nullptr,"mpv_create");
            int64_t wid=reinterpret_cast<int64_t>(window);
            require(mpv_set_option(mpv,"wid",MPV_FORMAT_INT64,&wid)>=0,"wid");
            for(auto pair:{std::pair{"vo","gpu-next"},{"gpu-api","d3d11"},{"gpu-context","d3d11"},{"hwdec","d3d11va"},{"ao","null"},{"terminal","no"},{"keep-open","yes"},{"input-default-bindings","no"},{"config","no"},{"resume-playback","no"}})
                require(mpv_set_option_string(mpv,pair.first,pair.second)>=0,"mpv option");
            if(live)for(auto pair:{std::pair{"user-agent","MediaStationGoWindows/1.1.0"},{"network-timeout","15"},{"demuxer-max-bytes","32MiB"},{"demuxer-max-back-bytes","4MiB"},{"cache-secs","3"},{"cache-pause","no"}})
                require(mpv_set_option_string(mpv,pair.first,pair.second)>=0,"live option");
            bool vfg=(mode&2)||mode==16;
            std::vector<std::string> labels;std::string filters;
            auto add=[&](unsigned bit,const char *label,unsigned native_mode,unsigned runtime,const char *parameters) {
                if(!(mode&bit)&&!(mode==16&&bit==2))return;
                if(!filters.empty())filters+=",";
                filters+="@"+std::string(label)+":nvvideo=mode="+std::to_string(native_mode)+parameters+":bridge="+quote(utf8(argv[4]))+":runtime="+quote(utf8(argv[runtime]))+":cache="+quote(utf8(argv[7]));
                labels.emplace_back(label);
            };
            add(4,"enhance_nr",3,8,":nr-target=720");
            add(1,"enhance_vsr",1,5,":quality=3:target-height=720");
            add(2,"enhance_vfg",2,6,mode==16?":quality=0:multiplier=0":":quality=0:multiplier=2");
            require(mpv_set_option_string(mpv,"vf",filters.c_str())>=0&&mpv_initialize(mpv)>=0,"Initialize filters");
            mpv_request_log_messages(mpv,"warn");auto path=live?live_url:utf8(argv[source]);const char *load[]={"loadfile",path.c_str(),nullptr};
            require(mpv_command(mpv,load)>=0,"loadfile");
            bool seek=false,seek_started=false,seek_complete=false,eof=false;
            double sdk_at_seek=0;
            auto deadline=std::chrono::steady_clock::now()+std::chrono::seconds(live?90:40);
            while(std::chrono::steady_clock::now()<deadline) {
                MSG msg;while(PeekMessageW(&msg,nullptr,0,0,PM_REMOVE)){TranslateMessage(&msg);DispatchMessageW(&msg);}
                auto *event=mpv_wait_event(mpv,.02);
                if(event->event_id==MPV_EVENT_LOG_MESSAGE&&!live){auto *log=static_cast<mpv_event_log_message*>(event->data);std::fprintf(stderr,"source=%u mode=%u %s: %s",source,mode,log->prefix,log->text);}
                if(event->event_id==MPV_EVENT_LOG_MESSAGE){auto *log=static_cast<mpv_event_log_message*>(event->data);if(log->log_level<=MPV_LOG_LEVEL_ERROR&&std::string(log->prefix).find("nvvideo")!=std::string::npos){if(live&&std::string(log->text).find("http")==std::string::npos)std::fprintf(stderr,"Native filter: %s",log->text);throw std::runtime_error("Native enhancement filter failed; playback cannot pass by disabling it");}}
                if(event->event_id==MPV_EVENT_END_FILE){auto *end=static_cast<mpv_event_end_file*>(event->data);require(end->reason!=MPV_END_FILE_REASON_ERROR,"Playback failed");}
                if(seek&&event->event_id==MPV_EVENT_SEEK)seek_started=true;
                if(seek&&seek_started&&event->event_id==MPV_EVENT_PLAYBACK_RESTART) {
                    double position=0;
                    bool positioned=mpv_get_property(mpv,"time-pos",MPV_FORMAT_DOUBLE,&position)>=0&&std::abs(position-(live?15:1))<.1;
                    if(live||!positioned)std::fprintf(stderr,"EXACT_SEEK_POSITION source=%u mode=%u actual=%.9f expected=%u\n",source,mode,position,live?15:1);
                    require(positioned,"Exact seek did not reach the requested position");
                    seek_complete=true;
                }
                if(!seek&&field(mpv,labels.back(),"sdk-frames")>=16){sdk_at_seek=field(mpv,labels.back(),"sdk-frames");const char *command[]={"seek",live?"15":"1","absolute+exact",nullptr};require(mpv_command(mpv,command)>=0,"seek");seek=true;}
                if(live&&seek_complete&&field(mpv,labels.back(),"sdk-frames")>sdk_at_seek+16)break;
                int done=0,seeking=0;if(mpv_get_property(mpv,"eof-reached",MPV_FORMAT_FLAG,&done)>=0&&done&&mpv_get_property(mpv,"seeking",MPV_FORMAT_FLAG,&seeking)>=0&&!seeking){eof=true;break;}
            }
            bool completed=seek&&seek_complete&&(live?field(mpv,labels.back(),"sdk-frames")>sdk_at_seek+16:eof);
            if(!completed)std::fprintf(stderr,"PLAYBACK_STATE source=%u mode=%u seek=%u seek-event=%u restart=%u eof=%u sdk=%.0f\n",source,mode,unsigned(seek),unsigned(seek_started),unsigned(seek_complete),unsigned(eof),field(mpv,labels.back(),"sdk-frames"));
            require(completed,"Seek/playback/EOF timeout");
            char *gamma=mpv_get_property_string(mpv,"video-out-params/gamma"),*format=mpv_get_property_string(mpv,"video-out-params/hw-pixelformat");
            bool correct=gamma&&std::string(gamma)=="pq"&&format&&std::string(format)=="p010";mpv_free(gamma);mpv_free(format);require(correct,"Output is not P010/PQ");
            for(const auto &label:labels) {
                double copied=field(mpv,label,source==3?"hdr10plus-output-frames":"rpu-output-frames");
                require(field(mpv,label,"sdk-frames")>16&&copied>16&&field(mpv,label,"metadata-copy-errors")==0,"Dynamic metadata was lost or provider did not run");
            }
            if(source!=3)require(field(mpv,labels.front(),"dovi-mapped-frames")>16,"RPU mapping did not run");
            if(source!=3)for(size_t i=1;i<labels.size();i++)require(field(mpv,labels[i],"dovi-mapped-frames")==0,"RPU was applied twice");
            if(live){require(field(mpv,labels.front(),"fel-reconstructed-frames")>16,"FEL reconstruction did not run");for(size_t i=1;i<labels.size();i++)require(field(mpv,labels[i],"fel-reconstructed-frames")==0,"FEL was composed twice");}
            if(vfg) {
                double source_pts=field(mpv,"enhance_vfg","metadata-source-pts"),end_pts=field(mpv,"enhance_vfg","output-end-pts");
                require(std::isfinite(source_pts)&&source_pts<=end_pts,"VFG used future source metadata");
                require(field(mpv,"enhance_vfg","metadata-interval-errors")==0,"VFG assigned future metadata to a generated frame");
            }
            std::printf("DYNAMIC_HDR_PLAYBACK_OK source=%u mode=%u providers=%zu seek=1 eof=%u fel-frames=%.0f raw-metadata=preserved duplicate-rpu=0\n",source,mode,labels.size(),unsigned(eof),field(mpv,labels.front(),"fel-reconstructed-frames"));std::fflush(stdout);
            mpv_terminate_destroy(mpv);mpv=nullptr;
        }
    }catch(const std::exception &error){std::fprintf(stderr,"DYNAMIC_HDR_PLAYBACK_FAIL %s\n",error.what());if(mpv)mpv_terminate_destroy(mpv);DestroyWindow(window);return 1;}
    DestroyWindow(window);return 0;
}
