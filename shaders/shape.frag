#version 460

layout(location = 0) in vec2 v_local;
layout(location = 1) flat in vec2 v_size;
layout(location = 2) flat in vec4 v_fill;
layout(location = 3) flat in vec4 v_stroke;
layout(location = 4) flat in vec4 v_params;
layout(location = 0) out vec4 out_color;

vec4 premultiply(vec4 c) {
    return vec4(c.rgb * c.a, c.a);
}

void main() {
    vec4 color;
    if (int(v_params.x + 0.5) == 1) {
        // Line: distance to the segment from (0, 0) to (len, 0), round caps.
        float along = v_local.x;
        float beyond = max(max(-along, along - v_size.x), 0.0);
        float d = length(vec2(beyond, v_local.y)) - v_size.y;
        float coverage = clamp(0.5 - d, 0.0, 1.0);
        float dash = v_params.z;
        if (dash > 0.0) {
            float m = mod(along, dash * 2.0);
            coverage *= clamp(min(m, dash - m) + 0.5, 0.0, 1.0);
        }
        color = premultiply(v_fill) * coverage;
    } else {
        // Rounded rectangle: signed distance, fill inside the border.
        float radius = v_params.y;
        float border = v_params.z;
        vec2 q = abs(v_local) - v_size + radius;
        float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
        float outer = clamp(0.5 - d, 0.0, 1.0);
        vec4 c = premultiply(v_fill);
        if (border > 0.0) {
            float inner = clamp(0.5 - (d + border), 0.0, 1.0);
            c = mix(premultiply(v_stroke), c, inner);
        }
        color = c * outer;
    }
    if (color.a <= 0.0) {
        discard;
    }
    out_color = color; // premultiplied
}
