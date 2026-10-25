/* MediaStationGo optional video providers, ABI 7. Host owns PTS and presentation. */
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <d3d11.h>
#define NVVIDEO_ABI 7
#ifdef __cplusplus
extern "C" {
#endif
struct nvvideo_session;
/* Per-frame RPU mapping, already parsed by mpv/libplacebo. GPU constants are
 * float4-aligned; no third-party structs cross the DLL boundary. */
struct nvvideo_dovi_segment {
    float polynomial[4]; /* xyz coefficients; w: 0 polynomial, 1..3 MMR order */
    float mmr[3][2][4]; /* xyz, xy/xz/yz/xyz for each order */
};
struct nvvideo_dovi_component {
    float pivots[9][4];
    struct nvvideo_dovi_segment segments[8];
    uint32_t num_pivots, padding[3];
};
struct nvvideo_dovi {
    float nonlinear[3][4]; /* xyz matrix row, w input offset */
    float linear[3][4]; /* BT.2020 HPE LMS -> linear BT.2020 RGB */
    struct nvvideo_dovi_component comp[3];
    float nlq[3][4]; /* integer EL offset, slope, threshold, residual bound */
    uint32_t el_width, el_height, el_chroma, nlq_active;
};
enum nvvideo_transfer { NVVIDEO_SDR = 0, NVVIDEO_PQ = 1, NVVIDEO_HLG = 2 };
struct nvvideo_config {
    uint32_t abi;
    uint32_t mode; /* 1: RTX Video SDK VSR, 2: Maxine VFG, 3: DLSSNR */
    uint32_t width, height, out_width, out_height;
    ID3D11Device *device;
    ID3D11DeviceContext *context;
    const wchar_t *runtime_directory;
    const wchar_t *cache_directory;
    uint32_t vsr_quality, vfg_quality, multiplier, scene_detection;
    int32_t nr_style, nr_auto_mask, nr_ui_correction;
    float nr_intensity, nr_tone, nr_structure, nr_skin;
    uint32_t nr_target;
    uint32_t transfer, full_range, chroma_location; /* transfer: nvvideo_transfer; chroma: 0 left, 1 top-left, 2 center, 3 top, 4 bottom-left, 5 bottom */
};
#ifdef NVVIDEO_BUILD
#define NVVIDEO_API __declspec(dllexport)
#else
#define NVVIDEO_API
#endif
NVVIDEO_API uint32_t __cdecl nvvideo_abi(void);
NVVIDEO_API struct nvvideo_session *__cdecl nvvideo_create(const struct nvvideo_config *, char *, size_t);
/* Apply the current frame's RPU exactly once, before enhancement. Both textures
 * are source-sized P010. Host retains scene metadata and raw RPU side data,
 * and marks the resulting pixels BT.2020 NCL/PQ to prevent double reshaping. */
NVVIDEO_API int __cdecl nvvideo_map_dovi(struct nvvideo_session *, ID3D11Texture2D *, ID3D11Texture2D *, ID3D11Texture2D *, const struct nvvideo_dovi *, char *, size_t);
/* Single-slice BGRA8 (SDR) or P010 (PQ/HLG). Host retains source color metadata. */
NVVIDEO_API int __cdecl nvvideo_process(struct nvvideo_session *, ID3D11Texture2D *, ID3D11Texture2D *, ID3D11Texture2D *, uint32_t, char *, size_t);
/* multiplier=0: index starts at 1 for each new source pair; 0 < timestep < 1. */
NVVIDEO_API int __cdecl nvvideo_process_timestep(struct nvvideo_session *, ID3D11Texture2D *, ID3D11Texture2D *, ID3D11Texture2D *, uint32_t, float, char *, size_t);
NVVIDEO_API void __cdecl nvvideo_destroy(struct nvvideo_session *);
#ifdef __cplusplus
}
#endif
