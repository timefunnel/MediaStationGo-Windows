/* MediaStationGo optional video providers, ABI 4. Host owns PTS and presentation. */
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <d3d11.h>
#ifdef __cplusplus
extern "C" {
#endif
struct nvvideo_session;
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
/* Single-slice BGRA8 (SDR) or P010 (PQ/HLG). Host retains source color metadata. */
NVVIDEO_API int __cdecl nvvideo_process(struct nvvideo_session *, ID3D11Texture2D *, ID3D11Texture2D *, ID3D11Texture2D *, uint32_t, char *, size_t);
NVVIDEO_API void __cdecl nvvideo_destroy(struct nvvideo_session *);
#ifdef __cplusplus
}
#endif
