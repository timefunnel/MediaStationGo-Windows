cbuffer ProbeConstants : register(b0)
{
    uint source_width;
    uint source_height;
    uint padded_width;
    uint padded_height;
    uint input_plane_stride;
    uint output_plane_stride;
    uint matrix_mode;
    uint limited_range;
};

Texture2D<float> input_y0 : register(t0);
Texture2D<float> input_y1 : register(t1);
Texture2D<float2> input_uv0 : register(t2);
Texture2D<float2> input_uv1 : register(t3);
RWByteAddressBuffer rife_input : register(u0);

ByteAddressBuffer rife_output : register(t4);
RWTexture2D<unorm float> output_y : register(u0);
RWTexture2D<unorm float2> output_uv : register(u1);

float code10(float value)
{
    return value * (65535.0 / 64.0);
}

float2 code10(float2 value)
{
    return value * (65535.0 / 64.0);
}

float3 decode_yuv(float y_code, float2 uv_code)
{
    float y;
    float cb;
    float cr;
    if (limited_range != 0) {
        y = (y_code - 64.0) / 876.0;
        cb = (uv_code.x - 512.0) / 896.0;
        cr = (uv_code.y - 512.0) / 896.0;
    } else {
        y = y_code / 1023.0;
        cb = (uv_code.x - 512.0) / 1023.0;
        cr = (uv_code.y - 512.0) / 1023.0;
    }
    if (matrix_mode == 1) {
        return float3(
            y + 1.4746 * cr,
            y - 0.1645531268 * cb - 0.5713531268 * cr,
            y + 1.8814 * cb);
    }
    return float3(
        y + 1.5748 * cr,
        y - 0.1873242729 * cb - 0.4681242729 * cr,
        y + 1.8556 * cb);
}

float3 load_rgb(uint frame_index, uint2 position)
{
    if (position.x >= source_width || position.y >= source_height)
        return 0.0;
    float y = frame_index == 0
        ? input_y0.Load(int3(position, 0))
        : input_y1.Load(int3(position, 0));
    float2 uv = frame_index == 0
        ? input_uv0.Load(int3(position / 2, 0))
        : input_uv1.Load(int3(position / 2, 0));
    return decode_yuv(code10(y), code10(uv));
}

void store_half_pair(uint channel, uint pair_index, float left, float right)
{
    uint packed = f32tof16(left) | (f32tof16(right) << 16);
    rife_input.Store(channel * input_plane_stride * 2 + pair_index * 4, packed);
}

[numthreads(8, 8, 1)]
void prepare_input(uint3 dispatch_id : SV_DispatchThreadID)
{
    uint pair_x = dispatch_id.x;
    uint y = dispatch_id.y;
    uint pair_width = padded_width / 2;
    if (pair_x >= pair_width || y >= padded_height)
        return;
    uint x0 = pair_x * 2;
    uint x1 = x0 + 1;
    float3 frame0_left = load_rgb(0, uint2(x0, y));
    float3 frame0_right = load_rgb(0, uint2(x1, y));
    float3 frame1_left = load_rgb(1, uint2(x0, y));
    float3 frame1_right = load_rgb(1, uint2(x1, y));
    uint pair_index = y * pair_width + pair_x;
    store_half_pair(0, pair_index, frame0_left.r, frame0_right.r);
    store_half_pair(1, pair_index, frame0_left.g, frame0_right.g);
    store_half_pair(2, pair_index, frame0_left.b, frame0_right.b);
    store_half_pair(3, pair_index, frame1_left.r, frame1_right.r);
    store_half_pair(4, pair_index, frame1_left.g, frame1_right.g);
    store_half_pair(5, pair_index, frame1_left.b, frame1_right.b);
    store_half_pair(6, pair_index, 0.5, 0.5);
    store_half_pair(7, pair_index,
                    2.0 * x0 / (padded_width - 1.0) - 1.0,
                    2.0 * x1 / (padded_width - 1.0) - 1.0);
    float vertical = 2.0 * y / (padded_height - 1.0) - 1.0;
    store_half_pair(8, pair_index, vertical, vertical);
    store_half_pair(9, pair_index,
                    2.0 / (padded_width - 1.0),
                    2.0 / (padded_width - 1.0));
    store_half_pair(10, pair_index,
                    2.0 / (padded_height - 1.0),
                    2.0 / (padded_height - 1.0));
}

float load_output_channel(uint channel, uint2 position)
{
    uint linear_index = position.y * padded_width + position.x;
    uint byte_offset = channel * output_plane_stride * 2 + (linear_index & ~1u) * 2;
    uint packed = rife_output.Load(byte_offset);
    uint bits = (linear_index & 1u) == 0 ? packed & 0xffff : packed >> 16;
    return f16tof32(bits);
}

float3 load_output_rgb(uint2 position)
{
    return float3(
        load_output_channel(0, position),
        load_output_channel(1, position),
        load_output_channel(2, position));
}

float3 encode_yuv(float3 rgb)
{
    float y;
    float cb;
    float cr;
    if (matrix_mode == 1) {
        y = dot(rgb, float3(0.2627, 0.6780, 0.0593));
        cb = (rgb.b - y) / 1.8814;
        cr = (rgb.r - y) / 1.4746;
    } else {
        y = dot(rgb, float3(0.2126, 0.7152, 0.0722));
        cb = (rgb.b - y) / 1.8556;
        cr = (rgb.r - y) / 1.5748;
    }
    return float3(y, cb, cr);
}

float p010_unorm(float code)
{
    return round(clamp(code, 0.0, 1023.0)) * (64.0 / 65535.0);
}

[numthreads(8, 8, 1)]
void write_output(uint3 dispatch_id : SV_DispatchThreadID)
{
    uint pair_x = dispatch_id.x;
    uint y = dispatch_id.y;
    uint pair_width = (source_width + 1) / 2;
    if (pair_x >= pair_width || y >= source_height)
        return;
    uint x0 = pair_x * 2;
    uint x1 = min(x0 + 1, source_width - 1);
    float3 yuv0 = encode_yuv(load_output_rgb(uint2(x0, y)));
    float3 yuv1 = encode_yuv(load_output_rgb(uint2(x1, y)));
    float y_code0 = limited_range != 0 ? yuv0.x * 876.0 + 64.0 : yuv0.x * 1023.0;
    float y_code1 = limited_range != 0 ? yuv1.x * 876.0 + 64.0 : yuv1.x * 1023.0;
    output_y[uint2(x0, y)] = p010_unorm(y_code0);
    output_y[uint2(x1, y)] = p010_unorm(y_code1);

    if ((y & 1u) == 0) {
        uint y1 = min(y + 1, source_height - 1);
        float3 lower0 = encode_yuv(load_output_rgb(uint2(x0, y1)));
        float3 lower1 = encode_yuv(load_output_rgb(uint2(x1, y1)));
        float cb = (yuv0.y + yuv1.y + lower0.y + lower1.y) * 0.25;
        float cr = (yuv0.z + yuv1.z + lower0.z + lower1.z) * 0.25;
        float cb_code = limited_range != 0 ? cb * 896.0 + 512.0 : cb * 1023.0 + 512.0;
        float cr_code = limited_range != 0 ? cr * 896.0 + 512.0 : cr * 1023.0 + 512.0;
        output_uv[uint2(pair_x, y / 2)] = float2(
            p010_unorm(cb_code), p010_unorm(cr_code));
    }
}
