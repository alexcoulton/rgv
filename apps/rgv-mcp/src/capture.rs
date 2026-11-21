//! Bounded preparation of viewer PNG captures for inline MCP image content.

use std::io::Cursor;

use image::{ImageFormat, imageops::FilterType};
use thiserror::Error;

/// Default upper bound, in pixels, for the width of an inlined capture.
pub(crate) const DEFAULT_CAPTURE_MAX_WIDTH: u32 = 1_600;
/// Smallest accepted `max_width`; narrower requests are raised to this bound.
pub(crate) const MINIMUM_CAPTURE_MAX_WIDTH: u32 = 64;
/// Largest accepted `max_width`; wider requests are lowered to this bound.
pub(crate) const MAXIMUM_CAPTURE_MAX_WIDTH: u32 = 16_384;

#[derive(Debug, Error)]
pub(crate) enum CaptureError {
    #[error("RGV returned a PNG capture that could not be decoded")]
    Decode,
    #[error("the downscaled PNG capture could not be encoded")]
    Encode,
}

/// A PNG ready to be inlined, with both its own and its source dimensions in pixels.
#[derive(Debug)]
pub(crate) struct InlineCapture {
    pub(crate) png: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) source_width: u32,
    pub(crate) source_height: u32,
}

impl InlineCapture {
    pub(crate) const fn downscaled(&self) -> bool {
        self.width != self.source_width || self.height != self.source_height
    }
}

pub(crate) fn effective_max_width(requested: Option<u32>) -> u32 {
    requested
        .unwrap_or(DEFAULT_CAPTURE_MAX_WIDTH)
        .clamp(MINIMUM_CAPTURE_MAX_WIDTH, MAXIMUM_CAPTURE_MAX_WIDTH)
}

/// Returns the target size for a capture wider than `max_width`, or `None` when the capture
/// already fits. The aspect ratio is preserved (height rounds to nearest, never below one pixel)
/// and a capture is never enlarged.
pub(crate) fn downscaled_dimensions(width: u32, height: u32, max_width: u32) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || max_width == 0 || width <= max_width {
        return None;
    }
    let scaled =
        (u64::from(height) * u64::from(max_width) + u64::from(width) / 2) / u64::from(width);
    let scaled_height = u32::try_from(scaled).unwrap_or(u32::MAX).max(1);
    Some((max_width, scaled_height))
}

/// Downscales `png` to at most `max_width` pixels wide. A capture that already fits is returned
/// byte-for-byte without being decoded.
pub(crate) fn prepare_inline_png(
    png: Vec<u8>,
    source_width: u32,
    source_height: u32,
    max_width: u32,
) -> Result<InlineCapture, CaptureError> {
    let Some((width, height)) = downscaled_dimensions(source_width, source_height, max_width)
    else {
        return Ok(InlineCapture {
            png,
            width: source_width,
            height: source_height,
            source_width,
            source_height,
        });
    };
    let decoded = image::load_from_memory_with_format(&png, ImageFormat::Png)
        .map_err(|_| CaptureError::Decode)?;
    drop(png);
    let resized = decoded.resize_exact(width, height, FilterType::CatmullRom);
    drop(decoded);
    let mut encoded = Cursor::new(Vec::new());
    resized
        .write_to(&mut encoded, ImageFormat::Png)
        .map_err(|_| CaptureError::Encode)?;
    Ok(InlineCapture {
        png: encoded.into_inner(),
        width,
        height,
        source_width,
        source_height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            if (x / 8 + y / 8) % 2 == 0 {
                image::Rgba([255, 255, 255, 255])
            } else {
                image::Rgba([0, 0, 0, 255])
            }
        });
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, ImageFormat::Png).unwrap();
        encoded.into_inner()
    }

    #[test]
    fn downscale_maths_preserves_aspect_ratio_and_never_upscales() {
        assert_eq!(downscaled_dimensions(3200, 1800, 1600), Some((1600, 900)));
        assert_eq!(downscaled_dimensions(3840, 2160, 1600), Some((1600, 900)));
        // 1000 * 1600 / 3001 = 533.16 rounds down; 1001 * 2 / 3 = 667.33 rounds down;
        // 5 * 2 / 4 = 2.5 rounds half up.
        assert_eq!(downscaled_dimensions(3001, 1000, 1600), Some((1600, 533)));
        assert_eq!(downscaled_dimensions(3, 1001, 2), Some((2, 667)));
        assert_eq!(downscaled_dimensions(4, 5, 2), Some((2, 3)));
        assert_eq!(downscaled_dimensions(1600, 900, 1600), None);
        assert_eq!(downscaled_dimensions(800, 600, 1600), None);
        assert_eq!(downscaled_dimensions(0, 600, 1600), None);
        assert_eq!(downscaled_dimensions(800, 600, 0), None);
    }

    #[test]
    fn downscale_maths_keeps_extreme_strips_at_least_one_pixel_high() {
        assert_eq!(downscaled_dimensions(20_000, 1, 1600), Some((1600, 1)));
        assert_eq!(downscaled_dimensions(u32::MAX, u32::MAX, 1), Some((1, 1)));
        assert_eq!(
            downscaled_dimensions(2, u32::MAX, 1),
            Some((1, u32::MAX / 2 + 1))
        );
    }

    #[test]
    fn requested_width_defaults_and_clamps() {
        assert_eq!(effective_max_width(None), DEFAULT_CAPTURE_MAX_WIDTH);
        assert_eq!(effective_max_width(Some(0)), MINIMUM_CAPTURE_MAX_WIDTH);
        assert_eq!(effective_max_width(Some(800)), 800);
        assert_eq!(
            effective_max_width(Some(u32::MAX)),
            MAXIMUM_CAPTURE_MAX_WIDTH
        );
    }

    #[test]
    fn wide_png_is_downscaled_to_the_requested_width() {
        let source = png(400, 100);
        let capture = prepare_inline_png(source.clone(), 400, 100, 100).unwrap();
        assert!(capture.downscaled());
        assert_eq!((capture.width, capture.height), (100, 25));
        assert_eq!((capture.source_width, capture.source_height), (400, 100));
        let decoded = image::load_from_memory_with_format(&capture.png, ImageFormat::Png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (100, 25));
    }

    #[test]
    fn fitting_png_is_returned_untouched_without_decoding() {
        // Not a decodable PNG: proves a capture that fits is never decoded or re-encoded.
        let opaque = b"opaque capture bytes".to_vec();
        let capture = prepare_inline_png(opaque.clone(), 400, 100, 1600).unwrap();
        assert!(!capture.downscaled());
        assert_eq!(capture.png, opaque);
        assert_eq!((capture.width, capture.height), (400, 100));
    }

    #[test]
    fn undecodable_wide_png_fails_instead_of_inlining_the_original() {
        assert!(matches!(
            prepare_inline_png(b"not a png".to_vec(), 4000, 100, 1600),
            Err(CaptureError::Decode)
        ));
    }
}
