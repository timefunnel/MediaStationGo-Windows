// Numeric PQ/HLG acceptance on the real GPU. CPU readback is test-only.
#define NOMINMAX
#include "hdr_pipeline.h"
#include "dovi_pipeline.h"
#include <dxgi1_2.h>
#include <vector>
#include <array>
#include <cmath>
#include <algorithm>
#include <chrono>
#include <thread>
using Microsoft::WRL::ComPtr;
constexpr unsigned W=640,H=360;
static double pq(double nits) {
    double p=std::pow(nits/10000.0,2610.0/16384.0);
    return std::pow((3424.0/4096.0+2413.0/128.0*p)/(1+2392.0/128.0*p),2523.0/32.0);
}
static std::array<double,3> hlg_signal(std::array<double,3> nits) {
    double luminance=(.2627*nits[0]+.6780*nits[1]+.0593*nits[2])/1000.0;
    for(double &value:nits) {
        double scene=luminance>0 ? value/1000.0/std::pow(luminance,1.0/6.0) : 0;
        value=scene<=1.0/12.0 ? std::sqrt(3*scene) : .17883277*std::log(12*scene-.28466892)+.55991073;
    }
    return nits;
}
static std::vector<uint16_t> pixels(bool full, unsigned shift, unsigned transfer) {
    std::vector<uint16_t> data(W*H*3/2);
    std::array<std::array<double,3>,8> colors={{{0,0,0},{1,1,1},{100,100,100},{203,203,203},{1000,1000,1000},{4000,4000,4000},{800,20,5},{5,100,800}}};
    if(transfer==NVVIDEO_HLG){colors[4]={500,500,500};colors[5]={1000,1000,1000};colors[6]={300,20,5};colors[7]={5,100,500};}
    for(unsigned y=0;y<H;y++)for(unsigned x=0;x<W;x++) {
        auto color=colors[(x/80)%8];
        if(y>220&&y<320)color=colors[((x+shift)/40)%8];
        auto signal=transfer==NVVIDEO_HLG ? hlg_signal(color) : std::array<double,3>{pq(color[0]),pq(color[1]),pq(color[2])};
        double r=signal[0],g=signal[1],b=signal[2],l=.2627*r+.6780*g+.0593*b;
        auto code=[](double v){return static_cast<uint16_t>(std::round(std::clamp(v,0.0,1023.0))*64);};
        data[y*W+x]=code(l*(full?1023:876)+(full?0:64));
        if(!(x&1)&&!(y&1)) {
            size_t pos=W*H+(y/2)*W+x;
            data[pos]=code((b-l)/1.8814*(full?1023:896)+512);
            data[pos+1]=code((r-l)/1.4746*(full?1023:896)+512);
        }
    }
    return data;
}
static ComPtr<ID3D11Texture2D> texture(ID3D11Device *device,unsigned w,unsigned h,const std::vector<uint16_t> *data=nullptr) {
    D3D11_TEXTURE2D_DESC d{};d.Width=w;d.Height=h;d.MipLevels=d.ArraySize=1;d.Format=DXGI_FORMAT_P010;d.SampleDesc.Count=1;
    d.BindFlags=D3D11_BIND_SHADER_RESOURCE;
    D3D11_SUBRESOURCE_DATA initial{data?data->data():nullptr,w*2,0};ComPtr<ID3D11Texture2D> t;
    HdrPipeline::hr(device->CreateTexture2D(&d,data?&initial:nullptr,&t),"test P010 texture");return t;
}
static std::vector<uint16_t> read(ID3D11Device *device,ID3D11DeviceContext *context,ID3D11Texture2D *texture) {
    D3D11_TEXTURE2D_DESC d{};texture->GetDesc(&d);d.BindFlags=d.MiscFlags=0;d.Usage=D3D11_USAGE_STAGING;d.CPUAccessFlags=D3D11_CPU_ACCESS_READ;
    ComPtr<ID3D11Texture2D> staging;HdrPipeline::hr(device->CreateTexture2D(&d,nullptr,&staging),"HDR test readback");context->CopyResource(staging.Get(),texture);
    D3D11_MAPPED_SUBRESOURCE map{};HdrPipeline::hr(context->Map(staging.Get(),0,D3D11_MAP_READ,0,&map),"HDR test map");
    unsigned stride=d.Width*(d.Format==DXGI_FORMAT_R16G16B16A16_FLOAT?4:1),rows=d.Format==DXGI_FORMAT_R16G16B16A16_FLOAT?d.Height:d.Height*3/2;
    std::vector<uint16_t> result(stride*rows);
    for(unsigned row=0;row<rows;row++)memcpy(result.data()+row*stride,static_cast<const uint8_t*>(map.pData)+row*map.RowPitch,stride*2);
    context->Unmap(staging.Get(),0);return result;
}
static double half_value(uint16_t bits) {
    unsigned exponent=(bits>>10)&31,fraction=bits&1023;
    if(exponent==31)throw std::runtime_error("Non-finite HDR base");
    double value=exponent?std::ldexp(1.0+fraction/1024.0,int(exponent)-15):std::ldexp(double(fraction),-24);
    return bits&32768?-value:value;
}
static void validate_hlg_light(const std::vector<uint16_t> &input,const std::vector<uint16_t> &base,bool full,unsigned chroma) {
    // Compare the GPU's retained light to an independent scalar BT.2100 reference,
    // including colored bars. A decode/encode round trip alone cannot detect a wrong OOTF.
    std::vector<std::pair<unsigned,unsigned>> samples;
    for(unsigned bar=0;bar<8;bar++)samples.emplace_back(bar*80+40,80);
    for(unsigned x:{79u,80u,81u,159u,160u,161u})for(unsigned y:{219u,220u,221u,319u,320u,321u})samples.emplace_back(x,y);
    for(auto [x,row]:samples) {
        double y=(double(input[row*W+x]>>6)-(full?0:64))/(full?1023:876);
        // Sample about physical luma-pixel centers, independently of shader UVs.
        double location_x=(chroma==2||chroma==3||chroma==5)?.5:0;
        double location_y=(chroma==1||chroma==3)?0:(chroma==4||chroma==5)?1:.5;
        double cx=(x-location_x)/2,cy=(row-location_y)/2;
        int ix=int(std::floor(cx)),iy=int(std::floor(cy));double fx=cx-ix,fy=cy-iy;
        auto chroma_code=[&](unsigned channel) {
            double value=0;
            for(int dy=0;dy<2;dy++)for(int dx=0;dx<2;dx++) {
                unsigned sx=unsigned(std::clamp(ix+dx,0,int(W/2-1))),sy=unsigned(std::clamp(iy+dy,0,int(H/2-1)));
                value+=(input[W*H+sy*W+sx*2+channel]>>6)*(dx?fx:1-fx)*(dy?fy:1-fy);
            }
            return (value-512)/(full?1023:896);
        };
        double u=chroma_code(0),v=chroma_code(1);
        std::array<double,3> scene={y+1.4746*v,y-.164553*u-.571353*v,y+1.8814*u};
        for(double &value:scene){value=std::clamp(value,0.0,1.0);value=value<=.5?value*value/3:(std::exp((value-.55991073)/.17883277)+.28466892)/12;}
        double luminance=.2627*scene[0]+.6780*scene[1]+.0593*scene[2];
        for(unsigned channel=0;channel<3;channel++) {
            double expected=1000*scene[channel]*std::pow(luminance,.2),actual=half_value(base[(row*W+x)*4+channel]);
            if(std::abs(actual-expected)>std::max(.03,expected*.001))throw std::runtime_error("HLG retained light does not match BT.2100 OOTF");
        }
    }
    std::printf("HLG_LINEAR_REFERENCE_OK full=%d chroma=%u samples=%zu\n",full,chroma,samples.size());
}
static unsigned gray_error(const std::vector<uint16_t> &a,const std::vector<uint16_t> &b,unsigned scale) {
    unsigned error=0;
    for(unsigned bar=0;bar<6;bar++) {
        unsigned x=(bar*80+40)*scale,y=80*scale;
        error=std::max(error,static_cast<unsigned>(std::abs(int(a[80*W+bar*80+40]>>6)-int(b[y*W*scale+x]>>6))));
    }
    return error;
}
static nvvideo_dovi test_rpu(unsigned variant) {
    nvvideo_dovi r{};
    for(unsigned c=0;c<3;c++) {
        r.nonlinear[c][c]=1;r.linear[c][c]=1;
        auto &component=r.comp[c];component.num_pivots=variant==3?9:3;
        for(unsigned p=0;p<component.num_pivots;p++)component.pivots[p][0]=float(p)/(component.num_pivots-1);
        for(unsigned p=0;p<component.num_pivots-1;p++) {
            auto &s=component.segments[p];
            if((c+p+variant)%2) {
                unsigned order=1+variant%3;s.polynomial[3]=float(order);
                s.mmr[0][0][c]=.85f;
                s.mmr[0][1][0]=.025f;
                if(order>=2)s.mmr[1][0][c]=.05f;
                if(order==3)s.mmr[2][1][3]=.01f;
            } else {s.polynomial[1]=.9f;s.polynomial[2]=.04f;}
        }
    }
    r.nonlinear[0][1]=.03f;r.nonlinear[1][2]=-.02f;
    r.nonlinear[0][3]=.01f;r.linear[2][1]=.04f;
    return r;
}
static double pq_nits(double signal) {
    double p=std::pow(std::max(signal,0.0),32.0/2523.0);
    return 10000*std::pow(std::max(p-3424.0/4096.0,0.0)/std::max(2413.0/128.0-2392.0/128.0*p,1e-6),16384.0/2610.0);
}
static std::array<double,3> dovi_reference(const std::vector<uint16_t> &data,int x,int y,const nvvideo_config &c,const nvvideo_dovi &r) {
    x=std::clamp(x,0,int(W)-1);y=std::clamp(y,0,int(H)-1);
    double offset_x=c.chroma_location==2||c.chroma_location==3||c.chroma_location==5?.5:1;
    double offset_y=c.chroma_location==1||c.chroma_location==3?1:c.chroma_location>=4?0:.5;
    double uv_x=(x+offset_x)/2-.5,uv_y=(y+offset_y)/2-.5;
    int ix=int(std::floor(uv_x)),iy=int(std::floor(uv_y));double fx=uv_x-ix,fy=uv_y-iy;
    std::array<double,3> s{double(data[y*W+x]>>6)/1024,0,0};
    for(int ch=0;ch<2;ch++)for(int dy=0;dy<2;dy++)for(int dx=0;dx<2;dx++)
        s[ch+1]+=(data[W*H+std::clamp(iy+dy,0,int(H/2)-1)*W+std::clamp(ix+dx,0,int(W/2)-1)*2+ch]>>6)/1024.0*(dx?fx:1-fx)*(dy?fy:1-fy);
    auto el_codes=s;
    for(unsigned ch=0;ch<3;ch++)if(r.comp[ch].num_pivots)
        s[ch]=std::clamp(s[ch],double(r.comp[ch].pivots[0][0]),double(r.comp[ch].pivots[r.comp[ch].num_pivots-1][0]));
    auto mapped=s;
    for(unsigned ch=0;ch<3;ch++) {
        const auto &v=r.comp[ch];if(!v.num_pivots)continue;
        unsigned i=0;while(i+2<v.num_pivots&&s[ch]>=v.pivots[i+1][0])i++;
        const auto &p=v.segments[i];double value=p.polynomial[0];
        if(!p.polynomial[3])value+=p.polynomial[1]*s[ch]+p.polynomial[2]*s[ch]*s[ch];
        else {
            std::array<double,7> terms{s[0],s[1],s[2],s[0]*s[1],s[0]*s[2],s[1]*s[2],s[0]*s[1]*s[2]};
            for(unsigned degree=0;degree<unsigned(p.polynomial[3]);degree++)for(unsigned k=0;k<7;k++)
                value+=(k<3?p.mmr[degree][0][k]:p.mmr[degree][1][k-3])*std::pow(terms[k],degree+1);
        }
        mapped[ch]=std::clamp(value,0.0,1.0);
    }
    if(r.nlq_active)for(unsigned ch=0;ch<3;ch++) {
        double rr=std::nearbyint(el_codes[ch]*1024-r.nlq[ch][0]);
        double residual=(rr>0?1:rr<0?-1:0)*((std::abs(rr)-.5)*r.nlq[ch][1]+r.nlq[ch][2]);
        mapped[ch]=std::clamp(mapped[ch]+std::clamp(residual,-double(r.nlq[ch][3]),double(r.nlq[ch][3])),0.0,1.0);
    }
    std::array<double,3> lms{},rgb{};
    for(unsigned i=0;i<3;i++) {
        double signal=0;for(unsigned k=0;k<3;k++)signal+=r.nonlinear[i][k]*(mapped[k]-r.nonlinear[k][3]);
        lms[i]=pq_nits(signal);
    }
    for(unsigned i=0;i<3;i++) {
        double nits=0;for(unsigned k=0;k<3;k++)nits+=r.linear[i][k]*lms[k];
        rgb[i]=pq(std::clamp(nits,0.0,10000.0));
    }
    double luma=.2627*rgb[0]+.6780*rgb[1]+.0593*rgb[2];
    return {luma,(rgb[2]-luma)/1.8814,(rgb[0]-luma)/1.4746};
}
static void validate_dovi(nvvideo_config c) {
    c.mode=2;c.transfer=NVVIDEO_PQ;
    for(unsigned range=0;range<2;range++)for(unsigned chroma=0;chroma<6;chroma++) {
        c.full_range=range;c.chroma_location=chroma;
        HdrPipeline hdr;hdr.initialize(c);DoviPipeline pipeline;pipeline.initialize(hdr);
        auto data=pixels(range!=0,0,NVVIDEO_PQ);auto input=texture(c.device,W,H,&data),output=texture(c.device,W,H);
        std::vector<uint16_t> previous;
        for(unsigned variant=0;variant<6;variant++) {
            auto r=test_rpu(variant);
            if(variant>=4) {
                r.nlq_active=1;r.el_width=W;r.el_height=H;r.el_chroma=chroma;
                for(unsigned ch=0;ch<3;ch++){r.nlq[ch][0]=512;r.nlq[ch][1]=1.f/4096;r.nlq[ch][2]=variant==5?.0001f:0;r.nlq[ch][3]=.125f;}
            }
            pipeline.process(input.Get(),output.Get(),r,variant>=4?input.Get():nullptr);
            auto result=read(c.device,c.context,output.Get());
            if(!previous.empty()&&previous==result)throw std::runtime_error("Per-frame RPU update did not change the mapped pixels");
            previous=result;
            unsigned max_error=0;
            for(int y:{0,80,240,358})for(int x:{0,40,78,80,82,120,158,160,200,520,600,638}) {
                auto expected=dovi_reference(data,x,y,c,r);
                auto code=[](double value){return int(std::round(std::clamp(value,0.0,1023.0)));};
                max_error=std::max(max_error,unsigned(std::abs(code(expected[0]*(range?1023:876)+(range?0:64))-int(result[y*W+x]>>6))));
                bool half_x=chroma==2||chroma==3||chroma==5,half_y=chroma==0||chroma==2;int center_y=chroma>=4?1:0;
                std::array<double,2> uv{};
                for(int dy=half_y?0:center_y-1;dy<=(half_y?1:center_y+1);dy++)for(int dx=half_x?0:-1;dx<=1;dx++) {
                    auto value=dovi_reference(data,x+dx,y+dy,c,r);
                    double weight=(half_x?.5:dx==0?.5:.25)*(half_y?.5:dy==center_y?.5:.25);
                    for(unsigned k=0;k<2;k++)uv[k]+=value[k+1]*weight;
                }
                for(unsigned k=0;k<2;k++)max_error=std::max(max_error,unsigned(std::abs(code(uv[k]*(range?1023:896)+512)-int(result[W*H+y/2*W+x+k]>>6))));
            }
            if(max_error>1||std::any_of(result.begin(),result.end(),[](uint16_t value){return value&63;}))throw std::runtime_error("Dolby Vision GPU mapping differs from scalar RPU reference");
            std::printf("DOVI_RPU_GPU_REFERENCE_OK full=%u chroma=%u variant=%u fel=%u max-code-error=%u\n",range,chroma,variant,r.nlq_active,max_error);
        }
        auto invalid=test_rpu(0);invalid.comp[0].num_pivots=10;bool rejected=false;
        try{pipeline.process(input.Get(),output.Get(),invalid);}catch(const std::exception &){rejected=true;}
        if(!rejected)throw std::runtime_error("Invalid RPU was accepted");
        auto missing_el=test_rpu(0);missing_el.nlq_active=1;
        missing_el.el_width=W;missing_el.el_height=H;
        rejected=false;
        try{pipeline.process(input.Get(),output.Get(),missing_el);}catch(const std::exception &){rejected=true;}
        if(!rejected)throw std::runtime_error("FEL was silently rendered without its enhancement layer");
    }
}
static void measure_dovi_4k(nvvideo_config c) {
    c.width=c.out_width=3840;c.height=c.out_height=2160;c.mode=2;c.transfer=NVVIDEO_PQ;
    HdrPipeline hdr;hdr.initialize(c);DoviPipeline pipeline;pipeline.initialize(hdr);
    std::vector<uint16_t> pixels4k(size_t(c.width)*c.height*3/2,uint16_t(512<<6));
    D3D11_TEXTURE2D_DESC d{};d.Width=c.width;d.Height=c.height;d.MipLevels=d.ArraySize=1;d.Format=DXGI_FORMAT_P010;d.SampleDesc.Count=1;d.BindFlags=D3D11_BIND_SHADER_RESOURCE;
    D3D11_SUBRESOURCE_DATA data{pixels4k.data(),c.width*2,0};ComPtr<ID3D11Texture2D> input,output;
    HdrPipeline::hr(c.device->CreateTexture2D(&d,&data,&input),"4K RPU input");
    HdrPipeline::hr(c.device->CreateTexture2D(&d,nullptr,&output),"4K RPU output");
    D3D11_QUERY_DESC query{D3D11_QUERY_EVENT,0};ComPtr<ID3D11Query> done;
    HdrPipeline::hr(c.device->CreateQuery(&query,&done),"4K RPU completion query");
    auto r=test_rpu(3);double sum=0,maximum=0;
    for(unsigned i=0;i<7;i++) {
        auto before=std::chrono::steady_clock::now();
        pipeline.process(input.Get(),output.Get(),r);c.context->End(done.Get());c.context->Flush();
        HRESULT status;while((status=c.context->GetData(done.Get(),nullptr,0,0))==S_FALSE)std::this_thread::yield();
        HdrPipeline::hr(status,"4K RPU completion");
        double ms=std::chrono::duration<double,std::milli>(std::chrono::steady_clock::now()-before).count();
        if(i){sum+=ms;maximum=std::max(maximum,ms);}
    }
    std::printf("DOVI_4K_MAP_TIMING width=3840 height=2160 frames=6 avg-ms=%.3f max-ms=%.3f includes=dispatch/copy/GPU-completion excludes=SDK/VO\n",sum/6,maximum);
}
int wmain(int argc,wchar_t **argv) {
    if(argc!=5)return 2;
    try {
        ComPtr<IDXGIFactory1> factory;HdrPipeline::hr(CreateDXGIFactory1(IID_PPV_ARGS(&factory)),"test DXGI");ComPtr<IDXGIAdapter1> adapter;
        for(unsigned i=0;factory->EnumAdapters1(i,&adapter)!=DXGI_ERROR_NOT_FOUND;i++) {DXGI_ADAPTER_DESC1 d{};adapter->GetDesc1(&d);if(d.VendorId==0x10de)break;adapter.Reset();}
        if(!adapter)throw std::runtime_error("NVIDIA adapter unavailable");
        ComPtr<ID3D11Device> device;ComPtr<ID3D11DeviceContext> context;
        HdrPipeline::hr(D3D11CreateDevice(adapter.Get(),D3D_DRIVER_TYPE_UNKNOWN,nullptr,D3D11_CREATE_DEVICE_BGRA_SUPPORT,nullptr,0,D3D11_SDK_VERSION,&device,nullptr,&context),"test D3D11");
        nvvideo_config c{NVVIDEO_ABI,2,W,H,W,H,device.Get(),context.Get(),argv[2],argv[3],4,0,2,1,1,0,0,.7f,.6f,.8f,1.f,1080,1,0,0};
        validate_dovi(c);
        measure_dovi_4k(c);
        for(unsigned transfer:{NVVIDEO_PQ,NVVIDEO_HLG})for(unsigned range=0;range<2;range++)for(unsigned chroma=0;chroma<6;chroma++) {
            c.transfer=transfer;c.full_range=range;c.chroma_location=chroma;auto data=pixels(range!=0,0,transfer);auto input=texture(device.Get(),W,H,&data);
            for(bool packed:{false,true}) {
                c.mode=packed?2:3;HdrPipeline pipeline;pipeline.initialize(c);
                if(packed){pipeline.input(input.Get(),pipeline.current.Get());pipeline.finish(pipeline.current.Get(),true);}
                else{pipeline.input(input.Get());if(transfer==NVVIDEO_HLG)validate_hlg_light(data,read(device.Get(),context.Get(),pipeline.base.Get()),range!=0,chroma);pipeline.finish(pipeline.proxy.Get(),false);}
                auto result=read(device.Get(),context.Get(),pipeline.output.Get());unsigned error=gray_error(data,result,1);
                unsigned color_error=0;for(unsigned bar=6;bar<8;bar++)for(unsigned plane=0;plane<3;plane++) {
                    unsigned pos=plane==0?80*W+bar*80+40:W*H+40*W+bar*80+40+plane-1;
                    color_error=std::max(color_error,static_cast<unsigned>(std::abs(int(data[pos]>>6)-int(result[pos]>>6))));
                }
                auto lowbits=std::count_if(result.begin(),result.end(),[](uint16_t v){return v&63;});
                unsigned tolerance=packed&&transfer==NVVIDEO_HLG?2:1; // HLG <-> PQ adds a separate RGB10 quantization.
                if(error>tolerance||color_error>tolerance||lowbits) {
                    std::fprintf(stderr,"HDR_IDENTITY_FAIL transfer=%u packed=%d full=%u chroma=%u gray=%u color=%u lowbits=%lld samples=",transfer,packed,range,chroma,error,color_error,static_cast<long long>(lowbits));
                    for(unsigned bar=0;bar<8;bar++)std::fprintf(stderr," %u/%u",data[80*W+bar*80+40],result[80*W+bar*80+40]);
                    throw std::runtime_error("HDR identity/10-bit quantization failed");
                }
                std::printf("HDR_IDENTITY_OK transfer=%u packed=%d full=%u chroma=%u max-gray-code-error=%u max-color-code-error=%u\n",transfer,packed,range,chroma,error,color_error);
            }
        }
        for(unsigned transfer:{NVVIDEO_PQ,NVVIDEO_HLG}) {
        c.transfer=transfer;c.full_range=c.chroma_location=0;
        auto a=pixels(false,0,transfer),b=pixels(false,12,transfer);auto first=texture(device.Get(),W,H,&a),second=texture(device.Get(),W,H,&b);
        for(unsigned mode:{2u,1u,3u}) {
            c.mode=mode;c.runtime_directory=argv[mode==3?4:mode];unsigned scale=mode==1?2:1;c.out_width=W*scale;c.out_height=H*scale;
            auto output=texture(device.Get(),c.out_width,c.out_height);char detail[512]{};
            auto *s=nvvideo_create(&c,detail,sizeof(detail));if(!s)throw std::runtime_error(detail);
            bool ok=true;for(unsigned i=0;i<6;i++)if(!nvvideo_process(s,first.Get(),second.Get(),output.Get(),1,detail,sizeof(detail))){ok=false;break;}
            nvvideo_destroy(s);if(!ok)throw std::runtime_error(detail);
            if(mode==2) {
                c.multiplier=0;s=nvvideo_create(&c,detail,sizeof(detail));if(!s)throw std::runtime_error(detail);
                std::vector<uint16_t> early;
                for(unsigned i=0;i<6;i++) {
                    if(!nvvideo_process_timestep(s,first.Get(),second.Get(),output.Get(),i%2+1,i%2?.7f:.3f,detail,sizeof(detail)))throw std::runtime_error(detail);
                    if(i==0)early=read(device.Get(),context.Get(),output.Get());
                }
                auto late=read(device.Get(),context.Get(),output.Get());nvvideo_destroy(s);c.multiplier=2;
                if(early==late||!std::all_of(late.begin(),late.end(),[](uint16_t v){return !(v&63);}))throw std::runtime_error("HDR VFG timestep/10-bit acceptance failed");
                std::printf("HDR_VFG_TIMESTEP_OK transfer=%u distinct-t=0.3/0.7 P010\n",transfer);
            }
            auto result=read(device.Get(),context.Get(),output.Get());unsigned error=gray_error(b,result,scale);
            unsigned protected_error=0;
            for(unsigned bar:{0u,5u})protected_error=(std::max)(protected_error,static_cast<unsigned>(std::abs(int(b[80*W+bar*80+40]>>6)-int(result[80*scale*W*scale+(bar*80+40)*scale]>>6))));
            if(protected_error>1||!std::all_of(result.begin(),result.end(),[](uint16_t v){return !(v&63);}))throw std::runtime_error("HDR SDK black/highlight/10-bit acceptance failed");
            size_t changed=0;if(scale==1)for(size_t i=0;i<result.size();i++)changed+=result[i]!=b[i];
            if(mode!=1&&changed<100)throw std::runtime_error("HDR SDK produced no changed pixels");
            std::printf("HDR_PROVIDER_GPU_OK transfer=%u mode=%u output=%ux%u gray-code-change=%u protected-code-error=%u changed-words=%zu sdk-runs=6\n",transfer,mode,c.out_width,c.out_height,error,protected_error,changed);
        }
        }
        return 0;
    }catch(const std::exception &error){std::fprintf(stderr,"HDR_GPU_FAIL %s\n",error.what());return 1;}
}
