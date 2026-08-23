use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{DetailLevel, GenomicInterval, IntervalError};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    interval: GenomicInterval,
    pixel_width: f64,
    #[serde(default)]
    start_offset: f64,
    #[serde(default)]
    exact_width: f64,
}

impl Viewport {
    pub fn new(interval: GenomicInterval, pixel_width: f64) -> Result<Self, ViewportError> {
        if !pixel_width.is_finite() || pixel_width <= 0.0 {
            return Err(ViewportError::InvalidPixelWidth(pixel_width));
        }
        Ok(Self {
            interval,
            pixel_width,
            start_offset: 0.0,
            exact_width: interval.len() as f64,
        })
    }

    pub const fn interval(self) -> GenomicInterval {
        self.interval
    }

    pub const fn pixel_width(self) -> f64 {
        self.pixel_width
    }

    /// Fractional zero-based start used by the visual camera. The enclosing
    /// integer interval remains available for indexed genomic queries.
    pub fn exact_start(self) -> f64 {
        self.interval.start() as f64 + self.start_offset_value()
    }

    /// Visible genomic width, which may differ from the enclosing query
    /// interval when the camera lies between bases.
    pub fn span(self) -> f64 {
        if self.exact_width.is_finite() && self.exact_width >= 1.0 {
            self.exact_width
        } else {
            self.interval.len() as f64
        }
    }

    pub fn bases_per_pixel(self) -> f64 {
        self.span() / self.pixel_width
    }

    pub fn detail_level(self) -> DetailLevel {
        DetailLevel::for_bases_per_pixel(self.bases_per_pixel())
    }

    pub fn genomic_at_pixel(self, x: f64) -> f64 {
        self.exact_start() + x.clamp(0.0, self.pixel_width) * self.bases_per_pixel()
    }

    pub fn pixel_at_genomic(self, position: u64) -> f64 {
        (position as f64 - self.exact_start()) / self.bases_per_pixel()
    }

    pub fn resized(self, pixel_width: f64) -> Result<Self, ViewportError> {
        if !pixel_width.is_finite() || pixel_width <= 0.0 {
            return Err(ViewportError::InvalidPixelWidth(pixel_width));
        }
        Ok(Self {
            pixel_width,
            ..self
        })
    }

    pub fn pan_pixels(self, delta_x: f64, contig_length: u64) -> Result<Self, ViewportError> {
        let delta_bases = delta_x * self.bases_per_pixel();
        Self::from_exact(
            self.interval.contig(),
            self.exact_start() + delta_bases,
            self.span(),
            self.pixel_width,
            contig_length,
        )
    }

    /// Re-centres the existing span on a genomic position while respecting
    /// contig boundaries.
    pub fn centered_on(self, position: u64, contig_length: u64) -> Result<Self, ViewportError> {
        let width = self.span().min(contig_length.max(1) as f64);
        Self::from_exact(
            self.interval.contig(),
            position as f64 - width * 0.5,
            width,
            self.pixel_width,
            contig_length,
        )
    }

