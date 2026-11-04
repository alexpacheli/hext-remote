// The full screen button, drawn over the video: a dark pill with four corner
// brackets. Twice as wide as tall; distances are in units of its height / 2.

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) p: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.p = (uv - vec2<f32>(0.5)) * vec2<f32>(4.0, 2.0);
    return out;
}

fn segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let along = clamp(dot(p - a, b - a) / dot(b - a, b - a), 0.0, 1.0);
    return length(p - a - (b - a) * along);
}

fn pill(p: vec2<f32>) -> f32 {
    let radius = 0.4;
    let d = abs(p) - vec2<f32>(2.0, 1.0) + vec2<f32>(radius);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0) - radius;
}

// One corner of the icon, mirrored to the four: its arms run `arm` from
// `corner` (negative: toward the center).
fn bracket(p: vec2<f32>, corner: vec2<f32>, arm: f32) -> f32 {
    let q = abs(p);
    return min(
        segment(q, corner, corner + vec2<f32>(arm, 0.0)),
        segment(q, corner, corner + vec2<f32>(0.0, arm)),
    );
}

fn paint(p: vec2<f32>, icon: f32) -> vec4<f32> {
    let edge = fwidth(p.y);
    let inside = 1.0 - smoothstep(-edge, edge, pill(p));
    let line = 1.0 - smoothstep(-edge, edge, icon - 0.07);
    let color = mix(vec3<f32>(0.12), vec3<f32>(1.0), line);
    return vec4<f32>(color, inside * mix(0.8, 1.0, line));
}

// Brackets pointing out of the center: make it bigger.
@fragment
fn fs_enter(in: VsOut) -> @location(0) vec4<f32> {
    return paint(in.p, bracket(in.p, vec2<f32>(0.75, 0.55), -0.3));
}

// Brackets pointing into the center: back to the window.
@fragment
fn fs_leave(in: VsOut) -> @location(0) vec4<f32> {
    return paint(in.p, bracket(in.p, vec2<f32>(0.45, 0.25), 0.3));
}
