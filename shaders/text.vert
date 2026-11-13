#version 450

// One instance per glyph quad, drawn as a 4-vertex triangle strip.
layout(location = 0) in vec4 rect;  // x0, y0, x1, y1 in pixels (y down)
layout(location = 1) in vec4 uv;    // u0, v0, u1, v1 normalized atlas coordinates
layout(location = 2) in vec4 color; // sRGB-encoded RGBA

layout(push_constant) uniform Push {
    vec2 pixel_to_ndc; // 2 / target size
} pc;

layout(location = 0) out vec2 v_uv;
layout(location = 1) out vec4 v_color;

vec3 srgb_to_linear(vec3 c) {
    return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), greaterThan(c, vec3(0.04045)));
}

void main() {
    vec2 corner = vec2(gl_VertexIndex & 1, gl_VertexIndex >> 1);
    vec2 position = mix(rect.xy, rect.zw, corner);
    gl_Position = vec4(position * pc.pixel_to_ndc - 1.0, 0.0, 1.0);
    v_uv = mix(uv.xy, uv.zw, corner);
    // The swapchain is sRGB, so blending happens in linear space.
    v_color = vec4(srgb_to_linear(color.rgb), color.a);
}
