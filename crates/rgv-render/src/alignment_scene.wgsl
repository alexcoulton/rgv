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

// How far a soft-clipped ghost base moves from its read's colour towards the ghost slot's
// colour; mirrored by `CLIP_GHOST_TINT` in `palette.rs`.
const CLIP_GHOST_TINT: f32 = 0.5;

// The direction arrowhead's length along the read, as a fraction of the read height, at most
// `ARROWHEAD_MAXIMUM_BASES` bases; a head shorter than `ARROWHEAD_MINIMUM_LENGTH` points is not
// drawn. Mirrored by `ARROWHEAD_LENGTH_FRACTION`, `ARROWHEAD_MAXIMUM_BASES` and
// `ARROWHEAD_MINIMUM_LENGTH` in `rgv-alignment-scene`.
const ARROWHEAD_LENGTH: f32 = 0.55;
const ARROWHEAD_MAXIMUM_BASES: f32 = 10.0;
const ARROWHEAD_MINIMUM_LENGTH: f32 = 0.5;

// Point markers and clip cut edges: full size from `BOUNDARY_MARK_FULL` points per base, a plain
// `BOUNDARY_MARK_HAIRLINE_WIDTH`-point hairline from `BOUNDARY_MARK_HAIRLINE`, hidden below.
// Mirrored by `boundary_mark_detail` in `rgv-alignment-scene`.
const BOUNDARY_MARK_FULL: f32 = 1.0;
const BOUNDARY_MARK_HAIRLINE: f32 = 0.1;
const BOUNDARY_MARK_HAIRLINE_WIDTH: f32 = 1.0;
// Soft-clip ghost cells are drawn only once their lowercase letters fit (the CPU label
// projector's minimum base glyph width), so no pale letterless blocks appear zoomed out.
const CLIP_GHOST_MINIMUM_POINTS_PER_BASE: f32 = 7.0;
// Point markers are I-beams: a bar a quarter of a base wide within [MINIMUM, MAXIMUM] points,
// with serifs across the top and bottom of the read reaching `INSERTION_SERIF_WIDTH` points
// beyond the bar on each side, `INSERTION_SERIF_THICKNESS_FRACTION` of the read height thick
// within [MINIMUM, MAXIMUM] points. Mirrored by the `INSERTION_*` constants, `point_marker_extent`
// and `insertion_mark_covers` in `rgv-alignment-scene`.
const INSERTION_BAR_BASE_FRACTION: f32 = 0.25;
const INSERTION_BAR_MINIMUM_WIDTH: f32 = 2.0;
const INSERTION_BAR_MAXIMUM_WIDTH: f32 = 3.0;
const INSERTION_SERIF_WIDTH: f32 = 2.5;
const INSERTION_SERIF_THICKNESS_FRACTION: f32 = 0.15;
const INSERTION_SERIF_MINIMUM_THICKNESS: f32 = 1.0;
const INSERTION_SERIF_MAXIMUM_THICKNESS: f32 = 2.0;

// One sixteen-byte instance (`AlignmentGpuInstance`):
//   span.x     whole bases from the page start to the primitive's start (bits 0..30)
//   span.y     whole bases covered (bits 0..30)
//              bits 30..32 of span.x (low) and span.y (high): for shapes 3 and 4, whole bases
//              from the shape's tip-side edge to its read's tip; see `tip_bases`
//   packed.x   row offset in the page (bits 0..20) | shape (20..24) | palette index (24..32)
//   packed.y   colour parameter word; see `palette_color`
struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) span: vec2<u32>,
    @location(1) packed: vec2<u32>,
};

struct InteractiveVertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) span: vec2<u32>,
    @location(1) packed: vec2<u32>,
    @location(2) interaction_flags: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) local_point: vec2<f32>,
    @location(2) @interpolate(flat) size: vec2<f32>,
    @location(3) @interpolate(flat) shape: u32,
    // The read's arrowhead length, for direction terminals; see `arrowhead_length`.
    @location(4) @interpolate(flat) head: f32,
    // For direction terminals, the points between the shape's tip-side edge and the read's tip.
    @location(5) @interpolate(flat) tip_offset: f32,
};

const SPAN_MASK: u32 = 0x3FFFFFFFu;

// Whole bases from a direction terminal's tip-side edge to its read's tip, carried in the top
// two bits of `span.x` (low) and `span.y` (high). Mirrored by `AlignmentGpuInstance::bases_from_tip`.
fn tip_bases(span: vec2<u32>) -> u32 {
    return (span.x >> 30u) | ((span.y >> 30u) << 2u);
}

fn camera_points_per_base() -> f32 {
    return camera.points_per_base.x + camera.points_per_base.y;
}

