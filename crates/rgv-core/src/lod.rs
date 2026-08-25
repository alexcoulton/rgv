use serde::{Deserialize, Serialize};

/// Widest horizontal scale at which individual read/reference base marks are useful.
///
/// At this scale a base occupies one pixel. Base glyphs have a separate, tighter
/// display threshold in the layout layer.
pub const MAXIMUM_BASE_MARK_BASES_PER_PIXEL: f64 = 1.0;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum DetailLevel {
    Overview,
    Regional,
    Reads,
    Bases,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderBudget {
    pub max_primitives: usize,
    pub max_visible_alignments: usize,
}

impl Default for RenderBudget {
    fn default() -> Self {
        Self {
            max_primitives: 250_000,
            max_visible_alignments: 50_000,
        }
    }
}

impl DetailLevel {
    pub fn for_bases_per_pixel(bases_per_pixel: f64) -> Self {
        if bases_per_pixel >= 1_000.0 {
            Self::Overview
        } else if bases_per_pixel >= 10.0 {
            Self::Regional
        } else if bases_per_pixel >= 0.125 {
            Self::Reads
        } else {
            Self::Bases
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_increases_as_scale_decreases() {
        assert_eq!(
            DetailLevel::for_bases_per_pixel(5_000.0),
            DetailLevel::Overview
        );
        assert_eq!(
            DetailLevel::for_bases_per_pixel(100.0),
            DetailLevel::Regional
        );
        assert_eq!(DetailLevel::for_bases_per_pixel(1.0), DetailLevel::Reads);
        assert_eq!(DetailLevel::for_bases_per_pixel(0.01), DetailLevel::Bases);
    }
}