    /// Zoom around a pixel anchor. Factors below one zoom in and above one zoom out.
    pub fn zoom_at(
        self,
        anchor_x: f64,
        factor: f64,
        contig_length: u64,
    ) -> Result<Self, ViewportError> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(ViewportError::InvalidZoomFactor(factor));
        }

        let new_width = (self.span() * factor).clamp(1.0, contig_length.max(1) as f64);
        let anchor_fraction = (anchor_x / self.pixel_width).clamp(0.0, 1.0);
        let anchor_position = self.genomic_at_pixel(anchor_x);
        Self::from_exact(
            self.interval.contig(),
            anchor_position - anchor_fraction * new_width,
            new_width,
            self.pixel_width,
            contig_length,
        )
    }

    fn start_offset_value(self) -> f64 {
        if self.start_offset.is_finite() && self.start_offset >= 0.0 {
            self.start_offset
        } else {
            0.0
        }
    }

    fn from_exact(
        contig: crate::ContigId,
        requested_start: f64,
        requested_width: f64,
        pixel_width: f64,
        contig_length: u64,
    ) -> Result<Self, ViewportError> {
        if !pixel_width.is_finite() || pixel_width <= 0.0 {
            return Err(ViewportError::InvalidPixelWidth(pixel_width));
        }
        let contig_width = contig_length.max(1) as f64;
        let exact_width = requested_width.clamp(1.0, contig_width);
        let exact_start = requested_start.clamp(0.0, (contig_width - exact_width).max(0.0));
        let interval_start = exact_start.floor() as u64;
        let interval_end = (exact_start + exact_width).ceil().min(contig_width) as u64;
        let interval = GenomicInterval::bounded(
            contig,
            interval_start,
            interval_end.max(interval_start.saturating_add(1)),
            contig_length.max(1),
        )?;
        Ok(Self {
            interval,
            pixel_width,
            start_offset: exact_start - interval_start as f64,
            exact_width,
        })
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum ViewportError {
    #[error("viewport pixel width must be finite and positive, got {0}")]
    InvalidPixelWidth(f64),
    #[error("zoom factor must be finite and positive, got {0}")]
    InvalidZoomFactor(f64),
    #[error(transparent)]
    InvalidInterval(#[from] IntervalError),
}

#[cfg(test)]
mod tests {
    use crate::ContigId;

    use super::*;

    fn viewport() -> Viewport {
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 100, 1_100).unwrap(),
            1_000.0,
        )
        .unwrap()
    }

    #[test]
    fn cursor_centred_zoom_preserves_anchor() {
        let original = viewport();
        let anchor_before = original.genomic_at_pixel(250.0);
        let zoomed = original.zoom_at(250.0, 0.5, 10_000).unwrap();
        let anchor_after = zoomed.genomic_at_pixel(250.0);
        assert!((anchor_before - anchor_after).abs() <= 1.0);
    }

    #[test]
    fn pan_clamps_to_contig() {
        let original = viewport();
        let left = original.pan_pixels(-10_000.0, 10_000).unwrap();
        let right = original.pan_pixels(20_000.0, 10_000).unwrap();
        assert_eq!(left.interval().start(), 0);
        assert_eq!(right.interval().end(), 10_000);
    }

    #[test]
    fn base_scale_pan_retains_fractional_camera_motion() {
        let contig = ContigId::new(0);
        let mut view =
            Viewport::new(GenomicInterval::new(contig, 100, 101).unwrap(), 1_000.0).unwrap();
        for _ in 0..100 {
            view = view.pan_pixels(1.0, 10_000).unwrap();
        }
        assert!((view.exact_start() - 100.1).abs() < 1.0e-9);
        assert!((view.span() - 1.0).abs() < f64::EPSILON);
        assert!((view.pixel_at_genomic(101) - 900.0).abs() < 1.0e-6);
        assert_eq!(view.interval().start(), 100);
        assert_eq!(view.interval().end(), 102);
    }

    #[test]
    fn fractional_zoom_preserves_the_exact_anchor() {
        let mut view = viewport();
        let anchor_x = 371.25;
        let anchor = view.genomic_at_pixel(anchor_x);
        for _ in 0..40 {
            view = view.zoom_at(anchor_x, 0.8, 10_000).unwrap();
        }
        assert!((view.genomic_at_pixel(anchor_x) - anchor).abs() < 1.0e-9);
        assert!(view.span() < 1.0 + f64::EPSILON);
    }

    #[test]
    fn recentering_from_an_ideogram_click_clamps_at_contig_edges() {
        let original = viewport();
        let left = original.centered_on(20, 10_000).unwrap();
        let middle = original.centered_on(5_000, 10_000).unwrap();
        let right = original.centered_on(9_980, 10_000).unwrap();
        assert_eq!((left.interval().start(), left.interval().end()), (0, 1_000));
        assert_eq!(
            (middle.interval().start(), middle.interval().end()),
            (4_500, 5_500)
        );
        assert_eq!(
            (right.interval().start(), right.interval().end()),
            (9_000, 10_000)
        );
    }

    #[test]
    fn pixel_genome_transforms_round_trip_across_viewports() {
        let widths = [1.0, 127.5, 800.0, 3_840.0];
        let spans = [1_u64, 17, 1_000, 1_000_000];
        for width in widths {
            for span in spans {
                let start = 91_337_u64;
                let view = Viewport::new(
                    GenomicInterval::new(ContigId::new(0), start, start + span).unwrap(),
                    width,
                )
                .unwrap();
                for step in 0..=100 {
                    let pixel = width * f64::from(step) / 100.0;
                    let genomic = view.genomic_at_pixel(pixel);
                    let genomic_offset = genomic - start as f64;
                    let round_trip = genomic_offset / view.bases_per_pixel();
                    assert!((round_trip - pixel).abs() <= 1.0e-6);
                }
            }
        }
    }
}