// The direction arrowhead's length for a read drawn `height` points tall: camera-global, never
// the width of the primitive being cut, so every tip primitive shares one diagonal. Zero means
// the read ends square. Mirrored by `arrowhead_length` in `rgv-alignment-scene`.
fn arrowhead_length(height: f32) -> f32 {
    let length = min(height * ARROWHEAD_LENGTH, camera_points_per_base() * ARROWHEAD_MAXIMUM_BASES);
    return select(length, 0.0, length < ARROWHEAD_MINIMUM_LENGTH);
}

// `page_offset` is a whole number of bases from the page start; see `page_offset_words`.
fn project_x(page_offset: u32) -> f32 {
    return camera.clip_origin.x + project_genomic_x(
        page_offset_words(page_offset),
        camera.page_start_whole,
        camera.page_start_fraction,
        camera.points_per_base,
    );
}

fn interaction_color(color: vec4<f32>, flags: u32) -> vec4<f32> {
    var target_color = color.rgb;
    var amount = 0.0;
    if (flags & 2u) != 0u {
        target_color = palette_slot(TOKEN_INTERACTION_SELECTED).rgb;
        amount = 0.58;
    } else if (flags & 1u) != 0u {
        target_color = palette_slot(TOKEN_INTERACTION_HOVERED).rgb;
        amount = 0.32;
    } else if (flags & 4u) != 0u {
        target_color = palette_slot(TOKEN_INTERACTION_SELECTED_CHAIN).rgb;
        amount = 0.38;
    } else if (flags & 8u) != 0u {
        target_color = palette_slot(TOKEN_INTERACTION_HIGHLIGHTED).rgb;
        amount = 0.46;
    }
    // The alpha is kept, so a faint modification cell on a selected read stays faint; mirrored
    // by `AlignmentPalette::interaction_color`.
    return vec4<f32>(mix(color.rgb, target_color, amount), color.a);
}

fn instance_color(packed: vec2<u32>) -> vec4<f32> {
    return palette_color(packed.x >> 24u, packed.y);
}

