// PQ/HLG GPU adapters. Original implementation using BT.2020 NCL and BT.2100.
// SDR model input is a proxy; its output never replaces the retained HDR base.
#pragma once
#include "video_enhancement.h"
#include <d3d11_3.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
#include <stdexcept>
#include <string>
#include <cstdio>
#include <cstring>
#include <utility>

struct HdrPipeline {
    using Texture = Microsoft::WRL::ComPtr<ID3D11Texture2D>;
    template<class T> using Ptr = Microsoft::WRL::ComPtr<T>;
    nvvideo_config c{};
    Ptr<ID3D11Device3> device;
    Ptr<ID3D11Buffer> constants;
    Ptr<ID3D11SamplerState> sampler;
    Ptr<ID3D11ComputeShader> decode_shader, encode_shader, restore_shader, pack_shader;
    Texture base, proxy, previous, current, generated, output;
    static void hr(HRESULT value, const char *operation) {
        if (FAILED(value)) {
            char detail[160]; snprintf(detail, sizeof(detail), "%s failed: 0x%08lx", operation, static_cast<unsigned long>(value));
            throw std::runtime_error(detail);
        }
    }
    static constexpr const char *source = R"HLSL(
cbuffer Params : register(b0) { uint w,h,ow,oh,fullRange,chroma,transfer,padding; }
Texture2D<float> inY : register(t0);
Texture2D<float2> inUV : register(t1);
Texture2D<float4> baseTex : register(t2);
Texture2D<float4> proxyTex : register(t3);
Texture2D<float4> enhancedTex : register(t4);
Texture2D<uint> packedTex : register(t5);
SamplerState linearClamp : register(s0);
RWTexture2D<float4> baseOut : register(u0);
RWTexture2D<unorm float4> proxyOut : register(u1);
RWTexture2D<uint> packedOut : register(u2);
RWTexture2D<uint> outY : register(u3);
RWTexture2D<uint2> outUV : register(u4);
float3 pqDecode(float3 v) {
    float3 p=pow(saturate(v),1.0/(2523.0/32.0));
    return 10000.0*pow(max(p-3424.0/4096.0,0)/max(2413.0/128.0-2392.0/128.0*p,1e-6),1.0/(2610.0/16384.0));
}
float3 pqEncode(float3 nits) {
    float3 p=pow(saturate(nits/10000.0),2610.0/16384.0);
    return pow((3424.0/4096.0+2413.0/128.0*p)/(1+2392.0/128.0*p),2523.0/32.0);
}
// Match Veyra's HLG reference display: 1000 nits, gamma 1.2, ideal black.
// The luminance-dependent OOTF is essential for colored HLG pixels.
float3 hlgDecode(float3 signal) {
    signal=saturate(signal);
    float3 scene=signal<=.5 ? signal*signal/3.0 : (exp((signal-.55991073)/.17883277)+.28466892)/12.0;
    return 1000.0*scene*pow(max(dot(scene,float3(.2627,.6780,.0593)),0),.2);
}
float3 hlgEncode(float3 nits) {
    float3 display=max(nits/1000.0,0);
    float luminance=dot(display,float3(.2627,.6780,.0593));
    float3 scene=luminance>0 ? display*pow(luminance,-1.0/6.0) : 0;
    return scene<=1.0/12.0 ? sqrt(3.0*scene) : .17883277*log(max(12.0*scene-.28466892,1e-6))+.55991073;
}
float3 sourceDecode(float3 signal) { return transfer==2 ? hlgDecode(signal) : pqDecode(signal); }
float3 sourceEncode(float3 nits) { return transfer==2 ? hlgEncode(nits) : pqEncode(nits); }
float3 to709(float3 c) {
    return mul(float3x3(1.660491,-.587641,-.072850,-.124550,1.132900,-.008349,-.018151,-.100579,1.118730),c);
}
float3 to2020(float3 c) {
    return mul(float3x3(.627404,.329283,.043313,.069097,.919540,.011362,.016391,.088013,.895595),c);
}
float3 srgbEncode(float3 v) { return v<=.0031308 ? 12.92*v : 1.055*pow(max(v,0),1.0/2.4)-.055; }
float3 srgbDecode(float3 v) { return v<=.04045 ? v/12.92 : pow((v+.055)/1.055,2.4); }
float3 readSource(uint2 p) {
    float2 offset=float2(chroma==2||chroma==3||chroma==5 ? .5 : 1,
                         chroma==1||chroma==3 ? 1 : chroma==4||chroma==5 ? 0 : .5);
    float y=inY[p]*(65535.0/64.0);
    float2 uv=inUV.SampleLevel(linearClamp,(float2(p)+offset)/float2(w,h),0)*(65535.0/64.0);
    y=fullRange ? y/1023.0 : (y-64.0)/876.0;
    uv=(uv-512.0)/(fullRange ? 1023.0 : 896.0);
    return float3(y+1.4746*uv.y,y-.164553*uv.x-.571353*uv.y,y+1.8814*uv.x);
}
[numthreads(16,16,1)] void decode(uint3 id:SV_DispatchThreadID) {
    if(id.x>=w||id.y>=h)return;
    float3 original=sourceDecode(readSource(id.xy));
    baseOut[id.xy]=float4(original,1);
    float3 v=max(to709(original)/203.0,0);
    // A fixed white point avoids exposure pumping between source frames.
    float3 mapped=v/(1+v);
    proxyOut[id.xy]=float4(srgbEncode(mapped),1);
}
[numthreads(16,16,1)] void encode10(uint3 id:SV_DispatchThreadID) {
    if(id.x>=w||id.y>=h)return;
    // VFG always sees PQ RGB10A2, including for an HLG source.
    float3 signal=readSource(id.xy);
    if(transfer==2)signal=pqEncode(hlgDecode(signal));
    uint3 rgb=(uint3)round(saturate(signal)*1023.0);
    packedOut[id.xy]=rgb.x|(rgb.y<<10)|(rgb.z<<20)|(3u<<30);
}
float3 restored(int2 p) {
    p=clamp(p,int2(0,0),int2(ow-1,oh-1));
    float2 uv=(float2(p)+.5)/float2(ow,oh);
    float3 original=baseTex.SampleLevel(linearClamp,uv,0).rgb;
    float3 proxy=srgbDecode(proxyTex.SampleLevel(linearClamp,uv,0).rgb);
    float3 neural=srgbDecode(enhancedTex[p].rgb);
    float luminance=dot(original,float3(.2627,.6780,.0593));
    float peak=max(proxy.r,max(proxy.g,proxy.b));
    float protection=saturate(luminance/4.0)*saturate((.98-peak)/.18);
    if(transfer==2)protection*=1-smoothstep(950.0,1000.0,max(original.r,max(original.g,original.b)));
    float3 delta=clamp(neural-proxy,-.5,.5)*203.0*protection;
    return sourceEncode(original+to2020(delta));
}
float3 unpacked(int2 p) {
    p=clamp(p,int2(0,0),int2(ow-1,oh-1));
    uint v=packedTex[p];
    float3 signal=float3(v&1023u,(v>>10)&1023u,(v>>20)&1023u)/1023.0;
    return transfer==2 ? hlgEncode(pqDecode(signal)) : signal;
}
float3 yuv(float3 rgb) {
    float y=dot(rgb,float3(.2627,.6780,.0593));
    return float3(y,(rgb.b-y)/1.8814,(rgb.r-y)/1.4746);
}
// Write exactly ten significant bits into the high bits of P010 words.
uint quantize(float v) { return ((uint)round(clamp(v,0,1023)))<<6; }
float3 picture(int2 p, bool packed) { return packed ? unpacked(p) : restored(p); }
void writeP010(uint2 p, bool packed) {
    float3 value=yuv(picture(int2(p),packed));
    outY[p]=quantize(value.x*(fullRange?1023.0:876.0)+(fullRange?0.0:64.0));
    if((p.x&1u)||(p.y&1u))return;
    float2 uv=0;
    // Downsample about the declared chroma center, including the left-edge case.
    bool halfX=chroma==2||chroma==3||chroma==5;
    bool halfY=chroma==0||chroma==2;
    int centerY=chroma==4||chroma==5 ? 1 : 0;
    for(int dy=halfY?0:centerY-1;dy<=(halfY?1:centerY+1);dy++)for(int dx=halfX?0:-1;dx<=1;dx++) {
        float wx=halfX?.5:dx==0?.5:.25;
        float wy=halfY?.5:dy==centerY?.5:.25;
        uv+=yuv(picture(int2(p)+int2(dx,dy),packed)).yz*wx*wy;
    }
    float2 code=uv*(fullRange?1023.0:896.0)+512.0;
    outUV[p/2]=uint2(quantize(code.x),quantize(code.y));
}
[numthreads(16,16,1)] void restore(uint3 id:SV_DispatchThreadID) {
    if(id.x<ow&&id.y<oh)writeP010(id.xy,false);
}
[numthreads(16,16,1)] void pack10(uint3 id:SV_DispatchThreadID) {
    if(id.x<ow&&id.y<oh)writeP010(id.xy,true);
}
)HLSL";
    Texture texture(unsigned width, unsigned height, DXGI_FORMAT format) {
        D3D11_TEXTURE2D_DESC desc{};
        desc.Width=width; desc.Height=height; desc.MipLevels=desc.ArraySize=1;
        desc.Format=format; desc.SampleDesc.Count=1;
        desc.BindFlags=D3D11_BIND_SHADER_RESOURCE|D3D11_BIND_UNORDERED_ACCESS;
        Texture result; hr(device->CreateTexture2D(&desc,nullptr,&result),"HDR texture"); return result;
    }
    Ptr<ID3D11ShaderResourceView> srv(ID3D11Texture2D *texture, int plane=-1) {
        Ptr<ID3D11ShaderResourceView> result;
        if(plane<0)hr(device->CreateShaderResourceView(texture,nullptr,&result),"HDR SRV");
        else {
            D3D11_SHADER_RESOURCE_VIEW_DESC1 d{}; d.Format=plane?DXGI_FORMAT_R16G16_UNORM:DXGI_FORMAT_R16_UNORM;
            d.ViewDimension=D3D11_SRV_DIMENSION_TEXTURE2D; d.Texture2D.MipLevels=1; d.Texture2D.PlaneSlice=plane;
            Ptr<ID3D11ShaderResourceView1> v; hr(device->CreateShaderResourceView1(texture,&d,&v),"HDR planar SRV"); hr(v.As(&result),"HDR SRV interface");
        }
        return result;
    }
    Ptr<ID3D11UnorderedAccessView> uav(ID3D11Texture2D *texture, int plane=-1) {
        Ptr<ID3D11UnorderedAccessView> result;
        if(plane<0)hr(device->CreateUnorderedAccessView(texture,nullptr,&result),"HDR UAV");
        else {
            D3D11_UNORDERED_ACCESS_VIEW_DESC1 d{}; d.Format=plane?DXGI_FORMAT_R16G16_UINT:DXGI_FORMAT_R16_UINT;
            d.ViewDimension=D3D11_UAV_DIMENSION_TEXTURE2D; d.Texture2D.PlaneSlice=plane;
            Ptr<ID3D11UnorderedAccessView1> v; hr(device->CreateUnorderedAccessView1(texture,&d,&v),"HDR planar UAV"); hr(v.As(&result),"HDR UAV interface");
        }
        return result;
    }
    void initialize(const nvvideo_config &config) {
        c=config; hr(c.device->QueryInterface(IID_PPV_ARGS(&device)),"HDR D3D11 device3");
        for(auto item : {std::pair{"decode",decode_shader.GetAddressOf()},{"encode10",encode_shader.GetAddressOf()},{"restore",restore_shader.GetAddressOf()},{"pack10",pack_shader.GetAddressOf()}}) {
            Ptr<ID3DBlob> code,error;
            HRESULT result=D3DCompile(source,strlen(source),"PQ/HLG GPU pipeline",nullptr,nullptr,item.first,"cs_5_0",D3DCOMPILE_ENABLE_STRICTNESS,0,&code,&error);
            if(FAILED(result))throw std::runtime_error(error?std::string(static_cast<const char*>(error->GetBufferPointer()),error->GetBufferSize()):"HDR shader compilation failed");
            hr(device->CreateComputeShader(code->GetBufferPointer(),code->GetBufferSize(),nullptr,item.second),"HDR compute shader");
        }
        unsigned values[8]={c.width,c.height,c.out_width,c.out_height,c.full_range,c.chroma_location,c.transfer,0};
        D3D11_BUFFER_DESC d{};d.ByteWidth=sizeof(values);d.BindFlags=D3D11_BIND_CONSTANT_BUFFER;d.Usage=D3D11_USAGE_IMMUTABLE;
        D3D11_SUBRESOURCE_DATA data{values,0,0};hr(device->CreateBuffer(&d,&data,&constants),"HDR constants");
        D3D11_SAMPLER_DESC s{};s.Filter=D3D11_FILTER_MIN_MAG_MIP_LINEAR;s.AddressU=s.AddressV=s.AddressW=D3D11_TEXTURE_ADDRESS_CLAMP;s.MaxLOD=D3D11_FLOAT32_MAX;
        hr(device->CreateSamplerState(&s,&sampler),"HDR sampler");
        output=texture(c.out_width,c.out_height,DXGI_FORMAT_P010);
        if(c.mode==2) {
            previous=texture(c.width,c.height,DXGI_FORMAT_R32_UINT);
            current=texture(c.width,c.height,DXGI_FORMAT_R32_UINT);
            generated=texture(c.width,c.height,DXGI_FORMAT_R32_UINT);
        } else {
            base=texture(c.width,c.height,DXGI_FORMAT_R16G16B16A16_FLOAT);
            proxy=texture(c.width,c.height,DXGI_FORMAT_B8G8R8A8_UNORM);
        }
    }
    void dispatch(ID3D11ComputeShader *shader, ID3D11ShaderResourceView **inputs, ID3D11UnorderedAccessView **outputs, bool scaled) {
        auto *ctx=c.context; ID3D11Buffer *buffer=constants.Get(); ID3D11SamplerState *state=sampler.Get();
        ctx->CSSetShader(shader,nullptr,0);ctx->CSSetConstantBuffers(0,1,&buffer);ctx->CSSetSamplers(0,1,&state);
        ctx->CSSetShaderResources(0,6,inputs);ctx->CSSetUnorderedAccessViews(0,5,outputs,nullptr);
        ctx->Dispatch(((scaled?c.out_width:c.width)+15)/16,((scaled?c.out_height:c.height)+15)/16,1);
        ID3D11ShaderResourceView *empty_srvs[6]{};ID3D11UnorderedAccessView *empty_uavs[5]{};
        ctx->CSSetShaderResources(0,6,empty_srvs);ctx->CSSetUnorderedAccessViews(0,5,empty_uavs,nullptr);
    }
    void input(ID3D11Texture2D *input, ID3D11Texture2D *packed=nullptr) {
        auto y=srv(input,0),uv=srv(input,1);
        Ptr<ID3D11UnorderedAccessView> a,b,p;
        if(packed)p=uav(packed);else{a=uav(base.Get());b=uav(proxy.Get());}
        ID3D11ShaderResourceView *inputs[6]={y.Get(),uv.Get(),nullptr,nullptr,nullptr,nullptr};
        ID3D11UnorderedAccessView *outputs[5]={a.Get(),b.Get(),p.Get(),nullptr,nullptr};
        dispatch(packed?encode_shader.Get():decode_shader.Get(),inputs,outputs,false);
    }
    void finish(ID3D11Texture2D *neural, bool packed) {
        auto y=uav(output.Get(),0),uv=uav(output.Get(),1);
        Ptr<ID3D11ShaderResourceView> a,b,n;
        if(packed)n=srv(neural);else{a=srv(base.Get());b=srv(proxy.Get());n=srv(neural);}
        ID3D11ShaderResourceView *inputs[6]={nullptr,nullptr,a.Get(),b.Get(),packed?nullptr:n.Get(),packed?n.Get():nullptr};
        ID3D11UnorderedAccessView *outputs[5]={nullptr,nullptr,nullptr,y.Get(),uv.Get()};
        dispatch(packed?pack_shader.Get():restore_shader.Get(),inputs,outputs,true);
    }
};
