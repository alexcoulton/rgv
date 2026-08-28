struct Screen {
    size: vec2<f32>,
    padding: vec2<f32>,
};

@group(0) @binding(0)
var<uniform> screen: Screen;

@group(0) @binding(1)
var committed_canvas: texture_2d<f32>;

@group(0) @binding(2)
var committed_sampler: sampler;

struct PreviewVertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) destination_origin: vec2<f32>,
    @location(1) destination_size: vec2<f32>,
    @location(2) source_origin: vec2<f32>,
    @location(3) source_size: vec2<f32>,
    @location(4) sample_transform: vec4<f32>,
};

struct PreviewVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) local_point: vec2<f32>,
    @location(1) @interpolate(flat) source_origin: vec2<f32>,
    @location(2) @interpolate(flat) source_size: vec2<f32>,
    @location(3) @interpolate(flat) sample_transform: vec4<f32>,
};

@vertex
fn preview_vertex(input: PreviewVertexInput) -> PreviewVertexOutput {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let local_point = corners[input.vertex_index];
    let point = input.destination_origin + local_point * input.destination_size;
    let normalized = point / screen.size;
    var output: PreviewVertexOutput;
    output.position = vec4<f32>(
        normalized.x * 2.0 - 1.0,
        1.0 - normalized.y * 2.0,
        0.0,
        1.0,
    );
    output.local_point = local_point;
    output.source_origin = input.source_origin;
    output.source_size = input.source_size;
    output.sample_transform = input.sample_transform;
    return output;
}

@fragment
fn preview_fragment(input: PreviewVertexOutput) -> @location(0) vec4<f32> {
    let neutral = vec4<f32>(0.9686, 0.9686, 0.9686, 1.0);
    if input.sample_transform.z < 0.5 {
        return neutral;
    }
    let source_x = input.local_point.x * input.sample_transform.x + input.sample_transform.y;
    if source_x < 0.0 || source_x > 1.0 {
        return neutral;
    }
    let source_point = input.source_origin
        + vec2<f32>(source_x, input.local_point.y) * input.source_size;
    return textureSample(committed_canvas, committed_sampler, source_point);
}

@vertex
fn background_vertex(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[vertex_index], 0.0, 1.0);
}

@fragment
fn background_fragment() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}
