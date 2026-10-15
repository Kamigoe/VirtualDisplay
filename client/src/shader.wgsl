// Draws the decoded YCbCr picture (BT.709) as an aspect-fitted quad.

struct Params {
    scale: vec2<f32>,
    // 0 = NV12 (tex_u holds CbCr in .rg), 1 = planar (tex_u = Cb, tex_v = Cr)
    plane_layout: u32,
    full_range: u32,
    // 1 when the swapchain format is *Srgb and expects linear output
    srgb_out: u32,
    // sampled unorm value * code_scale = integer code (255, 65535, or 65535/64 for P010)
    code_scale: f32,
    // 8 or 10
    bit_depth: u32,
    // 1: add +-0.5 LSB noise so 10-bit gradients don't band on an 8-bit swapchain
    dither: u32,
};

@group(0) @binding(0) var tex_y: texture_2d<f32>;
@group(0) @binding(1) var tex_u: texture_2d<f32>;
@group(0) @binding(2) var tex_v: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> params: Params;
// Nearest: the Mac's 4:2:0 conversion box-averages chroma, which replication reconstructs best
// (measured ~49 dB vs ~32 dB for bilinear on testsrc2 through VTPixelTransferSession + VideoToolbox).
@group(0) @binding(5) var samp_chroma: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    );
    let c = corners[i];
    var o: VsOut;
    o.pos = vec4(c * params.scale, 0.0, 1.0);
    o.uv = vec2((c.x + 1.0) * 0.5, (1.0 - c.y) * 0.5);
    return o;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3(2.4)), c / 12.92, c <= vec3(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let y = textureSample(tex_y, samp, in.uv).r;
    var cb: f32;
    var cr: f32;
    if params.plane_layout == 0u {
        let c = textureSample(tex_u, samp_chroma, in.uv).rg;
        cb = c.r;
        cr = c.g;
    } else {
        cb = textureSample(tex_u, samp_chroma, in.uv).r;
        cr = textureSample(tex_v, samp_chroma, in.uv).r;
    }

    // Work in integer code values so 8- and 10-bit share one set of range constants.
    let yc = y * params.code_scale;
    let cbc = cb * params.code_scale;
    let crc = cr * params.code_scale;
    let k = f32(1u << (params.bit_depth - 8u)); // 1 for 8-bit, 4 for 10-bit
    let max_code = 255.0 * k + (k - 1.0);       // 255 or 1023
    var luma: f32;
    var u: f32;
    var v: f32;
    if params.full_range == 1u {
        luma = yc / max_code;
        u = (cbc - 128.0 * k) / max_code;
        v = (crc - 128.0 * k) / max_code;
    } else {
        luma = (yc - 16.0 * k) / (219.0 * k);
        u = (cbc - 128.0 * k) / (224.0 * k);
        v = (crc - 128.0 * k) / (224.0 * k);
    }

    var rgb = vec3(luma + 1.5748 * v, luma - 0.1873 * u - 0.4681 * v, luma + 1.8556 * u);
    if params.dither == 1u {
        let n = fract(sin(dot(in.pos.xy, vec2(12.9898, 78.233))) * 43758.5453);
        rgb += vec3((n - 0.5) / 255.0);
    }
    rgb = clamp(rgb, vec3(0.0), vec3(1.0));
    if params.srgb_out == 1u {
        rgb = srgb_to_linear(rgb);
    }
    return vec4(rgb, 1.0);
}
