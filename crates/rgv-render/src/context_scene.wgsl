// Prefixed with `shaders/common.wgsl` and `shaders/rectangle_shapes.wgsl`; see `shaders.rs`.

struct Camera {
    screen_size: vec2<f32>,
    x_scale: f32,
    x_offset: f32,
    y_offset: f32,
    selected_object_id: u32,
    hovered_object_id: u32,
    // Source-space x subtracted before scaling; see `context_camera_uniform`.
    x_origin: f32,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

@vertex
fn vertex_main(input: VertexInput) -> VertexOutput {
    let corner = quad_corner(input.vertex_index);
    // A feature with any extent keeps at least one point of width when the camera zooms out
    // over stale layout; mirrored by `project_context_scene_rectangle`.
    var projected_width = input.size.x * camera.x_scale;
    if input.size.x > 0.0 {
        projected_width = max(projected_width, 1.0);
    }
    let projected_size = vec2<f32>(projected_width, input.size.y);
    // Origin-relative: the difference of two nearby source coordinates is exact in f32, so the
    // large `origin * scale` and `offset` terms cancel on the CPU in f64 instead of here.
    let projected_origin = vec2<f32>(
        (input.origin.x - camera.x_origin) * camera.x_scale + camera.x_offset,
        input.origin.y + camera.y_offset,
    );
    let point = projected_origin + corner * projected_size;

    var output: VertexOutput;
    output.position = points_to_clip(point, camera.screen_size);
    output.color = input.color;
    output.object_id = input.object_id;
    output.local_point = corner * projected_size;
    output.size = projected_size;
    output.shape = input.padding.x;
    return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return rectangle_fragment_color(
        input,
        camera.selected_object_id,
        camera.hovered_object_id,
    );
}
