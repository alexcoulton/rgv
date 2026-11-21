//! WGSL sources and the CPU half of the contracts they share.
//!
//! Every pipeline's shader module is the concatenation of the shared chunks it needs and its own
//! file. WGSL has no include directive and module-scope declarations may appear in any order, so
//! plain concatenation is sufficient and needs no preprocessor.

/// Quad corners, the point-to-clip transform and the fast-math-safe genomic projection.
const COMMON: &str = include_str!("shaders/common.wgsl");
/// Instance layout, varyings and fragment shape rules of the two rectangle pipelines.
const RECTANGLE_SHAPES: &str = include_str!("shaders/rectangle_shapes.wgsl");

/// The semantic palette uniform and `palette_color`, shared by the alignment pipelines.
const PALETTE: &str = include_str!("shaders/palette.wgsl");

/// One pipeline's complete shader: a label and the chunks concatenated to form its source.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ComposedShader {
    pub label: &'static str,
    chunks: &'static [&'static str],
}

pub(crate) const RECTANGLE: ComposedShader = ComposedShader {
    label: "RGV rectangle shader",
    chunks: &[COMMON, RECTANGLE_SHAPES, include_str!("rectangle.wgsl")],
};
pub(crate) const CONTEXT_SCENE: ComposedShader = ComposedShader {
    label: "RGV contextual scene shader",
    chunks: &[COMMON, RECTANGLE_SHAPES, include_str!("context_scene.wgsl")],
};
pub(crate) const ALIGNMENT_SCENE: ComposedShader = ComposedShader {
    label: "RGV alignment semantic page shader",
    chunks: &[COMMON, PALETTE, include_str!("alignment_scene.wgsl")],
};
pub(crate) const ALIGNMENT_SUMMARY: ComposedShader = ComposedShader {
    label: "RGV alignment summary semantic page shader",
    chunks: &[COMMON, PALETTE, include_str!("alignment_summary.wgsl")],
};
/// Coverage-pyramid depth bins (minimum, mean and maximum per bin).
pub(crate) const ALIGNMENT_DEPTH_PYRAMID: ComposedShader = ComposedShader {
    label: "RGV alignment depth pyramid shader",
    chunks: &[
        COMMON,
        PALETTE,
        include_str!("alignment_depth_pyramid.wgsl"),
    ],
};
/// The world-space track tile pipeline (features, bins, variants, interactions).
pub(crate) const TRACK_TILE: ComposedShader = ComposedShader {
    label: "RGV track tile shader",
    chunks: &[COMMON, include_str!("track_tile.wgsl")],
};

/// Every shader this crate compiles, for validation tests.
#[cfg(test)]
pub(crate) const ALL: [ComposedShader; 6] = [
    RECTANGLE,
    CONTEXT_SCENE,
    ALIGNMENT_SCENE,
    ALIGNMENT_SUMMARY,
    TRACK_TILE,
    ALIGNMENT_DEPTH_PYRAMID,
];

impl ComposedShader {
    /// The complete WGSL source.
    pub(crate) fn source(&self) -> String {
        let mut source =
            String::with_capacity(self.chunks.iter().map(|chunk| chunk.len() + 1).sum());
        for chunk in self.chunks {
            source.push_str(chunk);
            // A chunk that does not end in a newline must not run into the next one.
            source.push('\n');
        }
        source
    }

    pub(crate) fn create_module(&self, device: &wgpu::Device) -> wgpu::ShaderModule {
        device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(self.label),
            source: wgpu::ShaderSource::Wgsl(self.source().into()),
        })
    }
}

/// Largest whole-base camera-to-page distance the shaders accept; see `project_genomic_x`.
const MAXIMUM_ORIGIN_WHOLE_BASES: f64 = 7.0e8;

/// Splits `page_start - camera_start` into whole bases (rounded down) and a fraction in
/// `[0, 1)` for the shaders' `project_genomic_x`. The subtraction is exact in f64 for any
/// genomic coordinate; the shader then cancels the whole part against page-local offsets in
/// integer arithmetic, where no fast-math optimiser can reorder it.
pub(crate) fn split_camera_origin(page_start: u64, camera_exact_start: f64) -> (i32, f32) {
    let delta = page_start as f64 - camera_exact_start;
    if !delta.is_finite() {
        return (0, 0.0);
    }
    let whole = delta
        .floor()
        .clamp(-MAXIMUM_ORIGIN_WHOLE_BASES, MAXIMUM_ORIGIN_WHOLE_BASES);
    // `delta - whole` is exact and in [0, 1); rounding it to f32 can reach 1.0, which the
    // shader handles like any other fraction.
    (whole as i32, (delta - whole).clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_origin_splits_into_whole_bases_and_a_unit_fraction() {
        assert_eq!(split_camera_origin(100, 40.0), (60, 0.0));
        assert_eq!(split_camera_origin(100, 40.25), (59, 0.75));
        // The page starts left of the camera: the whole part rounds towards minus infinity.
        assert_eq!(split_camera_origin(0, 1_000_000.25), (-1_000_001, 0.75));
        let (whole, fraction) = split_camera_origin(0, 248_956_421.337);
        assert!((0.0..=1.0).contains(&fraction));
        assert!((f64::from(whole) + f64::from(fraction) + 248_956_421.337).abs() < 1.0e-6);
        // Out-of-range and non-finite origins saturate instead of wrapping.
        assert_eq!(split_camera_origin(u64::MAX, 0.0).0, 700_000_000);
        assert_eq!(split_camera_origin(0, f64::NAN), (0, 0.0));
    }
}
