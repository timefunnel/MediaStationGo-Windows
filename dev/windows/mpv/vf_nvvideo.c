/* Optional native VSR/VFG processing. mpv owns audio, PTS, subtitles and EOF. */
#include "config.h"
#include <math.h>
#include <stdint.h>
#include <windows.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_d3d11va.h>
#include "common/common.h"
#include "common/msg.h"
#include "common/tags.h"
#include "filters/filter.h"
#include "filters/filter_internal.h"
#include "filters/user_filters.h"
#include "options/m_option.h"
#include "osdep/io.h"
#include "video/mp_image.h"
#include "video/mp_image_pool.h"
#include "video/hwdec.h"
#include "video/d3d.h"
#include "video_enhancement.h"

struct opts {
    int mode, quality, target_height, multiplier, scene_detection, nr_style, nr_target, nr_auto_mask, nr_ui_correction;
    float nr_intensity, nr_tone, nr_structure, nr_skin;
    char *bridge, *runtime, *cache;
};
struct priv {
    struct opts *opts;
    HMODULE module;
    struct nvvideo_session *session;
    struct nvvideo_session *(__cdecl *create)(const struct nvvideo_config *, char *, size_t);
    int (__cdecl *process)(struct nvvideo_session *, ID3D11Texture2D *, ID3D11Texture2D *, ID3D11Texture2D *, uint32_t, char *, size_t);
    void (__cdecl *destroy_session)(struct nvvideo_session *);
    ID3D11Device *device;
    ID3D11DeviceContext *context;
    ID3D11Texture2D *previous;
    struct mp_image *pending[8];
    int pending_count, pending_index, out_width, out_height;
    AVBufferRef *hw_pool;
    struct mp_image_params params;
    double previous_pts;
    uint64_t sdk_frames, input_frames, resets;
    uint64_t session_frames;
    double process_ms, max_process_ms;
    bool failed;
};

static wchar_t *utf8_to_wide(void *parent, const char *value) {
    if (!value || !value[0]) return NULL;
    int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value, -1, NULL, 0);
    if (!count) return NULL;
    wchar_t *wide = talloc_array(parent, wchar_t, count);
    if (!MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value, -1, wide, count)) { talloc_free(wide); return NULL; }
    return wide;
}

