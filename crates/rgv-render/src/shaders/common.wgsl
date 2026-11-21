// Shared by every RGV pipeline. `shaders.rs` prepends this chunk to each pipeline's own source
// when the shader module is created, so there is exactly one copy of these rules.

// Corner of the unit quad for vertex `vertex_index` of a six-vertex, two-triangle instance.
fn quad_corner(vertex_index: u32) -> vec2<f32> {
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

// Logical points (origin top-left, y down) to clip space for a target of `screen_size` points.
fn points_to_clip(point: vec2<f32>, screen_size: vec2<f32>) -> vec4<f32> {
    let normalized = point / screen_size;
    return vec4<f32>(
        normalized.x * 2.0 - 1.0,
        1.0 - normalized.y * 2.0,
        0.0,
        1.0,
    );
}

// Genomic x projection. The camera origin and the page-local offset are each close to the
// distance between the camera and the page start (megabases), while their sum is a few hundred
// bases at most. Cancelling them in f32, even with error-free double-single transforms, is not
// safe: backends that compile with fast-math (Metal does by default) reassociate float sums and
// drop the low words, which a GPU readback test measured as geometry stepping by
// `ulp(distance)` bases (1.2 px at 1 Mb and 20 px/base) instead of following the camera.
// The cancellation is therefore done in integers, which no optimiser may reorder. The contract:
// `page_offset` is a whole number of bases split into two integer-valued f32 words (high word
// plus remainder), and the camera origin is a whole-base delta plus a fraction in [0, 1).
// Nothing here may add a float to `page_offset` before it is converted: a fractional-offset
// term written as `page_offset.x - floor(page_offset.x)` was reassociated in exactly that way.
fn project_genomic_x(
    page_offset: vec2<f32>,
    origin_whole: i32,
    origin_fraction: f32,
    points_per_base: vec2<f32>,
) -> f32 {
    // +-7e8 keeps the three-term integer sum inside i32 for any input.
    let high_whole = i32(clamp(page_offset.x, -7.0e8, 7.0e8));
    let low_whole = i32(clamp(page_offset.y, -7.0e8, 7.0e8));
    let whole = origin_whole + high_whole + low_whole;
    let bases = f32(whole) + origin_fraction;
    return bases * points_per_base.x + bases * points_per_base.y;
}
