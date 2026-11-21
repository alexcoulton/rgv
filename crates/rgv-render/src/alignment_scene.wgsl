// Prefixed with `shaders/common.wgsl`; see `shaders.rs`.

struct Camera {
    screen_size: vec2<f32>,
    clip_origin: vec2<f32>,
    points_per_base: vec2<f32>,
    // Whole-base and fractional parts of (page start - camera start); see `project_genomic_x`.
    page_start_whole: i32,
    page_start_fraction: f32,
    row_scroll: f32,
    row_height: f32,
    read_height: f32,
    page_row_start: u32,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) start_offset: vec2<f32>,
    @location(1) end_offset: vec2<f32>,
    @location(2) row_offset: u32,
    @location(3) shape: u32,
    @location(4) color: vec4<f32>,
};

struct InteractiveVertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) start_offset: vec2<f32>,
    @location(1) end_offset: vec2<f32>,
    @location(2) row_offset: u32,
    @location(3) shape: u32,
    @location(4) color: vec4<f32>,
    @location(5) interaction_flags: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local_point: vec2<f32>,
    @location(2) @interpolate(flat) size: vec2<f32>,
    @location(3) @interpolate(flat) shape: u32,
};

fn project_x(page_offset: vec2<f32>) -> f32 {
    return camera.clip_origin.x + project_genomic_x(
        page_offset,
        camera.page_start_whole,
        camera.page_start_fraction,
        camera.points_per_base,
    );
}

fn interaction_color(color: vec4<f32>, flags: u32) -> vec4<f32> {
    var target_color = color.rgb;
    var amount = 0.0;
    if (flags & 2u) != 0u {
        target_color = vec3<f32>(0.16, 0.42, 0.92);
        amount = 0.58;
    } else if (flags & 1u) != 0u {
        target_color = vec3<f32>(0.28, 0.52, 0.94);
        amount = 0.32;
    } else if (flags & 4u) != 0u {
        target_color = vec3<f32>(0.90, 0.62, 0.16);
        amount = 0.38;
    } else if (flags & 8u) != 0u {
        target_color = vec3<f32>(0.96, 0.70, 0.12);
        amount = 0.46;
    }
    return vec4<f32>(mix(color.rgb, target_color, amount), 1.0);
}

fn project_instance(
    vertex_index: u32,
    start_offset: vec2<f32>,
    end_offset: vec2<f32>,
    row_offset: u32,
    shape: u32,
    color: vec4<f32>,
) -> VertexOutput {
    let corner = quad_corner(vertex_index);
    var left = project_x(start_offset);
    var right = project_x(end_offset);
    let row = f32(camera.page_row_start + row_offset);
    let row_top = camera.clip_origin.y
        + (row - camera.row_scroll) * camera.row_height
        + (camera.row_height - camera.read_height) * 0.5;
    // Minimum-size policy (mirrored by `project_alignment_gpu_instance` and the CPU projector's
    // `project_primitive`): every shape keeps at least one point, which is at least one physical
    // pixel at any scale factor, in both axes. Without it sub-pixel mismatches shimmer as the
    // camera pans and whole rows vanish once rows are shorter than a pixel.
    var top = row_top;
    var height = max(camera.read_height, 1.0);

    if shape == 1u {
        top = row_top + camera.read_height * 0.45;
        height = max(camera.read_height * 0.10, 1.0);
    }
    if shape == 2u {
        let original_width = right - left;
        let marker_width = max(original_width, 1.5);
        left -= (marker_width - original_width) * 0.5;
        right = left + marker_width;
    } else {
        right = max(right, left + 1.0);
    }

    let point = vec2<f32>(
        select(left, right, corner.x > 0.5),
        top + corner.y * height,
    );
    var output: VertexOutput;
    output.position = points_to_clip(point, camera.screen_size);
    output.color = color;
    output.local_point = corner * vec2<f32>(right - left, height);
    output.size = vec2<f32>(right - left, height);
    output.shape = shape;
    return output;
}

@vertex
fn vertex_main(input: VertexInput) -> VertexOutput {
    return project_instance(
        input.vertex_index,
        input.start_offset,
        input.end_offset,
        input.row_offset,
        input.shape,
        input.color,
    );
}

@vertex
fn interaction_vertex_main(input: InteractiveVertexInput) -> VertexOutput {
    return project_instance(
        input.vertex_index,
        input.start_offset,
        input.end_offset,
        input.row_offset,
        input.shape,
        interaction_color(input.color, input.interaction_flags),
    );
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if input.shape == 3u || input.shape == 4u {
        let width = max(input.size.x, 0.0001);
        var distance_from_tip = input.size.x - input.local_point.x;
        if input.shape == 4u {
            distance_from_tip = input.local_point.x;
        }
        let head_width = min(input.size.y * 0.55, width * 0.35);
        if distance_from_tip < head_width {
            let half_height = input.size.y * 0.5;
            let allowed = half_height * clamp(distance_from_tip / head_width, 0.0, 1.0);
            if abs(input.local_point.y - half_height) > allowed {
                discard;
            }
        }
    }
    return input.color;
}
