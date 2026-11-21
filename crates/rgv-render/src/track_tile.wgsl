// Prefixed with `shaders/common.wgsl`; see `shaders.rs`.
//
// World-space track tiles. An instance is a whole number of bases relative to its tile start;
// the lane uniform carries the camera as the tile start minus the camera start (whole bases
// plus a fraction), and `project_genomic_x` cancels the two in integers. Mirrored on the CPU by
// `project_track_instance` and `track_fragment_coverage` in `track_tile_renderer.rs`.

struct TrackPalette {
    colors: array<vec4<f32>, 64>,
};

struct Lane {
    screen_size: vec2<f32>,
    lane_origin: vec2<f32>,
    lane_size: vec2<f32>,
    row_pitch: f32,
    row_height: f32,
    points_per_base: vec2<f32>,
    origin_fraction: f32,
    row_offset: f32,
    value_range: vec2<f32>,
    origin_whole: i32,
    selected: u32,
    hovered: u32,
    bin_style: u32,
    padding: vec2<u32>,
};

@group(0) @binding(0)
var<uniform> lane: Lane;

@group(1) @binding(0)
var<uniform> track_palette: TrackPalette;

const SHAPE_ARROW_RIGHT: u32 = 1u;
const SHAPE_ARROW_LEFT: u32 = 2u;
const SHAPE_LINE: u32 = 3u;
const SHAPE_LINE_CHEVRON_RIGHT: u32 = 4u;
const SHAPE_LINE_CHEVRON_LEFT: u32 = 5u;
const SHAPE_ARC: u32 = 6u;
const SHAPE_DIAMOND: u32 = 7u;

const VERTICAL_VALUE: u32 = 1u;
const VERTICAL_LANE: u32 = 2u;
const FLAG_BIN_MEAN: u32 = 4u;
const FLAG_POINT: u32 = 8u;

const NO_RECORD: u32 = 0x00FFFFFFu;
// Horizontal slack, in points, kept around the lane when a quad is clamped to it.
const CLAMP_MARGIN: f32 = 64.0;
const POINT_HALF_SIZE: f32 = 4.0;
const CHEVRON_PERIOD: f32 = 12.0;

struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) start: i32,
    @location(1) length: u32,
    @location(2) y: vec2<f32>,
    @location(3) packing: u32,
    @location(4) parameter: u32,
    @location(5) record: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) color: vec4<f32>,
    // Point of the fragment, in logical points.
    @location(1) point: vec2<f32>,
    // True (unclamped) left, top, right, bottom of the shape, in logical points.
    @location(2) @interpolate(flat) bounds: vec4<f32>,
    @location(3) @interpolate(flat) shape: u32,
    @location(4) @interpolate(flat) highlight: u32,
};

// The two integer-valued f32 words of a signed tile-local offset: the multiple of 4096 at or
// below it, and the remainder in 0..4095. Both are exact in f32 for |offset| <= 7e8.
fn signed_offset_words(offset: i32) -> vec2<f32> {
    let high = offset & i32(-4096);
    return vec2<f32>(f32(high), f32(offset - high));
}

fn track_color(token: u32, parameter: u32) -> vec4<f32> {
    let slot = token & 63u;
    if slot == 0u {
        return vec4<f32>(
            f32(parameter & 255u),
            f32((parameter >> 8u) & 255u),
            f32((parameter >> 16u) & 255u),
            f32(parameter >> 24u),
        ) / 255.0;
    }
    let low = track_palette.colors[slot];
    if (token & 128u) == 0u {
        return low;
    }
    let high = track_palette.colors[(slot + 1u) & 63u];
    return low + (high - low) * bitcast<f32>(parameter);
}

fn value_to_y(value: f32) -> f32 {
    let span = max(lane.value_range.y - lane.value_range.x, 1.0e-20);
    let fraction = clamp((value - lane.value_range.x) / span, 0.0, 1.0);
    return lane.lane_origin.y + lane.lane_size.y * (1.0 - fraction);
}