static void reset(struct mp_filter *f) {
    struct priv *p = f->priv;
    for (int i = 0; i < 8; i++) TA_FREEP(&p->pending[i]);
    p->pending_count = p->pending_index = 0;
    if (p->previous) ID3D11Texture2D_Release(p->previous);
    p->previous = NULL;
    if (p->session) p->destroy_session(p->session);
    p->session = NULL;
    p->session_frames = 0;
    av_buffer_unref(&p->hw_pool);
    p->previous_pts = MP_NOPTS_VALUE;
    p->resets++;
}
static void fail(struct mp_filter *f, const char *detail) {
    struct priv *p = f->priv; p->failed = true;
    MP_ERR(f, "Native enhancement failed mode=%d: %s\n", p->opts->mode, detail);
    reset(f); mp_filter_internal_mark_failed(f);
}
static ID3D11Texture2D *single_texture(struct mp_filter *f, struct mp_image *image) {
    struct priv *p = f->priv;
    ID3D11Texture2D *source = (void *)image->planes[0], *texture = NULL;
    D3D11_TEXTURE2D_DESC desc; ID3D11Texture2D_GetDesc(source, &desc);
    desc.Width = image->w; desc.Height = image->h; desc.ArraySize = desc.MipLevels = 1;
    desc.Usage = D3D11_USAGE_DEFAULT; desc.CPUAccessFlags = desc.MiscFlags = 0;
    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    HRESULT hr = ID3D11Device_CreateTexture2D(p->device, &desc, NULL, &texture);
    if (FAILED(hr)) return NULL;
    D3D11_BOX box = {0, 0, 0, image->w, image->h, 1};
    ID3D11DeviceContext_CopySubresourceRegion(p->context, (void *)texture, 0, 0, 0, 0, (void *)source, (UINT)(uintptr_t)image->planes[1], &box);
    return texture;
}
static struct mp_image *output_frame(struct mp_filter *f, struct mp_image *input) {
    struct priv *p = f->priv;
    AVHWFramesContext *frames = (void *)input->hwctx->data;
    if (!mp_update_av_hw_frames_pool(&p->hw_pool, frames->device_ref, IMGFMT_D3D11, IMGFMT_BGRA, p->out_width, p->out_height, false)) return NULL;
    AVFrame *frame = av_frame_alloc();
    if (!frame) return NULL;
    if (av_hwframe_get_buffer(p->hw_pool, frame, 0) < 0) { av_frame_free(&frame); return NULL; }
    struct mp_image *output = mp_image_from_av_frame(frame); av_frame_free(&frame);
    if (output) { mp_image_copy_attributes(output, input); output->params.crop = (struct mp_rect){0, 0, p->out_width, p->out_height}; }
    return output;
}
static bool initialize(struct mp_filter *f, struct mp_image *input) {
    struct priv *p = f->priv;
    if (!p->module) {
        wchar_t *path = utf8_to_wide(f, p->opts->bridge);
        if (!path) return false;
        p->module = LoadLibraryExW(path, NULL, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32); talloc_free(path);
        if (!p->module) return false;
        uint32_t (__cdecl *abi)(void) = (void *)GetProcAddress(p->module, "nvvideo_abi");
        p->create = (void *)GetProcAddress(p->module, "nvvideo_create");
        p->process = (void *)GetProcAddress(p->module, "nvvideo_process");
        p->destroy_session = (void *)GetProcAddress(p->module, "nvvideo_destroy");
        if (!abi || abi() != 2 || !p->create || !p->process || !p->destroy_session) return false;
    }
    if (p->context) ID3D11DeviceContext_Release(p->context);
    if (p->device) ID3D11Device_Release(p->device);
    ID3D11Texture2D_GetDevice((ID3D11Texture2D *)input->planes[0], &p->device);
    ID3D11Device_GetImmediateContext(p->device, &p->context);
    wchar_t *runtime = utf8_to_wide(f, p->opts->runtime), *cache = utf8_to_wide(f, p->opts->cache);
    if (!runtime || !cache) { talloc_free(runtime); talloc_free(cache); return false; }
    p->out_width = input->w; p->out_height = input->h;
    if (p->opts->mode == 1) {
        // Target fits a 16:9 bounding box, preserves the source aspect ratio,
        // and never reduces the source. Zero retains the previous 2x default.
        double scale = p->opts->target_height ? fmin(p->opts->target_height * 16.0 / 9.0 / input->w, (double)p->opts->target_height / input->h) : 2.0;
        if (scale > 1.0) { p->out_width = ((int)(input->w * scale + 1e-6)) & ~1; p->out_height = ((int)(input->h * scale + 1e-6)) & ~1; }
    }
    struct nvvideo_config config = {
        .abi = 2, .mode = p->opts->mode, .width = input->w, .height = input->h,
        .out_width = p->out_width, .out_height = p->out_height, .device = p->device, .context = p->context,
        .runtime_directory = runtime, .cache_directory = cache,
        .vsr_quality = p->opts->mode == 1 && p->opts->quality >= 0 ? p->opts->quality : 4,
        .vfg_quality = p->opts->mode == 2 && p->opts->quality >= 0 ? p->opts->quality : 0,
        .multiplier = p->opts->multiplier, .scene_detection = p->opts->scene_detection,
        .nr_style = p->opts->nr_style, .nr_auto_mask = p->opts->nr_auto_mask, .nr_ui_correction = p->opts->nr_ui_correction,
        .nr_intensity = p->opts->nr_intensity, .nr_tone = p->opts->nr_tone, .nr_structure = p->opts->nr_structure, .nr_skin = p->opts->nr_skin,
        .nr_target = p->opts->nr_target,
    };
    char detail[512] = {0}; p->session = p->create(&config, detail, sizeof(detail));
    talloc_free(runtime); talloc_free(cache);
    if (!p->session) MP_ERR(f, "%s\n", detail);
    p->params = input->params;
    return p->session != NULL;
}
static void process(struct mp_filter *f) {
    struct priv *p = f->priv;
    if (p->failed || !mp_pin_in_needs_data(f->ppins[1])) return;
    if (p->pending_index < p->pending_count) {
        struct mp_image *next = p->pending[p->pending_index]; p->pending[p->pending_index++] = NULL;
        mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_VIDEO, next)); return;
    }
    struct mp_frame frame = mp_pin_out_read(f->ppins[0]);
    if (!frame.type) return;
    if (frame.type != MP_FRAME_VIDEO) { if (frame.type == MP_FRAME_EOF) reset(f); mp_pin_in_write(f->ppins[1], frame); return; }
    struct mp_image *input = frame.data;
    if (input->imgfmt != IMGFMT_D3D11 || input->params.hw_subfmt != IMGFMT_BGRA || !input->hwctx || input->params.color.transfer == PL_COLOR_TRC_PQ || input->params.color.transfer == PL_COLOR_TRC_HLG || (input->fields & MP_IMGFIELD_INTERLACED)) {
        mp_frame_unref(&frame); fail(f, "Only progressive SDR D3D11 BGRA8 input is supported"); return;
    }
    if (p->session && !mp_image_params_static_equal(&p->params, &input->params)) reset(f);
    if (!p->session && !initialize(f, input)) { mp_frame_unref(&frame); fail(f, "Cannot initialize installed native provider; see preceding SDK error"); return; }
    p->input_frames++;
    ID3D11Texture2D *current = single_texture(f, input);
    if (!current) { mp_frame_unref(&frame); fail(f, "Cannot allocate an input texture"); return; }
    if (p->opts->mode == 2 && !p->previous) {
        p->previous = current; p->previous_pts = input->pts;
        input->pkt_duration /= p->opts->multiplier;
        mp_pin_in_write(f->ppins[1], frame); return;
    }
    if (p->opts->mode == 2 && (input->pts == MP_NOPTS_VALUE || p->previous_pts == MP_NOPTS_VALUE || input->pts <= p->previous_pts || input->pts - p->previous_pts > 0.25)) {
        ID3D11Texture2D_Release(current); mp_frame_unref(&frame); fail(f, "VFG requires strictly increasing source PTS with intervals no greater than 250 ms"); return;
    }
    int count = p->opts->mode == 2 ? p->opts->multiplier - 1 : 1;
    p->pending_index = p->pending_count = 0;
    ID3D11Texture2D *result = NULL;
    D3D11_TEXTURE2D_DESC desc; ID3D11Texture2D_GetDesc(current, &desc);
    desc.Width = p->out_width; desc.Height = p->out_height;
    if (p->opts->mode == 3) desc.BindFlags |= D3D11_BIND_UNORDERED_ACCESS;
    HRESULT hr = ID3D11Device_CreateTexture2D(p->device, &desc, NULL, &result);
    char detail[512] = {0};
    bool ok = SUCCEEDED(hr);
    for (int i = 1; ok && i <= count; i++) {
        struct mp_image *output = output_frame(f, input);
        LARGE_INTEGER before, after, frequency; QueryPerformanceFrequency(&frequency); QueryPerformanceCounter(&before);
        ok = output && p->process(p->session, p->previous, current, result, i, detail, sizeof(detail));
        QueryPerformanceCounter(&after);
        double duration = (after.QuadPart - before.QuadPart) * 1000.0 / frequency.QuadPart;
        if (!ok) { TA_FREEP(&output); break; }
        p->process_ms += duration; p->max_process_ms = fmax(p->max_process_ms, duration);
        ID3D11DeviceContext_CopySubresourceRegion(p->context, (void *)output->planes[0], (UINT)(uintptr_t)output->planes[1], 0, 0, 0, (void *)result, 0, NULL);
        p->sdk_frames++;
        p->session_frames++;
        if (p->opts->mode == 2) {
            output->pkt_duration = (input->pts - p->previous_pts) / p->opts->multiplier;
            output->pts = p->previous_pts + output->pkt_duration * i;
            input->pkt_duration = output->pkt_duration;
        }
        p->pending[p->pending_count++] = output;
    }
    if (ok) {
        if (p->opts->mode == 2) {
            p->pending[p->pending_count++] = input; frame = MP_NO_FRAME;
            ID3D11Texture2D_Release(p->previous); p->previous = current; current = NULL; p->previous_pts = input->pts;
        }
        struct mp_image *next = p->pending[p->pending_index]; p->pending[p->pending_index++] = NULL;
        mp_pin_in_write(f->ppins[1], MAKE_FRAME(MP_FRAME_VIDEO, next));
    }
    if (result) ID3D11Texture2D_Release(result);
    if (current) ID3D11Texture2D_Release(current);
    mp_frame_unref(&frame);
    if (!ok) fail(f, detail[0] ? detail : "Native output allocation failed");
}
static bool command(struct mp_filter *f, struct mp_filter_command *cmd) {
    if (cmd->type != MP_FILTER_COMMAND_GET_META) return false;
    struct priv *p = f->priv; struct mp_tags *tags = talloc_zero(NULL, struct mp_tags);
    mp_tags_set_str(tags, "status", p->failed ? "failed" : p->session_frames ? "active" : "initializing");
    mp_tags_set_str(tags, "backend", p->opts->mode == 1 ? "rtx-video" : p->opts->mode == 2 ? "nvidia-vfg" : "dlssnr");
    mp_tags_set_str(tags, "multiplier", mp_tprintf(30, "%d", p->opts->mode == 2 ? p->opts->multiplier : 1));
    mp_tags_set_str(tags, "quality", mp_tprintf(30, "%d", p->opts->quality));
    mp_tags_set_str(tags, "avg-process-ms", mp_tprintf(40, "%.3f", p->sdk_frames ? p->process_ms / p->sdk_frames : 0));
    mp_tags_set_str(tags, "max-process-ms", mp_tprintf(40, "%.3f", p->max_process_ms));
    mp_tags_set_str(tags, "sdk-frames", mp_tprintf(80, "%llu", (unsigned long long)p->sdk_frames));
    mp_tags_set_str(tags, "resets", mp_tprintf(80, "%llu", (unsigned long long)p->resets));
    *(struct mp_tags **)cmd->res = tags; return true;
}
static void destroy(struct mp_filter *f) {
    struct priv *p = f->priv; reset(f);
    if (p->context) ID3D11DeviceContext_Release(p->context);
    if (p->device) ID3D11Device_Release(p->device);
    if (p->module) FreeLibrary(p->module);
}
static const struct mp_filter_info info = {.name = "nvvideo", .process = process, .reset = reset, .destroy = destroy, .command = command, .priv_size = sizeof(struct priv)};
static struct mp_filter *create(struct mp_filter *parent, void *options) {
    struct mp_filter *f = mp_filter_create(parent, &info);
    if (!f) { talloc_free(options); return NULL; }
    struct priv *p = f->priv; p->opts = talloc_steal(p, options); p->previous_pts = MP_NOPTS_VALUE;
    mp_filter_add_pin(f, MP_PIN_IN, "in"); mp_filter_add_pin(f, MP_PIN_OUT, "out"); return f;
}
#define OPT_BASE_STRUCT struct opts
static const m_option_t options[] = {
    {"mode", OPT_INT(mode), M_RANGE(1, 3)}, {"quality", OPT_INT(quality), M_RANGE(-1, 4)},
    {"target-height", OPT_INT(target_height), M_RANGE(0, 4320)}, {"multiplier", OPT_INT(multiplier), M_RANGE(2, 8)},
    {"scene-detection", OPT_INT(scene_detection), M_RANGE(0, 1)}, {"nr-style", OPT_INT(nr_style), M_RANGE(0, 2)},
    {"nr-target", OPT_INT(nr_target), M_RANGE(0, 1440)},
    {"nr-intensity", OPT_FLOAT(nr_intensity), M_RANGE(0, 1)}, {"nr-tone", OPT_FLOAT(nr_tone), M_RANGE(0, 1)},
    {"nr-structure", OPT_FLOAT(nr_structure), M_RANGE(0, 1)}, {"nr-skin", OPT_FLOAT(nr_skin), M_RANGE(-1, 2)},
    {"nr-auto-mask", OPT_INT(nr_auto_mask), M_RANGE(0, 1)}, {"nr-ui-correction", OPT_INT(nr_ui_correction), M_RANGE(0, 1)},
    {"bridge", OPT_STRING(bridge)}, {"runtime", OPT_STRING(runtime)}, {"cache", OPT_STRING(cache)}, {0}};
const struct mp_user_filter_entry vf_nvvideo = {.desc = {.name = "nvvideo", .description = "Optional NVIDIA video SDK provider", .priv_size = sizeof(struct opts), .priv_defaults = &(const struct opts){.mode = 1, .quality = -1, .multiplier = 2, .scene_detection = 1, .nr_target = 1080, .nr_intensity = 1, .nr_tone = 1, .nr_structure = 1, .nr_skin = -1}, .options = options}, .create = create};
