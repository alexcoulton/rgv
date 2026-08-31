use std::ops::Range;

use rgv_core::{GenomicInterval, Viewport};
use thiserror::Error;

use crate::AlignmentCamera;

/// Read-row work requested alongside an exact genomic scene camera.
///
/// Genomic summaries do not require a visible read lane. Keeping that state as an explicit
/// variant avoids manufacturing render geometry merely to send a viewport to the source
/// coordinator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlignmentSceneRowDemand {
    #[default]
    None,
    Visible {
        start: u32,
        end: u32,
    },
}

impl AlignmentSceneRowDemand {
    pub fn visible(rows: Range<u32>) -> Result<Self, AlignmentSceneCameraError> {
        if rows.start >= rows.end {
            return Err(AlignmentSceneCameraError::InvalidVisibleRows {
                start: rows.start,
                end: rows.end,
            });
        }
        Ok(Self::Visible {
            start: rows.start,
            end: rows.end,
        })
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        matches!(self, Self::None)
    }

    #[must_use]
    pub fn rows(self) -> Range<u32> {
        match self {
            Self::None => 0..0,
            Self::Visible { start, end } => start..end,
        }
    }
}

/// Headless source/preparation camera for one alignment-scene lane.
///
/// Unlike [`AlignmentCamera`], this type contains no clip rectangle, row height, or other render
/// geometry. It is therefore valid for a panel that displays Coverage, Sashimi, or modification
/// frequency while its Reads lane is collapsed. The exact fractional [`Viewport`] drives
/// genomic acquisition and summary projection; row demand independently controls read-page work.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSceneCamera {
    viewport: Viewport,
    row_demand: AlignmentSceneRowDemand,
}

impl AlignmentSceneCamera {
    /// Creates a genomic-only camera with no visible-read demand.
    #[must_use]
    pub const fn without_visible_rows(viewport: Viewport) -> Self {
        Self {
            viewport,
            row_demand: AlignmentSceneRowDemand::None,
        }
    }

    pub fn with_visible_rows(
        viewport: Viewport,
        visible_rows: Range<u32>,
    ) -> Result<Self, AlignmentSceneCameraError> {
        Ok(Self {
            viewport,
            row_demand: AlignmentSceneRowDemand::visible(visible_rows)?,
        })
    }

    #[must_use]
    pub const fn viewport(self) -> Viewport {
        self.viewport
    }

    #[must_use]
    pub const fn interval(self) -> GenomicInterval {
        self.viewport.interval()
    }

    #[must_use]
    pub fn exact_start(self) -> f64 {
        self.viewport.exact_start()
    }

    #[must_use]
    pub fn span(self) -> f64 {
        self.viewport.span()
    }

    #[must_use]
    pub fn bases_per_pixel(self) -> f64 {
        self.viewport.bases_per_pixel()
    }

    #[must_use]
    pub const fn row_demand(self) -> AlignmentSceneRowDemand {
        self.row_demand
    }

    /// Returns `0..0` for a genomic-only camera.
    #[must_use]
    pub fn visible_rows(self) -> Range<u32> {
        self.row_demand.rows()
    }

    #[must_use]
    pub const fn has_visible_rows(self) -> bool {
        !self.row_demand.is_empty()
    }
}

impl From<AlignmentCamera> for AlignmentSceneCamera {
    fn from(camera: AlignmentCamera) -> Self {
        let visible_rows = camera.visible_rows();
        Self {
            viewport: camera.viewport(),
            row_demand: AlignmentSceneRowDemand::Visible {
                start: visible_rows.start,
                end: visible_rows.end,
            },
        }
    }
}

impl From<&AlignmentCamera> for AlignmentSceneCamera {
    fn from(camera: &AlignmentCamera) -> Self {
        Self::from(*camera)
    }
}

/// Cross-type equality lets existing read-visible callers compare a coordinator frame with the
/// render camera that produced its preparation demand during migration.
impl PartialEq<AlignmentCamera> for AlignmentSceneCamera {
    fn eq(&self, other: &AlignmentCamera) -> bool {
        *self == Self::from(*other)
    }
}

impl PartialEq<AlignmentSceneCamera> for AlignmentCamera {
    fn eq(&self, other: &AlignmentSceneCamera) -> bool {
        AlignmentSceneCamera::from(*self) == *other
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentSceneCameraError {
    #[error("alignment scene visible-row demand is empty or reversed: {start}..{end}")]
    InvalidVisibleRows { start: u32, end: u32 },
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, GenomicInterval};

    use super::*;
    use crate::CpuClipRect;

    fn viewport() -> Viewport {
        Viewport::new(
            GenomicInterval::new(ContigId::new(3), 100, 200).unwrap(),
            400.0,
        )
        .unwrap()
        .pan_pixels(0.5, 1_000)
        .unwrap()
    }

    #[test]
    fn genomic_only_camera_preserves_exact_viewport_without_rows() {
        let viewport = viewport();
        let camera = AlignmentSceneCamera::without_visible_rows(viewport);
        assert_eq!(camera.viewport(), viewport);
        assert_eq!(camera.interval(), viewport.interval());
        assert_eq!(camera.exact_start(), viewport.exact_start());
        assert_eq!(camera.span(), viewport.span());
        assert_eq!(camera.bases_per_pixel(), viewport.bases_per_pixel());
        assert_eq!(camera.visible_rows(), 0..0);
        assert!(!camera.has_visible_rows());
    }

    #[test]
    fn render_camera_conversion_retains_only_viewport_and_visible_rows() {
        let render = AlignmentCamera::from_viewport(
            viewport(),
            7.25,
            10.0,
            8.0,
            CpuClipRect {
                x: 13.0,
                y: 21.0,
                width: 400.0,
                height: 35.0,
            },
        )
        .unwrap();
        let scene = AlignmentSceneCamera::from(render);
        assert_eq!(scene.viewport(), render.viewport());
        assert_eq!(scene.visible_rows(), render.visible_rows());
        assert!(scene.has_visible_rows());
        assert_eq!(scene, render);
    }

    #[test]
    fn visible_row_constructor_rejects_empty_or_reversed_demand() {
        assert_eq!(
            AlignmentSceneCamera::with_visible_rows(viewport(), 4..4),
            Err(AlignmentSceneCameraError::InvalidVisibleRows { start: 4, end: 4 })
        );
        assert_eq!(
            AlignmentSceneCamera::with_visible_rows(viewport(), 5..4),
            Err(AlignmentSceneCameraError::InvalidVisibleRows { start: 5, end: 4 })
        );
    }
}
