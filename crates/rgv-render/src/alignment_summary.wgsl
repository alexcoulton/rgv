// Prefixed with `shaders/common.wgsl`; see `shaders.rs`.

struct SummaryUniform {
    screen_size: vec2<f32>,
    clip_origin: vec2<f32>,
    clip_size: vec2<f32>,
    points_per_base: vec2<f32>,
    // Whole-base and fractional parts of (page start - camera start); see `project_genomic_x`.
    page_start_whole: i32,
    page_start_fraction: f32,
    covered_clip_x: vec2<f32>,
    scale_maximum: f32,
    minimum_alternate_fraction: f32,
    transform: u32,
    quality_weighted: u32,
    family: u32,
    hidden_modification_mask: u32,
    modification_color_mode: u32,
    junction_normalization: f32,
    junction_row_count: u32,
    minimum_junction_depth: u32,
    minimum_junction_flank: u32,
    junction_display_mode: u32,
    allele_interpretation: u32,
    bisulfite_context_mask: u32,
    padding: vec2<u32>,
    hovered_junction_span: vec4<f32>,
    selected_junction_span: vec4<f32>,
    junction_interaction: vec4<u32>,
};

@group(0) @binding(0)
var<uniform> frame: SummaryUniform;

// Twelve bytes (`AlignmentSummaryGpuDepthInstance`): `span` is the whole-base start offset from
// the page start and the length.
struct DepthInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) span: vec2<u32>,
    @location(1) depth: u32,
};

struct AlleleInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) start_offset: vec2<f32>,
    @location(1) end_offset: vec2<f32>,
    @location(2) local_depth: u32,
    @location(3) allele: u32,
    @location(4) raw: vec3<f32>,
    @location(5) quality_weighted: vec3<f32>,
    @location(6) interpretation: vec4<u32>,
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

struct JunctionInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) start_offset: vec2<f32>,
    @location(1) end_offset: vec2<f32>,
    @location(2) evidence: vec4<u32>,
    @location(3) identity: vec4<u32>,
};

struct OverlayInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) origin: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) palette_index: u32,
    @location(3) color_parameter: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) @interpolate(flat) visible: u32,
    @location(2) local: vec2<f32>,
    @location(3) @interpolate(flat) pixel_size: vec2<f32>,
    @location(4) @interpolate(flat) shape: u32,
    @location(5) @interpolate(flat) stroke: f32,
    @location(6) @interpolate(flat) junction_component: u32,
};

fn project_x(page_offset: vec2<f32>) -> f32 {
    return frame.clip_origin.x + project_genomic_x(
        page_offset,
        frame.page_start_whole,
        frame.page_start_fraction,
        frame.points_per_base,
    );
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
    let point = origin + quad_corner(vertex_index) * size;
    var output: VertexOutput;
    output.position = points_to_clip(point, frame.screen_size);
    output.color = color;
    output.visible = visible;
    output.local = quad_corner(vertex_index);
    output.pixel_size = size;
    output.shape = 0u;
    output.stroke = 0.0;
    output.junction_component = 3u;
    return output;
}

fn junction_output(
    vertex_index: u32,
    origin: vec2<f32>,
    size: vec2<f32>,
    color: vec4<f32>,
    visible: u32,
    shape: u32,
    stroke: f32,
    junction_component: u32,
) -> VertexOutput {
    var output = rectangle_output(vertex_index, origin, size, color, visible);
    output.shape = shape;
    output.stroke = stroke;
    output.junction_component = junction_component;
    return output;
}

@vertex
fn depth_vertex(input: DepthInput) -> VertexOutput {
    let left = max(project_x(page_offset_words(input.span.x)), frame.covered_clip_x.x);
    let semantic_right = min(
        project_x(page_offset_words(input.span.x + input.span.y)),
        frame.covered_clip_x.y,
    );
    let visible = select(0u, 1u, semantic_right > left);
    let right = max(semantic_right, left + 1.0);
    let height = max(transformed_depth_fraction(input.depth) * frame.clip_size.y, 1.0);
    let top = frame.clip_origin.y + frame.clip_size.y - height;
    return rectangle_output(
        input.vertex_index,
        vec2<f32>(left, top),
        vec2<f32>(right - left, height),
        // Opaque: bars narrower than one point are widened to one point and then overlap their
        // neighbours, and a translucent colour would blend twice in the overlap.
        palette_slot(TOKEN_SUMMARY_DEPTH),
        visible,
    );
}