@vertex
fn vertex_main(input: VertexInput) -> VertexOutput {
    let row = input.packing & 0xFFFFu;
    let shape = (input.packing >> 16u) & 0xFFu;
    let flags = input.packing >> 24u;
    let vertical = flags & 3u;

    var left = lane.lane_origin.x + project_genomic_x(
        signed_offset_words(input.start),
        lane.origin_whole,
        lane.origin_fraction,
        lane.points_per_base,
    );
    var right = lane.lane_origin.x + project_genomic_x(
        signed_offset_words(input.start + i32(input.length)),
        lane.origin_whole,
        lane.origin_fraction,
        lane.points_per_base,
    );

    var top: f32;
    var bottom: f32;
    if vertical == VERTICAL_VALUE {
        var from_value = value_to_y(input.y.x);
        let to_value = value_to_y(input.y.y);
        if (flags & FLAG_BIN_MEAN) != 0u && lane.bin_style == 1u {
            from_value = to_value;
        }
        top = min(from_value, to_value);
        bottom = max(from_value, to_value);
    } else if vertical == VERTICAL_LANE {
        top = lane.lane_origin.y + input.y.x * lane.lane_size.y;
        bottom = lane.lane_origin.y + input.y.y * lane.lane_size.y;
    } else {
        let row_top = lane.lane_origin.y + lane.row_offset + f32(row) * lane.row_pitch;
        top = row_top + input.y.x * lane.row_height;
        bottom = row_top + input.y.y * lane.row_height;
    }

    // One-point minimum sizes: nothing that exists in the data vanishes at any zoom.
    if (flags & FLAG_POINT) != 0u {
        let center = vec2<f32>((left + right) * 0.5, (top + bottom) * 0.5);
        left = center.x - POINT_HALF_SIZE;
        right = center.x + POINT_HALF_SIZE;
        top = center.y - POINT_HALF_SIZE;
        bottom = center.y + POINT_HALF_SIZE;
    } else {
        if right - left < 1.0 {
            let center = (left + right) * 0.5;
            left = center - 0.5;
            right = center + 0.5;
        }
        if bottom - top < 1.0 {
            let center = (top + bottom) * 0.5;
            top = center - 0.5;
            bottom = center + 0.5;
        }
    }

    // The quad is clamped to the lane (plus slack) so interpolated points stay small and exact
    // however long the shape is; the fragment stage works from the true bounds.
    let low_x = lane.lane_origin.x - CLAMP_MARGIN;
    let high_x = lane.lane_origin.x + lane.lane_size.x + CLAMP_MARGIN;
    var quad_top = top;
    var quad_bottom = bottom;
    if shape == SHAPE_ARC {
        // Room for the stroke around the apex and the feet.
        quad_top = top - 1.5;
        quad_bottom = bottom + 1.5;
    }
    let corner = quad_corner(input.vertex_index);
    let point = vec2<f32>(
        mix(clamp(left, low_x, high_x), clamp(right, low_x, high_x), corner.x),
        mix(quad_top, quad_bottom, corner.y),
    );

    let record = input.record & NO_RECORD;
    var highlight = 0u;
    if record != NO_RECORD {
        if record == lane.selected {
            highlight = 1u;
        } else if record == lane.hovered {
            highlight = 2u;
        }
    }

    var output: VertexOutput;
    output.position = points_to_clip(point, lane.screen_size);
    output.color = track_color(input.record >> 24u, input.parameter);
    output.point = point;
    output.bounds = vec4<f32>(left, top, right, bottom);
    output.shape = shape;
    output.highlight = highlight;
    return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let size = input.bounds.zw - input.bounds.xy;
    let local = input.point - input.bounds.xy;
    let half_height = size.y * 0.5;
    var coverage = 1.0;

    if input.shape == SHAPE_ARROW_RIGHT || input.shape == SHAPE_ARROW_LEFT {
        let head = min(size.y * 0.55, size.x * 0.5);
        var from_tip = input.bounds.z - input.point.x;
        if input.shape == SHAPE_ARROW_LEFT {
            from_tip = local.x;
        }
        if from_tip < head {
            let allowed = half_height * clamp(from_tip / max(head, 1.0e-6), 0.0, 1.0);
            if abs(local.y - half_height) > allowed {
                discard;
            }
        }
    } else if input.shape == SHAPE_LINE
        || input.shape == SHAPE_LINE_CHEVRON_RIGHT
        || input.shape == SHAPE_LINE_CHEVRON_LEFT {
        let from_centre = abs(local.y - half_height);
        var inside = from_centre <= 0.5;
        if input.shape != SHAPE_LINE && from_centre <= 3.0 {
            // A chevron every CHEVRON_PERIOD points, anchored to the shape's own start so it
            // does not slide against the feature while panning.
            var along = fract(local.x / CHEVRON_PERIOD) * CHEVRON_PERIOD - CHEVRON_PERIOD * 0.5;
            if input.shape == SHAPE_LINE_CHEVRON_LEFT {
                along = -along;
            }
            inside = inside || abs(along + from_centre - 3.0) <= 0.6;
        }
        if !inside {
            discard;
        }
    } else if input.shape == SHAPE_ARC {
        let fraction = clamp(local.x / max(size.x, 1.0e-6), 0.0, 1.0);
        let offset = fraction - 0.5;
        let curve = size.y * 4.0 * offset * offset;
        let slope = size.y * 8.0 * offset / max(size.x, 1.0);
        let distance = abs(local.y - curve) / sqrt(1.0 + slope * slope);
        coverage = 1.0 - smoothstep(0.25, 1.25, distance);
        if coverage <= 0.0 {
            discard;
        }
    } else if input.shape == SHAPE_DIAMOND {
        let half = max(size * 0.5, vec2<f32>(1.0e-6, 1.0e-6));
        let reach = abs(local.x - half.x) / half.x + abs(local.y - half.y) / half.y;
        if reach > 1.0 {
            discard;
        }
    }

    if input.highlight == 1u {
        return vec4<f32>(mix(input.color.rgb, vec3<f32>(0.16, 0.42, 0.92), 0.58), 1.0);
    }
    if input.highlight == 2u {
        return vec4<f32>(mix(input.color.rgb, vec3<f32>(0.28, 0.52, 0.94), 0.26), 1.0);
    }
    return vec4<f32>(input.color.rgb, input.color.a * coverage);
}
