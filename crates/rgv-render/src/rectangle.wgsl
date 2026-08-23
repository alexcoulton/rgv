struct Screen {
    size: vec2<f32>,
    selected_object_id: u32,
    hovered_object_id: u32,
};

@group(0) @binding(0)
var<uniform> screen: Screen;

struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) origin: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) object_id: u32,
    @location(4) padding: vec3<u32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) @interpolate(flat) object_id: u32,
};

@vertex
fn vertex_main(input: VertexInput) -> VertexOutput {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let point = input.origin + corners[input.vertex_index] * input.size;
    let normalized = point / screen.size;

    var output: VertexOutput;
    output.position = vec4<f32>(
        normalized.x * 2.0 - 1.0,
        1.0 - normalized.y * 2.0,
        0.0,
        1.0,
    );
    output.color = input.color;
    output.object_id = input.object_id;
    return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if input.object_id != 0u && input.object_id == screen.selected_object_id {
        return vec4<f32>(mix(input.color.rgb, vec3<f32>(0.16, 0.42, 0.92), 0.58), 1.0);
    }
    if input.object_id != 0u && input.object_id == screen.hovered_object_id {
        return vec4<f32>(mix(input.color.rgb, vec3<f32>(0.28, 0.52, 0.94), 0.26), 1.0);
    }
    return input.color;
}
