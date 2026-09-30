#include "vd_decoder.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavutil/error.h>
#include <libavutil/hwcontext.h>
#include <libavutil/pixdesc.h>

struct VdDecoder {
    AVCodecContext *ctx;
    AVPacket *pkt;
    AVFrame *frame;
    AVFrame *sw;
    AVBufferRef *hw_device;
    enum AVPixelFormat hw_pix_fmt;
    char hw_name[32];
    char desc[96];
};

/* Prefer the hardware format; if the device cannot handle this stream (FFmpeg retries get_format
 * after a failed hwaccel init, e.g. HEVC 4:4:4 on AMD), fall back to the first software format. */
static enum AVPixelFormat pick_format(AVCodecContext *ctx, const enum AVPixelFormat *fmts) {
    VdDecoder *d = ctx->opaque;
    const enum AVPixelFormat *p;
    if (d->hw_pix_fmt != AV_PIX_FMT_NONE) {
        for (p = fmts; *p != AV_PIX_FMT_NONE; p++) {
            if (*p == d->hw_pix_fmt) return *p;
        }
    }
    for (p = fmts; *p != AV_PIX_FMT_NONE; p++) {
        const AVPixFmtDescriptor *desc = av_pix_fmt_desc_get(*p);
        if (desc && !(desc->flags & AV_PIX_FMT_FLAG_HWACCEL)) return *p;
    }
    return AV_PIX_FMT_NONE;
}

static const char *const auto_devices[] = {
#if defined(_WIN32)
    "d3d11va", "vulkan",
#elif defined(__APPLE__)
    "videotoolbox",
#else
    "vaapi", "vulkan",
#endif
    NULL,
};

static void try_hw(VdDecoder *d, const AVCodec *codec, const char *hwaccel) {
    const char *single[] = {hwaccel, NULL};
    const char *const *cands = strcmp(hwaccel, "auto") == 0 ? auto_devices : single;
    for (int i = 0; cands[i]; i++) {
        enum AVHWDeviceType type = av_hwdevice_find_type_by_name(cands[i]);
        if (type == AV_HWDEVICE_TYPE_NONE) continue;
        enum AVPixelFormat fmt = AV_PIX_FMT_NONE;
        for (int j = 0;; j++) {
            const AVCodecHWConfig *cfg = avcodec_get_hw_config(codec, j);
            if (!cfg) break;
            if ((cfg->methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) && cfg->device_type == type) {
                fmt = cfg->pix_fmt;
                break;
            }
        }
        if (fmt == AV_PIX_FMT_NONE) continue;
        if (av_hwdevice_ctx_create(&d->hw_device, type, NULL, NULL, 0) < 0) continue;
        d->ctx->hw_device_ctx = av_buffer_ref(d->hw_device);
        d->hw_pix_fmt = fmt;
        snprintf(d->hw_name, sizeof d->hw_name, "%s", cands[i]);
        return;
    }
}

int vd_decoder_open(VdDecoder **out, const char *hwaccel, int threads, char *err, size_t err_len) {
    *out = NULL;
    const AVCodec *codec = avcodec_find_decoder(AV_CODEC_ID_HEVC);
    if (!codec) {
        snprintf(err, err_len, "FFmpeg was built without an HEVC decoder");
        return AVERROR_DECODER_NOT_FOUND;
    }
    VdDecoder *d = calloc(1, sizeof *d);
    if (!d) return AVERROR(ENOMEM);
    d->hw_pix_fmt = AV_PIX_FMT_NONE;
    d->ctx = avcodec_alloc_context3(codec);
    d->pkt = av_packet_alloc();
    d->frame = av_frame_alloc();
    d->sw = av_frame_alloc();
    if (!d->ctx || !d->pkt || !d->frame || !d->sw) {
        vd_decoder_close(d);
        return AVERROR(ENOMEM);
    }
    d->ctx->opaque = d;
    d->ctx->get_format = pick_format;
    /* Every packet is a whole picture and there is no reordering: output immediately. */
    d->ctx->flags |= AV_CODEC_FLAG_LOW_DELAY;
    /* Frame threading would add (threads - 1) frames of latency; slice threading does not. */
    d->ctx->thread_type = FF_THREAD_SLICE;
    d->ctx->thread_count = threads;

    if (hwaccel && strcmp(hwaccel, "none") != 0) try_hw(d, codec, hwaccel);

    int r = avcodec_open2(d->ctx, codec, NULL);
    if (r < 0) {
        vd_decoder_error_string(r, err, err_len);
        vd_decoder_close(d);
        return r;
    }
    snprintf(d->desc, sizeof d->desc, "%s", d->hw_name[0] ? d->hw_name : "software");
    *out = d;
    return 0;
}

int vd_decoder_decode(VdDecoder *d, const uint8_t *data, int size, VdFrame *out) {
    d->pkt->data = (uint8_t *)data;
    d->pkt->size = size;
    int r = avcodec_send_packet(d->ctx, d->pkt);
    d->pkt->data = NULL;
    d->pkt->size = 0;
    if (r < 0 && r != AVERROR(EAGAIN)) return r;

    r = avcodec_receive_frame(d->ctx, d->frame);
    if (r == AVERROR(EAGAIN) || r == AVERROR_EOF) return 0;
    if (r < 0) return r;

    AVFrame *f = d->frame;
    if (d->hw_pix_fmt != AV_PIX_FMT_NONE && f->format == d->hw_pix_fmt) {
        av_frame_unref(d->sw);
        r = av_hwframe_transfer_data(d->sw, f, 0);
        if (r < 0) return r;
        av_frame_copy_props(d->sw, f);
        f = d->sw;
        snprintf(d->desc, sizeof d->desc, "%s (%s)", d->hw_name, av_get_pix_fmt_name(f->format));
    } else {
        snprintf(d->desc, sizeof d->desc, "software (%s)", av_get_pix_fmt_name(f->format));
    }

    switch (f->format) {
    case AV_PIX_FMT_NV12: out->layout = VD_LAYOUT_NV12; break;
    case AV_PIX_FMT_YUV420P:
    case AV_PIX_FMT_YUVJ420P: out->layout = VD_LAYOUT_I420; break;
    case AV_PIX_FMT_YUV444P:
    case AV_PIX_FMT_YUVJ444P: out->layout = VD_LAYOUT_I444; break;
    default: return VD_ERR_UNSUPPORTED_FORMAT;
    }
    out->width = f->width;
    out->height = f->height;
    out->full_range = f->color_range == AVCOL_RANGE_JPEG || f->format == AV_PIX_FMT_YUVJ420P ||
                      f->format == AV_PIX_FMT_YUVJ444P;
    for (int i = 0; i < 3; i++) {
        out->data[i] = f->data[i];
        out->linesize[i] = f->linesize[i];
    }
    return 1;
}

const char *vd_decoder_describe(const VdDecoder *d) { return d->desc; }

void vd_decoder_error_string(int err, char *buf, size_t len) {
    if (err == VD_ERR_UNSUPPORTED_FORMAT) {
        snprintf(buf, len, "decoder produced an unsupported pixel format");
        return;
    }
    av_strerror(err, buf, len);
}

void vd_decoder_close(VdDecoder *d) {
    if (!d) return;
    avcodec_free_context(&d->ctx);
    av_packet_free(&d->pkt);
    av_frame_free(&d->frame);
    av_frame_free(&d->sw);
    av_buffer_unref(&d->hw_device);
    free(d);
}