// Allele tokens 0..3 are A, C, G, T; anything else is the unknown-base colour.
fn allele_color(allele: u32) -> vec4<f32> {
    return palette_slot(TOKEN_BASE_A + min(allele, TOKEN_BASE_N - TOKEN_BASE_A));
}

@vertex
fn allele_vertex(input: AlleleInput) -> VertexOutput {
    var values = input.raw;
    if frame.quality_weighted != 0u {
        values = input.quality_weighted;
    }
    let generic = input.interpretation.x == 0u && frame.allele_interpretation == 0u;
    let bisulfite = input.interpretation.x == 1u
        && frame.allele_interpretation == 1u
        && (input.interpretation.y & frame.bisulfite_context_mask) != 0u;
    let semantically_visible = select(
        0u,
        1u,
        input.local_depth > 0u
            && values.x > 0.0
            && ((generic && values.y >= frame.minimum_alternate_fraction) || bisulfite),
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
    var color = allele_color(input.allele);
    if bisulfite {
        color = select(
            palette_slot(TOKEN_BISULFITE_PROTECTED),
            palette_slot(TOKEN_BISULFITE_CONVERTED),
            input.interpretation.w == 1u,
        );
    }
    return rectangle_output(
        input.vertex_index,
        vec2<f32>(left, top),
        vec2<f32>(right - left, height),
        color,
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
        color = palette_slot(TOKEN_SUMMARY_MODIFICATION_BACKGROUND);
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
            color = palette_slot(TOKEN_SUMMARY_MODIFICATION);
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

fn junction_annotation_color(annotation: u32) -> vec4<f32> {
    if annotation == 1u {
        return palette_slot(TOKEN_JUNCTION_ANNOTATED);
    }
    if annotation == 2u {
        return palette_slot(TOKEN_JUNCTION_NOVEL);
    }
    return palette_slot(TOKEN_SUMMARY_NEUTRAL);
}

fn junction_interaction_matches(
    input: JunctionInput,
    span: vec4<f32>,
    component: u32,
    present: u32,
) -> bool {
    return present != 0u
        && input.identity.y == component
        && all(input.start_offset == span.xy)
        && all(input.end_offset == span.zw);
}

@vertex
fn junction_vertex(input: JunctionInput) -> VertexOutput {
    var visible = select(
        0u,
        1u,
        input.evidence.y >= frame.minimum_junction_depth
            && input.evidence.z >= frame.minimum_junction_flank
            && input.evidence.w >= frame.minimum_junction_flank,
    );
    // The quad covers only the visible (covered and clipped) part of the junction, but the arc
    // itself is parameterised over the junction's full, unclamped span below. Clamping the span
    // instead would squeeze the whole parabola into the visible part, so an end that is really
    // off-screen would appear to land on the viewport edge.
    // Offsets are signed: a junction entering the first prepared page from the left carries a
    // negative `start_offset` (its true genomic start), which the double-single projection
    // handles like any other page-relative coordinate.
    let full_left = project_x(input.start_offset);
    let full_right = project_x(input.end_offset);
    let left = max(full_left, frame.covered_clip_x.x);
    let semantic_right = min(full_right, frame.covered_clip_x.y);
    visible *= select(0u, 1u, semantic_right > left);
    let right = max(semantic_right, left + 1.0);
    var row = 0u;
    var row_count = 1u;
    if frame.junction_display_mode != 0u {
        row = input.identity.x;
        row_count = max(frame.junction_row_count, 1u);
        visible *= select(0u, 1u, row < row_count);
    }
    let band_height = frame.clip_size.y / f32(row_count);
    let baseline = frame.clip_origin.y + (f32(row) + 0.5) * band_height;
    let component = input.identity.y;
    if component == 3u {
        visible *= select(0u, 1u, input.identity.z != 0u);
        return junction_output(
            input.vertex_index,
            vec2<f32>((left + right) * 0.5 - 2.5, baseline - 2.5),
            vec2<f32>(5.0, 5.0),
            junction_annotation_color(input.identity.z),
            visible,
            0u,
            0.0,
            3u,
        );
    }
    visible *= select(0u, 1u, input.evidence.x > 0u);
    let component_value = f32(input.evidence.x) * frame.junction_normalization;
    let fraction = sqrt(clamp(component_value / max(frame.scale_maximum, 0.000001), 0.0, 1.0));
    let height = max(band_height * 0.45 * (0.18 + 0.82 * fraction), 3.0);
    let downward = component == 1u;
    let top = select(baseline - height, baseline, downward);
    var color = palette_slot(TOKEN_JUNCTION_UNKNOWN);
    if component == 0u {
        color = palette_slot(TOKEN_JUNCTION_POSITIVE);
    } else if component == 1u {
        color = palette_slot(TOKEN_JUNCTION_NEGATIVE);
    }
    let hovered = junction_interaction_matches(
        input,
        frame.hovered_junction_span,
        frame.junction_interaction.x,
        frame.junction_interaction.y,
    );
    let selected = junction_interaction_matches(
        input,
        frame.selected_junction_span,
        frame.junction_interaction.z,
        frame.junction_interaction.w,
    );
    var stroke = clamp(1.0 + height * 0.075, 1.25, 4.5);
    if selected {
        color = mix(color, palette_slot(TOKEN_JUNCTION_INTERACTION), 0.38);
        stroke = min(stroke + 2.0, 6.5);
    } else if hovered {
        color = mix(color, palette_slot(TOKEN_JUNCTION_INTERACTION), 0.22);
        stroke = min(stroke + 1.25, 5.75);
    }
    var output = junction_output(
        input.vertex_index,
        vec2<f32>(left, top),
        vec2<f32>(right - left, height),
        color,
        visible,
        select(1u, 2u, downward),
        stroke,
        component,
    );
    // `local.x` is the arc parameter (0 at the junction start, 1 at its end) and `pixel_size.x`
    // the full arc width; the fragment stage therefore draws the slice of the true curve that
    // falls inside this quad. Mirrored by `project_alignment_summary_junction_instance` and the
    // CPU summary projector, which both report the unclamped arc bounds.
    let full_width = max(full_right - full_left, 1.0);
    let arc_parameter = (vec2<f32>(left, right) - vec2<f32>(full_left)) / full_width;
    output.local.x = mix(arc_parameter.x, arc_parameter.y, quad_corner(input.vertex_index).x);
    output.pixel_size = vec2<f32>(full_width, height);
    return output;
}

@vertex
fn overlay_vertex(input: OverlayInput) -> VertexOutput {
    return rectangle_output(
        input.vertex_index,
        input.origin,
        input.size,
        palette_color(input.palette_index, input.color_parameter),
        1u,
    );
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    if input.visible == 0u {
        discard;
    }
    var color = input.color;
    if input.shape == 1u || input.shape == 2u {
        let inner_height = max(input.pixel_size.y - input.stroke, 0.0);
        let parabola = 4.0 * (input.local.x - 0.5) * (input.local.x - 0.5);
        var curve_y = input.stroke * 0.5 + inner_height * parabola;
        if input.shape == 2u {
            curve_y = input.pixel_size.y - input.stroke * 0.5 - inner_height * parabola;
        }
        // Perpendicular (not vertical) distance to the curve, so the stroke keeps its thickness
        // on the steep flanks. Same first-order form as `rectangle.wgsl` and
        // `junction_arc_geometry_at_point`; mirrored by `junction_arc_stroke_distance`.
        let slope = 8.0 * inner_height * (input.local.x - 0.5) / max(input.pixel_size.x, 1.0);
        let distance = abs(input.local.y * input.pixel_size.y - curve_y)
            / sqrt(1.0 + slope * slope);
        var alpha = 1.0 - smoothstep(input.stroke * 0.5, input.stroke * 0.5 + 1.0, distance);
        if input.pixel_size.x >= 12.0 && input.junction_component < 2u {
            let marker_fraction = select(0.70, 0.30, input.junction_component == 1u);
            let marker_x = input.pixel_size.x * marker_fraction;
            let marker_parabola = 4.0 * (marker_fraction - 0.5) * (marker_fraction - 0.5);
            var marker_y = input.stroke * 0.5 + inner_height * marker_parabola;
            if input.shape == 2u {
                marker_y = input.pixel_size.y - input.stroke * 0.5 - inner_height * marker_parabola;
            }
            let point = input.local * input.pixel_size;
            let delta = point - vec2<f32>(marker_x, marker_y);
            if abs(delta.x) <= 3.0 && abs(delta.y) <= 3.0 {
                let directed_x = select(delta.x, -delta.x, input.junction_component == 1u);
                let chevron_distance = abs(abs(delta.y) - 0.5 * (2.0 - directed_x));
                let marker_alpha = 1.0 - smoothstep(0.65, 1.45, chevron_distance);
                alpha = max(alpha, marker_alpha);
            }
        }
        if alpha <= 0.0 {
            discard;
        }
        color.a *= alpha;
    }
    return color;
}
