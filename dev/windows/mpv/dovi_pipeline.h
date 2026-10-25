// RPU color mapping for the existing mpv/libplacebo rendering contract.
// Polynomial/MMR and HPE LMS conversion follow libplacebo's colorspace API.
#pragma once
#include "hdr_pipeline.h"
#include <cmath>

struct DoviPipeline {
    using Texture = HdrPipeline::Texture;
    template<class T> using Ptr = HdrPipeline::Ptr<T>;
    HdrPipeline *hdr = nullptr;
    Ptr<ID3D11Buffer> metadata;
    Ptr<ID3D11ComputeShader> shader;
    Texture mapped;
    static constexpr const char *source = R"HLSL(
struct Segment { float4 polynomial; float4 mmr[6]; };
struct Component { float4 pivots[9]; Segment segments[8]; uint4 count; };
Texture2D<float> elY : register(t4);
Texture2D<float2> elUV : register(t5);
cbuffer Rpu : register(b1) { float4 nonlinear[3]; float4 lmsToRgb[3]; Component components[3]; float4 nlq[3]; uint4 elConfig; }
float reshapeComponent(uint c, float3 s) {
        uint count=components[c].count.x;
        if(count==0)return s[c];
        uint index=0;
        [unroll] for(uint i=1;i<8;i++)
            if(i<count-1 && s[c]>=components[c].pivots[i].x)index=i;
        float4 polynomial=components[c].segments[index].polynomial;
        float value;
        uint order=(uint)polynomial.w;
        if(order==0)value=(polynomial.z*s[c]+polynomial.y)*s[c]+polynomial.x;
        else {
            value=polynomial.x;
            float4 products=float4(s.x*s.y,s.x*s.z,s.y*s.z,s.x*s.y*s.z);
            float3 power=s; float4 crossPower=products;
            [unroll] for(uint n=0;n<3;n++) {
                if(n<order)value+=dot(components[c].segments[index].mmr[2*n].xyz,power)+dot(components[c].segments[index].mmr[2*n+1],crossPower);
                power*=s; crossPower*=products;
            }
        }
        return saturate(value);
}
float3 reshape(float3 signal) {
    float3 s=signal;
    [unroll] for(uint c=0;c<3;c++)if(components[c].count.x)
        s[c]=clamp(s[c],components[c].pivots[0].x,components[c].pivots[components[c].count.x-1].x);
    return float3(reshapeComponent(0,s),reshapeComponent(1,s),reshapeComponent(2,s));
}
float3 doviPqDecode(float3 signal) {
    // Preserve out-of-gamut LMS until after the linear color matrix, matching
    // libplacebo. Clipping each LMS component before that matrix changes hue.
    float3 p=pow(max(signal,0),1.0/(2523.0/32.0));
    return 10000.0*pow(max(p-3424.0/4096.0,0)/max(2413.0/128.0-2392.0/128.0*p,1e-6),1.0/(2610.0/16384.0));
}
float3 doviPicture(int2 p) {
    p=clamp(p,int2(0,0),int2(w-1,h-1));
    float2 offset=float2(chroma==2||chroma==3||chroma==5 ? .5 : 1,
                         chroma==1||chroma==3 ? 1 : chroma==4||chroma==5 ? 0 : .5);
    float3 signal=float3(inY[p],inUV.SampleLevel(linearClamp,(float2(p)+offset)/float2(w,h),0))*(65535.0/(64.0*1024.0));
    signal=reshape(signal);
    if(elConfig.w) {
        uint ec=elConfig.z;
        float2 eo=float2(ec==2||ec==3||ec==5?.5:1,ec==1||ec==3?1:ec==4||ec==5?0:.5);
        float2 pos=(float2(p)+.5)*float2(elConfig.xy)/float2(w,h)-.5;
        float3 codes=float3(elY.SampleLevel(linearClamp,(pos+.5)/float2(elConfig.xy),0),elUV.SampleLevel(linearClamp,(pos+eo)/float2(elConfig.xy),0))*(65535.0/64.0);
        float3 rr=round(codes-float3(nlq[0].x,nlq[1].x,nlq[2].x));
        float3 residual=sign(rr)*((abs(rr)-.5)*float3(nlq[0].y,nlq[1].y,nlq[2].y)+float3(nlq[0].z,nlq[1].z,nlq[2].z));
        float3 bound=float3(nlq[0].w,nlq[1].w,nlq[2].w);
        signal=saturate(signal+clamp(residual,-bound,bound));
    }
    signal-=float3(nonlinear[0].w,nonlinear[1].w,nonlinear[2].w);
    float3 lms=doviPqDecode(float3(dot(nonlinear[0].xyz,signal),dot(nonlinear[1].xyz,signal),dot(nonlinear[2].xyz,signal)));
    return pqEncode(float3(dot(lmsToRgb[0].xyz,lms),dot(lmsToRgb[1].xyz,lms),dot(lmsToRgb[2].xyz,lms)));
}
[numthreads(16,16,1)] void mapDovi(uint3 id:SV_DispatchThreadID) {
    uint2 p=id.xy;
    if(p.x>=w||p.y>=h)return;
    float3 value=yuv(doviPicture(int2(p)));
    outY[p]=quantize(value.x*(fullRange?1023.0:876.0)+(fullRange?0.0:64.0));
    if((p.x&1u)||(p.y&1u))return;
    bool halfX=chroma==2||chroma==3||chroma==5;
    bool halfY=chroma==0||chroma==2;
    int centerY=chroma==4||chroma==5 ? 1 : 0;
    float2 uv=0;
    for(int dy=halfY?0:centerY-1;dy<=(halfY?1:centerY+1);dy++)for(int dx=halfX?0:-1;dx<=1;dx++) {
        float wx=halfX?.5:dx==0?.5:.25;
        float wy=halfY?.5:dy==centerY?.5:.25;
        uv+=yuv(doviPicture(int2(p)+int2(dx,dy))).yz*wx*wy;
    }
    float2 code=uv*(fullRange?1023.0:896.0)+512.0;
    outUV[p/2]=uint2(quantize(code.x),quantize(code.y));
}
)HLSL";
    static void validate(const nvvideo_dovi &rpu) {
        if(rpu.nlq_active>1||rpu.el_chroma>5)throw std::runtime_error("Invalid Dolby Vision enhancement-layer parameters");
        for(const auto &component:rpu.nlq)for(float value:component)
            if(!std::isfinite(value)||value<0)throw std::runtime_error("Invalid Dolby Vision NLQ coefficient");
        for(unsigned row=0;row<3;row++)for(unsigned col=0;col<4;col++)
            if(!std::isfinite(rpu.nonlinear[row][col])||!std::isfinite(rpu.linear[row][col]))throw std::runtime_error("Non-finite Dolby Vision color matrix");
        for(const auto &component:rpu.comp) {
            auto count=component.num_pivots;
            if(count==0)continue;
            if(count<2||count>9)throw std::runtime_error("Invalid Dolby Vision pivot count");
            for(unsigned i=0;i<count;i++)
                if(!std::isfinite(component.pivots[i][0])||component.pivots[i][0]<0||component.pivots[i][0]>1||(i&&component.pivots[i][0]<=component.pivots[i-1][0]))throw std::runtime_error("Invalid Dolby Vision pivot order/range");
            for(unsigned i=0;i<count-1;i++) {
                const auto &s=component.segments[i];
                for(float coefficient:s.polynomial)if(!std::isfinite(coefficient))throw std::runtime_error("Non-finite Dolby Vision polynomial");
                float order=s.polynomial[3];
                if(order!=0&&order!=1&&order!=2&&order!=3)throw std::runtime_error("Invalid Dolby Vision reshape method/order");
                for(const auto &degree:s.mmr)for(const auto &part:degree)for(float coefficient:part)
                    if(!std::isfinite(coefficient))throw std::runtime_error("Non-finite Dolby Vision MMR coefficient");
            }
        }
    }
    void initialize(HdrPipeline &pipeline) {
        static_assert(sizeof(nvvideo_dovi)%16==0);
        hdr=&pipeline;
        auto codeText=std::string(HdrPipeline::source)+source;
        Ptr<ID3DBlob> code,error;
        HRESULT result=D3DCompile(codeText.c_str(),codeText.size(),"Dolby Vision RPU mapping",nullptr,nullptr,"mapDovi","cs_5_0",D3DCOMPILE_ENABLE_STRICTNESS,0,&code,&error);
        if(FAILED(result))throw std::runtime_error(error?std::string(static_cast<const char*>(error->GetBufferPointer()),error->GetBufferSize()):"Dolby Vision shader compilation failed");
        HdrPipeline::hr(hdr->device->CreateComputeShader(code->GetBufferPointer(),code->GetBufferSize(),nullptr,&shader),"Dolby Vision shader");
        D3D11_BUFFER_DESC d{};d.ByteWidth=sizeof(nvvideo_dovi);d.BindFlags=D3D11_BIND_CONSTANT_BUFFER;d.Usage=D3D11_USAGE_DEFAULT;
        HdrPipeline::hr(hdr->device->CreateBuffer(&d,nullptr,&metadata),"Dolby Vision per-frame constants");
        mapped=hdr->texture(hdr->c.width,hdr->c.height,DXGI_FORMAT_P010);
    }
    void process(ID3D11Texture2D *input, ID3D11Texture2D *output, const nvvideo_dovi &rpu, ID3D11Texture2D *el=nullptr) {
        validate(rpu);
        for(auto *t:{input,output}) {
            D3D11_TEXTURE2D_DESC d{};t->GetDesc(&d);
            if(d.Width!=hdr->c.width||d.Height!=hdr->c.height||d.Format!=DXGI_FORMAT_P010||d.ArraySize!=1||d.MipLevels!=1)throw std::runtime_error("Dolby Vision requires source-sized single-slice P010 textures");
        }
        auto y=hdr->srv(input,0),uv=hdr->srv(input,1);
        Ptr<ID3D11ShaderResourceView> ey,euv;
        if(rpu.nlq_active) {
            if(!el)throw std::runtime_error("Dolby Vision FEL requires its matching decoded enhancement layer");
            D3D11_TEXTURE2D_DESC d{};el->GetDesc(&d);
            if(d.Format!=DXGI_FORMAT_P010||d.ArraySize!=1||d.MipLevels!=1||d.Width!=rpu.el_width||d.Height!=rpu.el_height||!d.Width||!d.Height||
               (d.Width!=hdr->c.width&&2*d.Width!=hdr->c.width)||(d.Height!=hdr->c.height&&2*d.Height!=hdr->c.height))throw std::runtime_error("Unsupported Dolby Vision enhancement-layer texture dimensions/format");
            ey=hdr->srv(el,0);euv=hdr->srv(el,1);
        }
        auto outY=hdr->uav(mapped.Get(),0),outUV=hdr->uav(mapped.Get(),1);
        auto *ctx=hdr->c.context;auto *buffer=metadata.Get();
        ctx->UpdateSubresource(buffer,0,nullptr,&rpu,0,0);ctx->CSSetConstantBuffers(1,1,&buffer);
        ID3D11ShaderResourceView *inputs[6]={y.Get(),uv.Get(),nullptr,nullptr,ey.Get(),euv.Get()};
        ID3D11UnorderedAccessView *outputs[5]={nullptr,nullptr,nullptr,outY.Get(),outUV.Get()};
        hdr->dispatch(shader.Get(),inputs,outputs,false);
        ctx->CopyResource(output,mapped.Get());
    }
};
