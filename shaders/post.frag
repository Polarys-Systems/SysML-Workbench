#version 460

// Full-target passes over one texture: copy it, shrink it 4x, or blur it along
// one axis. Textures are sRGB, so sampling and blending happen in linear space.

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 out_color;

layout(set = 0, binding = 0) uniform sampler2D src;

layout(push_constant) uniform Push {
    vec2 uv_scale;  // target uv -> source uv
    vec2 step;      // blur: source uv offset per tap unit (direction * spread)
    float mode;     // 0 copy, 1 shrink 4x, 2 blur
} pc;

void main() {
    vec2 uv = v_uv * pc.uv_scale;
    int mode = int(pc.mode + 0.5);
    if (mode == 1) {
        // Four bilinear taps average the 4x4 source block under this pixel.
        vec2 t = 1.0 / vec2(textureSize(src, 0));
        vec4 c = texture(src, uv + t * vec2(-1.0, -1.0))
               + texture(src, uv + t * vec2(1.0, -1.0))
               + texture(src, uv + t * vec2(-1.0, 1.0))
               + texture(src, uv + t * vec2(1.0, 1.0));
        out_color = c * 0.25;
    } else if (mode == 2) {
        // 13-tap Gaussian folded into 7 bilinear taps.
        const float offsets[4] = float[](0.0, 1.411764705882353, 3.2941176470588234, 5.176470588235294);
        const float weights[4] = float[](0.1964825501511404, 0.2969069646728344, 0.09447039785044732, 0.010381362401148057);
        vec4 c = texture(src, uv) * weights[0];
        for (int i = 1; i < 4; i++) {
            c += texture(src, uv + pc.step * offsets[i]) * weights[i];
            c += texture(src, uv - pc.step * offsets[i]) * weights[i];
        }
        out_color = c;
    } else {
        out_color = texture(src, uv);
    }
}
