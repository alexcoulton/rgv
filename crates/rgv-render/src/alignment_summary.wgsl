struct SummaryUniform {
    screen_size: vec2<f32>,
    clip_origin: vec2<f32>,
    clip_size: vec2<f32>,
    points_per_base: vec2<f32>,
    page_start_from_camera: vec2<f32>,
    covered_clip_x: vec2<f32>,
    scale_maximum: f32,
    minimum_alternate_fraction: f32,
    transform: u32,
    quality_weighted: u32,
    family: u32,
    hidden_modification_mask: u32,
    modification_color_mode: u32,
    padding: u32,
};

@group(0) @binding(0)
var<uniform> frame: SummaryUniform;

struct DepthInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) start_offset: vec2<f32>,
    @location(1) end_offset: vec2<f32>,
    @location(2) depth: u32,
};

struct AlleleInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) start_offset: vec2<f32>,
    @location(1) end_offset: vec2<f32>,
    @location(2) local_depth: u32,
    @location(3) allele: u32,
    @location(4) raw: vec3<f32>,
    @location(5) quality_weighted: vec3<f32>,
};

struct ModificationInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) start_offset: vec2<f32>,
    @location(1) end_offset: vec2<f32>,
    @location(2) fractions: vec2<f32>,
    @location(3) by_type_color: vec4<f32>,
    @location(4) identity: vec4<u32>,
    @location(5) shape: u32,
};

struct OverlayInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) origin: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) @interpolate(flat) visible: u32,
};

fn corner(vertex_index: u32) -> vec2<f32> {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    return corners[vertex_index];
}

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
    let delta = double_single_add(frame.page_start_from_camera, page_offset);
    let projected = double_single_multiply(delta, frame.points_per_base);
    return frame.clip_origin.x + projected.x + projected.y;
}

fn transformed_depth_fraction(depth: u32) -> f32 {
    var fraction = min(f32(depth) / max(frame.scale_maximum, 1.0), 1.0);
    if frame.transform == 1u {
        fraction = sqrt(fraction);
    }
    return fraction;
}

fn rectangle_output(
    vertex_index: u32,
    origin: vec2<f32>,
    size: vec2<f32>,
    color: vec4<f32>,
    visible: u32,
) -> VertexOutput {
    let point = origin + corner(vertex_index) * size;
    let normalized = point / frame.screen_size;
    var output: VertexOutput;
    output.position = vec4<f32>(
        normalized.x * 2.0 - 1.0,
        1.0 - normalized.y * 2.0,
        0.0,
        1.0,
    );
    output.color = color;
    output.visible = visible;
    return output;
}

@vertex
fn depth_vertex(input: DepthInput) -> VertexOutput {
    let left = max(project_x(input.start_offset), frame.covered_clip_x.x);
    let semantic_right = min(project_x(input.end_offset), frame.covered_clip_x.y);
    let visible = select(0u, 1u, semantic_right > left);
    let right = max(semantic_right, left + 1.0);
    let height = max(transformed_depth_fraction(input.depth) * frame.clip_size.y, 1.0);
    let top = frame.clip_origin.y + frame.clip_size.y - height;
    return rectangle_output(
        input.vertex_index,
        vec2<f32>(left, top),
        vec2<f32>(right - left, height),
        vec4<f32>(0.48, 0.48, 0.48, 0.90),
        visible,
    );
}

fn allele_color(allele: u32) -> vec4<f32> {
    if allele == 0u {
        return vec4<f32>(0.24, 0.78, 0.38, 1.0);
    }
    if allele == 1u {
        return vec4<f32>(0.22, 0.52, 0.96, 1.0);
    }
    if allele == 2u {
        return vec4<f32>(0.96, 0.65, 0.18, 1.0);
    }
    if allele == 3u {
        return vec4<f32>(0.94, 0.28, 0.28, 1.0);
    }
    return vec4<f32>(0.60, 0.64, 0.70, 1.0);
}

@vertex
fn allele_vertex(input: AlleleInput) -> VertexOutput {
    var values = input.raw;
    if frame.quality_weighted != 0u {
        values = input.quality_weighted;
    }
    let semantically_visible = select(
        0u,
        1u,
        input.local_depth > 0u
            && values.x > 0.0
            && values.y >= frame.minimum_alternate_fraction,
    );
    let left = max(project_x(input.start_offset), frame.covered_clip_x.x);
    let semantic_right = min(project_x(input.end_offset), frame.covered_clip_x.y);
    let horizontally_visible = select(0u, 1u, semantic_right > left);
    let visible = semantically_visible * horizontally_visible;
    let right = max(semantic_right, left + 1.0);
    let coverage_height = max(
        transformed_depth_fraction(input.local_depth) * frame.clip_size.y,
        1.0,
    );
    let height = min(max(values.x * coverage_height, 1.0), coverage_height);
    let top = max(
        frame.clip_origin.y + frame.clip_size.y
            - (values.z + values.x) * coverage_height,
        frame.clip_origin.y,
    );
    return rectangle_output(
        input.vertex_index,
        vec2<f32>(left, top),
        vec2<f32>(right - left, height),
        allele_color(input.allele),
        visible,
    );
}

@vertex
fn modification_vertex(input: ModificationInput) -> VertexOutput {
    let unhidden_mask = ~frame.hidden_modification_mask;
    var visible = 0u;
    var fraction = input.fractions.x;
    if frame.quality_weighted != 0u {
        fraction = input.fractions.y;
    }
    var color = input.by_type_color;
    var height = max(clamp(fraction, 0.0, 1.0) * frame.clip_size.y, 1.0);
    if input.shape == 1u {
        visible = select(
            0u,
            1u,
            frame.modification_color_mode == 2u && (input.identity.y & unhidden_mask) != 0u,
        );
        height = frame.clip_size.y;
    } else {
        let code_bit = 1u << input.identity.x;
        visible = select(
            0u,
            1u,
            input.identity.z > 0u
                && (code_bit & frame.hidden_modification_mask) == 0u
                && (input.identity.y & unhidden_mask) == 0u,
        );
        if frame.modification_color_mode != 0u {
            color = vec4<f32>(0.90, 0.24, 0.92, 0.92);
        }
    }
    let left = max(project_x(input.start_offset), frame.covered_clip_x.x);
    let semantic_right = min(project_x(input.end_offset), frame.covered_clip_x.y);
    visible *= select(0u, 1u, semantic_right > left);
    let right = max(semantic_right, left + 1.0);
    let top = frame.clip_origin.y + frame.clip_size.y - height;
    return rectangle_output(
        input.vertex_index,
        vec2<f32>(left, top),
        vec2<f32>(right - left, height),
        color,
        visible,
    );
}

@vertex
fn overlay_vertex(input: OverlayInput) -> VertexOutput {
    return rectangle_output(
        input.vertex_index,
        input.origin,
        input.size,
        input.color,
        1u,
    );
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if input.visible == 0u {
        discard;
    }
    return input.color;
}
