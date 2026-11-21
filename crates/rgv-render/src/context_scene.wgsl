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
    @location(2) local_point: vec2<f32>,
    @location(3) @interpolate(flat) size: vec2<f32>,
    @location(4) @interpolate(flat) shape: u32,
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
    let point = projected_origin + corners[input.vertex_index] * projected_size;
    let normalized = point / camera.screen_size;

    var output: VertexOutput;
    output.position = vec4<f32>(
        normalized.x * 2.0 - 1.0,
        1.0 - normalized.y * 2.0,
        0.0,
        1.0,
    );
    output.color = input.color;
    output.object_id = input.object_id;
    output.local_point = corners[input.vertex_index] * projected_size;
    output.size = projected_size;
    output.shape = input.padding.x;
    return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    var output_color = input.color;
    if input.shape >= 7u && input.shape <= 12u {
        let base_stroke = clamp(1.0 + input.size.y * 0.075, 1.25, 4.5);
        var expansion = 0.0;
        if input.shape == 9u || input.shape == 10u {
            expansion = 1.5;
        } else if input.shape == 11u || input.shape == 12u {
            expansion = 2.0;
        }
        let stroke = base_stroke + expansion * 2.0;
        let inner_height = max(input.size.y - base_stroke, 0.0);
        let fraction = clamp(input.local_point.x / max(input.size.x, 0.0001), 0.0, 1.0);
        let parabola = 4.0 * (fraction - 0.5) * (fraction - 0.5);
        var curve_y = base_stroke * 0.5 + inner_height * parabola;
        if input.shape == 8u || input.shape == 10u || input.shape == 12u {
            curve_y = input.size.y - base_stroke * 0.5 - inner_height * parabola;
        }
        let slope = 8.0 * inner_height * (fraction - 0.5) / max(input.size.x, 1.0);
        let distance = abs(input.local_point.y - curve_y) / sqrt(1.0 + slope * slope);
        let coverage = 1.0 - smoothstep(max(stroke * 0.5 - 0.75, 0.0), stroke * 0.5 + 0.75, distance);
        if coverage <= 0.0 {
            discard;
        }
        output_color.a *= coverage;
    } else if input.shape == 5u || input.shape == 6u {
        let bar_width = min(input.size.x, 2.0);
        let stem_half_height = min(
            max(input.size.y * 0.14, min(input.size.y, 1.0)),
            input.size.y * 0.5,
        );
        var in_bar = input.local_point.x <= bar_width;
        if input.shape == 6u {
            in_bar = input.local_point.x >= input.size.x - bar_width;
        }
        let in_stem = abs(input.local_point.y - input.size.y * 0.5) <= stem_half_height;
        if !in_bar && !in_stem {
            discard;
        }
    } else if input.shape != 0u {
        let is_terminal = input.shape == 1u || input.shape == 2u;
        var head_width = input.size.x;
        if is_terminal {
            head_width = min(input.size.y * 0.55, input.size.x * 0.35);
        }
        var distance_from_tip = input.local_point.x;
        if input.shape == 2u || input.shape == 4u || input.shape == 6u {
            distance_from_tip = input.size.x - input.local_point.x;
        }
        if distance_from_tip < head_width {
            let half_height = input.size.y * 0.5;
            let allowed_half_height = half_height * clamp(distance_from_tip / head_width, 0.0, 1.0);
            if abs(input.local_point.y - half_height) > allowed_half_height {
                discard;
            }
        }
    }
    if input.object_id != 0u && input.object_id == camera.selected_object_id {
        return vec4<f32>(mix(output_color.rgb, vec3<f32>(0.16, 0.42, 0.92), 0.58), 1.0);
    }
    if input.object_id != 0u && input.object_id == camera.hovered_object_id {
        return vec4<f32>(mix(output_color.rgb, vec3<f32>(0.28, 0.52, 0.94), 0.26), 1.0);
    }
    return output_color;
}
