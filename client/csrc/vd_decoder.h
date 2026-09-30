/* Thin C shim over libavcodec's HEVC decoder so the Rust side needs no FFmpeg bindings. */
#ifndef VD_DECODER_H
#define VD_DECODER_H

#include <stddef.h>
#include <stdint.h>

enum {
    VD_LAYOUT_NV12 = 0, /* Y plane + interleaved CbCr at half resolution */
    VD_LAYOUT_I420 = 1, /* Y, Cb, Cr planes; chroma at half resolution */
    VD_LAYOUT_I444 = 2, /* Y, Cb, Cr planes; full resolution */
};

#define VD_ERR_UNSUPPORTED_FORMAT (-100000)

typedef struct VdFrame {
    int width;
    int height;
    int layout;
    int full_range;
    const uint8_t *data[3];
    int linesize[3];
} VdFrame;

typedef struct VdDecoder VdDecoder;

/* hwaccel: "auto", "none", or an FFmpeg device type name ("d3d11va", "vaapi", "vulkan", "videotoolbox", ...).
 * threads: software decoding slice threads (0 = auto). Returns 0 or a negative AVERROR. */
int vd_decoder_open(VdDecoder **out, const char *hwaccel, int threads, char *err, size_t err_len);

/* Decodes one complete Annex-B access unit. Returns 1 and fills *out if a picture is ready,
 * 0 if not, or a negative error. *out stays valid until the next call. */
int vd_decoder_decode(VdDecoder *d, const uint8_t *data, int size, VdFrame *out);

/* Human-readable decode path of the last picture, e.g. "d3d11va" or "software (yuv444p)". */
const char *vd_decoder_describe(const VdDecoder *d);

void vd_decoder_error_string(int err, char *buf, size_t len);

void vd_decoder_close(VdDecoder *d);

#endif
