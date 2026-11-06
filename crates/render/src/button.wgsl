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

fn rounded(p: vec2<f32>, half_size: vec2<f32>) -> f32 {
    let radius = 0.4;
    let d = abs(p) - half_size + vec2<f32>(radius);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0) - radius;
}

fn pill(p: vec2<f32>) -> f32 {
    return rounded(p, vec2<f32>(2.0, 1.0));
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

// A speech bubble: the other person is a message away.
@fragment
fn fs_chat(in: VsOut) -> @location(0) vec4<f32> {
    let p = in.p;
    let bubble = abs(rounded(p + vec2<f32>(0.0, 0.1), vec2<f32>(0.75, 0.45)));
    let tail = min(
        segment(p, vec2<f32>(-0.3, 0.35), vec2<f32>(-0.45, 0.7)),
        segment(p, vec2<f32>(-0.45, 0.7), vec2<f32>(0.05, 0.35)),
    );
    return paint(p, min(bubble, tail));
}

// How many displays there are, and the one shown (from 0).
@group(0) @binding(0) var<uniform> displays: vec2<u32>;

// Half a cell's width, in units of half its height (BAR_CELL / BUTTON_HEIGHT).
const CELL_HALF_WIDTH: f32 = 32.0 / 28.0;

// Distance to digit `n` drawn with seven segments.
fn digit(p: vec2<f32>, n: u32) -> f32 {
    let w = 0.28;
    let h = 0.5;
    // Bits: top, top right, bottom right, bottom, bottom left, top left, middle.
    var lit = array<u32, 10>(0x3f, 0x06, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 0x07, 0x7f, 0x6f);
    var ends = array<vec4<f32>, 7>(
        vec4<f32>(-w, -h, w, -h),
        vec4<f32>(w, -h, w, 0.0),
        vec4<f32>(w, 0.0, w, h),
        vec4<f32>(-w, h, w, h),
        vec4<f32>(-w, 0.0, -w, h),
        vec4<f32>(-w, -h, -w, 0.0),
        vec4<f32>(-w, 0.0, w, 0.0),
    );
    var d = 1000.0;
    for (var i = 0u; i < 7u; i++) {
        if ((lit[n] >> i) & 1u) == 1u {
            d = min(d, segment(p, ends[i].xy, ends[i].zw));
        }
    }
    return d;
}

// The numbered buttons, side by side: the one shown stands out.
@fragment
fn fs_bar(in: VsOut) -> @location(0) vec4<f32> {
    let count = f32(displays.x);
    let across = (in.p.x / 4.0 + 0.5) * count;
    let cell = clamp(floor(across), 0.0, count - 1.0);
    let p = vec2<f32>((across - cell - 0.5) * 2.0 * CELL_HALF_WIDTH, in.p.y);
    let edge = fwidth(in.p.y);
    let shape = rounded(p, vec2<f32>(CELL_HALF_WIDTH - 0.08, 1.0));
    let inside = 1.0 - smoothstep(-edge, edge, shape);
    let number = (u32(cell) + 1u) % 10u;
    let line = 1.0 - smoothstep(-edge, edge, digit(p, number) - 0.08);
    let shown = u32(cell) == displays.y;
    let back = select(0.12, 0.95, shown);
    let ink = select(1.0, 0.1, shown);
    let alpha = select(mix(0.8, 1.0, line), 1.0, shown);
    return vec4<f32>(vec3<f32>(mix(back, ink, line)), inside * alpha);
}
