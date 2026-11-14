#version 460
#extension GL_EXT_buffer_reference : require

// Anti-aliased 2D shapes (rounded rectangles and line segments). Instance data is
// read through a buffer device address: no vertex buffers, no descriptor sets.
// Each instance is a 4-vertex triangle strip covering the shape plus 1px of AA.

struct Shape {
    vec4 geom;   // rect: x0, y0, x1, y1 | line: ax, ay, bx, by (layer space)
    vec4 fill;   // sRGB RGBA; the line color for lines
    vec4 stroke; // sRGB RGBA border color (rects)
    vec4 params; // x: kind (0 rect, 1 line, 2 glass rect), y: corner radius | thickness,
                 // z: border width | dash length (0 = solid), w: rect blur (soft shadow)
};

layout(buffer_reference, std430, buffer_reference_align = 16) readonly buffer Shapes {
    Shape items[];
};

layout(push_constant) uniform Push {
    Shapes shapes;      // device address of this frame's shape buffer
    vec2 pixel_to_ndc;  // 2 / target size
    vec2 offset;        // screen = layer * scale + offset (pixels)
    float scale;
    float _pad;
} pc;

layout(location = 0) out vec2 v_local;
layout(location = 1) flat out vec2 v_size;
layout(location = 2) flat out vec4 v_fill;
layout(location = 3) flat out vec4 v_stroke;
layout(location = 4) flat out vec4 v_params;

const float AA = 1.0;

vec3 srgb_to_linear(vec3 c) {
    return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), greaterThan(c, vec3(0.04045)));
}

void main() {
    Shape s = pc.shapes.items[gl_InstanceIndex];
    vec2 corner = vec2(gl_VertexIndex & 1, gl_VertexIndex >> 1);
    vec2 screen;
    if (int(s.params.x) == 1) {
        vec2 a = s.geom.xy * pc.scale + pc.offset;
        vec2 b = s.geom.zw * pc.scale + pc.offset;
        float len = length(b - a);
        vec2 dir = len > 0.0 ? (b - a) / len : vec2(1.0, 0.0);
        vec2 nrm = vec2(-dir.y, dir.x);
        float half_t = max(s.params.y * pc.scale, 1.0) * 0.5;
        float ext = half_t + AA;
        float along = mix(-ext, len + ext, corner.x);
        float across = mix(-ext, ext, corner.y);
        screen = a + dir * along + nrm * across;
        v_local = vec2(along, across);
        v_size = vec2(len, half_t);
        v_params = vec4(1.0, half_t, s.params.z * pc.scale, 0.0);
    } else {
        vec2 p0 = s.geom.xy * pc.scale + pc.offset;
        vec2 p1 = s.geom.zw * pc.scale + pc.offset;
        vec2 hs = abs(p1 - p0) * 0.5;
        float blur = max(s.params.w * pc.scale, 0.0);
        v_local = mix(-(hs + AA + blur), hs + AA + blur, corner);
        screen = (p0 + p1) * 0.5 + v_local;
        v_size = hs;
        float radius = min(s.params.y * pc.scale, min(hs.x, hs.y));
        float border = s.params.z > 0.0 ? max(s.params.z * pc.scale, 1.0) : 0.0;
        v_params = vec4(s.params.x, radius, border, blur);
    }
    gl_Position = vec4(screen * pc.pixel_to_ndc - 1.0, 0.0, 1.0);
    // The swapchain is sRGB, so blending happens in linear space.
    v_fill = vec4(srgb_to_linear(s.fill.rgb), s.fill.a);
    v_stroke = vec4(srgb_to_linear(s.stroke.rgb), s.stroke.a);
}
