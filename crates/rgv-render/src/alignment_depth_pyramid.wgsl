// Prefixed with `shaders/common.wgsl` and `shaders/palette.wgsl`; see `shaders.rs`.
//
// Coverage-pyramid depth: one instance per bin, drawn as three stacked bands that never
// overlap, so the result is the same whatever the draw order:
//
//   band 0  baseline .. minimum   every base of the bin is at least this deep   (solid)
//   band 1  minimum  .. mean      up to the bin's mean depth                    (medium)
//   band 2  mean     .. maximum   the envelope up to the deepest base           (light)
//
// A dropout shows as a low solid band under a tall medium one, a narrow peak as a tall light
// band over a low medium one. Mirrored by `project_alignment_depth_pyramid_bin` in
// `alignment_summary/depth_pyramid.rs`.

struct PyramidUniform {
    screen_size: vec2<f32>,
    clip_origin: vec2<f32>,
    clip_size: vec2<f32>,
    points_per_base: vec2<f32>,
    // Whole-base and fractional parts of (page start - camera start); see `project_genomic_x`.
    page_start_whole: i32,
    page_start_fraction: f32,
    covered_clip_x: vec2<f32>,
    scale_maximum: f32,
    transform: u32,
    // Bases per bin, and bases from the page start to the end of its last bin: the last bin of
    // a contig is narrower than the others.
    bin_bases: u32,
    page_bases: u32,
};

@group(0) @binding(0)
var<uniform> frame: PyramidUniform;

// Twelve bytes (`AlignmentDepthPyramidGpuInstance`). The bin's position is its index in the
// page: `instance_index * bin_bases` bases from the page start.
struct BinInput {
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
    @location(0) minimum: u32,
    @location(1) mean: f32,
    @location(2) maximum: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) @interpolate(flat) visible: u32,
};

const BAND_ALPHA_MEAN: f32 = 0.72;
const BAND_ALPHA_ENVELOPE: f32 = 0.36;

fn project_x(page_offset: vec2<f32>) -> f32 {
    return frame.clip_origin.x + project_genomic_x(
        page_offset,
        frame.page_start_whole,
        frame.page_start_fraction,
        frame.points_per_base,
    );
}

fn depth_height(depth: f32) -> f32 {
    var fraction = min(depth / max(frame.scale_maximum, 1.0), 1.0);
    if frame.transform == 1u {
        fraction = sqrt(fraction);
    }
    return fraction * frame.clip_size.y;
}

@vertex
fn depth_pyramid_vertex(input: BinInput) -> VertexOutput {
    let band = input.vertex_index / 6u;
    let corner = quad_corner(input.vertex_index % 6u);

    let start = input.instance_index * frame.bin_bases;
    let end = min(start + frame.bin_bases, frame.page_bases);
    let left = max(project_x(page_offset_words(start)), frame.covered_clip_x.x);
    let right = min(project_x(page_offset_words(end)), frame.covered_clip_x.y);

    // A bin with any coverage at all is at least one point tall.
    var top_maximum = depth_height(f32(input.maximum));
    if input.maximum > 0u {
        top_maximum = max(top_maximum, 1.0);
    }
    let top_minimum = min(depth_height(f32(input.minimum)), top_maximum);
    let top_mean = clamp(depth_height(input.mean), top_minimum, top_maximum);

    var low = 0.0;
    var high = top_minimum;
    var alpha = 1.0;
    if band == 1u {
        low = top_minimum;
        high = top_mean;
        alpha = BAND_ALPHA_MEAN;
    } else if band == 2u {
        low = top_mean;
        high = top_maximum;
        alpha = BAND_ALPHA_ENVELOPE;
    }

    let baseline = frame.clip_origin.y + frame.clip_size.y;
    let origin = vec2<f32>(left, baseline - high);
    let size = vec2<f32>(right - left, high - low);
    var color = palette_slot(TOKEN_SUMMARY_DEPTH);
    color.a = color.a * alpha;

    var output: VertexOutput;
    output.position = points_to_clip(origin + corner * size, frame.screen_size);
    output.color = color;
    output.visible = select(0u, 1u, right > left && high > low);
    return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if input.visible == 0u {
        discard;
    }
    return input.color;
}