fn project_instance(
    vertex_index: u32,
    span: vec2<u32>,
    packed: vec2<u32>,
    color: vec4<f32>,
) -> VertexOutput {
    let corner = quad_corner(vertex_index);
    let row_offset = packed.x & 0xFFFFFu;
    let shape = (packed.x >> 20u) & 0xFu;
    let start = span.x & SPAN_MASK;
    var left = project_x(start);
    var right = project_x(start + (span.y & SPAN_MASK));
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
    let points_per_base = camera_points_per_base();
    let full_marks = points_per_base >= BOUNDARY_MARK_FULL;
    let hidden_marks = points_per_base < BOUNDARY_MARK_HAIRLINE;
    var output_shape = shape;
    // Point markers (2: start edge, 5: end edge of the one-base span) are I-beams centred on
    // that base boundary: the quad spans the bar plus both serifs and `fragment_main` cuts the
    // I out of it. Zoomed out they are a plain one-point hairline (drawn as a rectangle, shape
    // 0), and further out nothing; mirrored by `point_marker_extent` in `rgv-alignment-scene`.
    if shape == 2u || shape == 5u {
        var marker_width = BOUNDARY_MARK_HAIRLINE_WIDTH;
        if full_marks {
            marker_width = clamp(
                (right - left) * INSERTION_BAR_BASE_FRACTION,
                INSERTION_BAR_MINIMUM_WIDTH,
                INSERTION_BAR_MAXIMUM_WIDTH,
            ) + 2.0 * INSERTION_SERIF_WIDTH;
        } else {
            output_shape = 0u;
        }
        let boundary = select(left, right, shape == 5u);
        left = boundary - marker_width * 0.5;
        right = left + marker_width;
        if hidden_marks {
            right = left;
        }
    } else if shape >= 6u && shape <= 13u {
        // Clip cut edges (6 + at_end + 2 hard + 4 behind_arrow), just inside the start (or end)
        // edge of the one-base span: a soft clip's sawtooth is half a base wide within [2, 6]
        // points, a hard clip's bar two; zoomed out either is a plain one-point hairline (drawn
        // as a rectangle, shape 0), and further out nothing. Behind an arrowhead on the same
        // boundary (the read's tip) the edge moves inward by the arrowhead's length (see
        // `arrowhead_length`). Mirrored by `clip_edge_extent` in `rgv-alignment-scene`.
        let bits = shape - 6u;
        let cell = right - left;
        var edge_width = BOUNDARY_MARK_HAIRLINE_WIDTH;
        if !full_marks {
            output_shape = 0u;
        } else if (bits & 2u) == 0u {
            edge_width = clamp(cell * 0.5, 2.0, 6.0);
        } else {
            edge_width = 2.0;
        }
        var inset = 0.0;
        if (bits & 4u) != 0u {
            inset = arrowhead_length(height);
        }
        if (bits & 1u) != 0u {
            right = right - inset;
            left = right - edge_width;
        } else {
            left = left + inset;
            right = left + edge_width;
        }
        if hidden_marks {
            right = left;
        }
    } else if shape == 3u {
        // A right-pointing direction terminal keeps its tip-side edge (on the read's end, or
        // `tip_bases` short of it): its one-point minimum grows leftwards. Mirrored by
        // `directional_extent` in `rgv-alignment-scene`.
        left = min(left, right - 1.0);
    } else if shape == 14u && points_per_base < CLIP_GHOST_MINIMUM_POINTS_PER_BASE {
        // Ghost cells appear only together with their letters. Mirrored by `clip_ghost_visible`.
        right = left;
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
    if shape == 14u {
        // A soft-clipped ghost base: its read's colour (after any interaction emphasis), faded
        // towards the ghost slot and drawn with its alpha; see `clip_ghost_color`.
        let tint = palette_slot(TOKEN_CLIP_GHOST);
        output.color = vec4<f32>(mix(color.rgb, tint.rgb, CLIP_GHOST_TINT), tint.a);
    }
    output.local_point = corner * vec2<f32>(right - left, height);
    output.size = vec2<f32>(right - left, height);
    output.shape = output_shape;
    output.head = arrowhead_length(height);
    output.tip_offset = f32(tip_bases(span)) * points_per_base;
    return output;
}

@vertex
fn vertex_main(input: VertexInput) -> VertexOutput {
    return project_instance(
        input.vertex_index,
        input.span,
        input.packed,
        instance_color(input.packed),
    );
}

@vertex
fn interaction_vertex_main(input: InteractiveVertexInput) -> VertexOutput {
    return project_instance(
        input.vertex_index,
        input.span,
        input.packed,
        interaction_color(instance_color(input.packed), input.interaction_flags),
    );
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if input.shape == 3u || input.shape == 4u {
        // The read's one arrowhead. Every primitive at the tip (aligned block, one-base terminal,
        // base cell) ends on the read's tip and is cut by this same diagonal: the head's length
        // depends on the read height only, never on the primitive's own width, so a primitive
        // narrower than the head is trimmed rather than re-pointed at its own scale. A cell or
        // block piece `tip_bases` short of the tip is cut by the same diagonal, measured from
        // the read's tip `tip_offset` points beyond its own tip-side edge. A zero head (far
        // zoomed out) leaves the read's end square. Mirrored by `arrowhead_covers` in
        // `rgv-alignment-scene`.
        var distance_from_tip = input.size.x - input.local_point.x;
        if input.shape == 4u {
            distance_from_tip = input.local_point.x;
        }
        distance_from_tip = distance_from_tip + input.tip_offset;
        let head_width = input.head;
        if distance_from_tip < head_width {
            let half_height = input.size.y * 0.5;
            let allowed = half_height * clamp(distance_from_tip / head_width, 0.0, 1.0);
            if abs(input.local_point.y - half_height) > allowed {
                discard;
            }
        }
    }
    if input.shape == 2u || input.shape == 5u {
        // An insertion's I-beam: the centred bar, plus a serif across the top and bottom of the
        // read. Mirrored by `insertion_mark_covers` in `rgv-alignment-scene`.
        let bar = max(input.size.x - 2.0 * INSERTION_SERIF_WIDTH, 0.0);
        let serif = clamp(
            input.size.y * INSERTION_SERIF_THICKNESS_FRACTION,
            INSERTION_SERIF_MINIMUM_THICKNESS,
            INSERTION_SERIF_MAXIMUM_THICKNESS,
        );
        let in_bar = abs(input.local_point.x - input.size.x * 0.5) < bar * 0.5;
        let in_serif = input.local_point.y < serif || input.local_point.y > input.size.y - serif;
        if !in_bar && !in_serif {
            discard;
        }
    }
    if input.shape >= 6u && input.shape <= 13u && ((input.shape - 6u) & 2u) == 0u {
        // A soft clip's cut edge: a zigzag stroke 1.5 points thick whose vertices alternate
        // between the read boundary and the inner side, with teeth about four points tall.
        // Mirrored by `clip_edge_covers` in `rgv-alignment-scene`.
        let width = input.size.x;
        let height = input.size.y;
        let teeth = max(floor(height / 4.0 + 0.5), 1.0);
        let period = height / teeth;
        let cycles = input.local_point.y / period;
        let phase = cycles - floor(cycles);
        let triangle = 1.0 - abs(2.0 * phase - 1.0);
        let amplitude = max(width - 1.5, 0.0);
        let centre = 0.75 + amplitude * triangle;
        let slope = 2.0 * amplitude / period;
        let half_stroke = 0.75 * sqrt(1.0 + slope * slope);
        var from_boundary = input.local_point.x;
        if ((input.shape - 6u) & 1u) != 0u {
            from_boundary = width - input.local_point.x;
        }
        if abs(from_boundary - centre) > half_stroke {
            discard;
        }
    }
    return input.color;
}
