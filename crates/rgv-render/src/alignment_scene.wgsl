struct Camera {
    screen_size: vec2<f32>,
    clip_origin: vec2<f32>,
    points_per_base: vec2<f32>,
    page_start_from_camera: vec2<f32>,
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

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local_point: vec2<f32>,
    @location(2) @interpolate(flat) size: vec2<f32>,
    @location(3) @interpolate(flat) shape: u32,
};

fn two_sum(a: f32, b: f32) -> vec2<f32> {
    let sum = a + b;
    let recovered_b = sum - a;
    return vec2<f32>(sum, (a - (sum - recovered_b)) + (b - recovered_b));
}

fn quick_two_sum(a: f32, b: f32) -> vec2<f32> {
    let sum = a + b;
    return vec2<f32>(sum, b - (sum - a));
}

fn double_single_add(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    let leading = two_sum(a.x, b.x);
    return quick_two_sum(leading.x, leading.y + a.y + b.y);
}

fn double_single_multiply(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    let product = a.x * b.x;
    var error = fma(a.x, b.x, -product);
    error += a.x * b.y;
    error += a.y * b.x;
    error += a.y * b.y;
    return quick_two_sum(product, error);
}

fn project_x(page_offset: vec2<f32>) -> f32 {
    let delta = double_single_add(camera.page_start_from_camera, page_offset);
    let projected = double_single_multiply(delta, camera.points_per_base);
    return camera.clip_origin.x + projected.x + projected.y;
}

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
    let corner = corners[input.vertex_index];
    var left = project_x(input.start_offset);
    var right = project_x(input.end_offset);
    let row = f32(camera.page_row_start + input.row_offset);
    let row_top = camera.clip_origin.y
        + (row - camera.row_scroll) * camera.row_height
        + (camera.row_height - camera.read_height) * 0.5;
    var top = row_top;
    var height = camera.read_height;

    if input.shape == 1u {
        top = row_top + camera.read_height * 0.45;
        height = max(camera.read_height * 0.10, 1.0);
    } else if input.shape == 2u {
        let original_width = right - left;
        let marker_width = max(original_width, 1.5);
        left -= (marker_width - original_width) * 0.5;
        right = left + marker_width;
    } else if input.shape == 3u || input.shape == 4u {
        right = max(right, left + 1.0);
    }

    let point = vec2<f32>(
        select(left, right, corner.x > 0.5),
        top + corner.y * height,
    );
    let normalized = point / camera.screen_size;
    var output: VertexOutput;
    output.position = vec4<f32>(
        normalized.x * 2.0 - 1.0,
        1.0 - normalized.y * 2.0,
        0.0,
        1.0,
    );
    output.color = input.color;
    output.local_point = corner * vec2<f32>(right - left, height);
    output.size = vec2<f32>(right - left, height);
    output.shape = input.shape;
    return output;
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
