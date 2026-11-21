// Prefixed with `shaders/common.wgsl` and `shaders/rectangle_shapes.wgsl`; see `shaders.rs`.

struct Screen {
    size: vec2<f32>,
    selected_object_id: u32,
    hovered_object_id: u32,
};

@group(0) @binding(0)
var<uniform> screen: Screen;

@vertex
fn vertex_main(input: VertexInput) -> VertexOutput {
    let corner = quad_corner(input.vertex_index);
    let point = input.origin + corner * input.size;

    var output: VertexOutput;
    output.position = points_to_clip(point, screen.size);
    output.color = input.color;
    output.object_id = input.object_id;
    output.local_point = corner * input.size;
    output.size = input.size;
    output.shape = input.padding.x;
    return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return rectangle_fragment_color(
        input,
        screen.selected_object_id,
        screen.hovered_object_id,
    );
}
