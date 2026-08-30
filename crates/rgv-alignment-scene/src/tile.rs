use std::num::NonZeroU64;

use rgv_core::{ContigId, GenomicInterval};
use thiserror::Error;

/// Measurement-selected canonical genomic tile identity.
///
/// Tile width is part of the key rather than a hidden application constant. Changing the measured
/// scheme therefore creates different cache identities instead of ambiguously reusing old pages.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CanonicalGenomicTile {
    contig: ContigId,
    coordinate: u64,
    bases: NonZeroU64,
}

impl CanonicalGenomicTile {
    #[must_use]
    pub const fn new(contig: ContigId, coordinate: u64, bases: NonZeroU64) -> Self {
        Self {
            contig,
            coordinate,
            bases,
        }
    }

    #[must_use]
    pub const fn contig(self) -> ContigId {
        self.contig
    }

    #[must_use]
    pub const fn coordinate(self) -> u64 {
        self.coordinate
    }

    #[must_use]
    pub const fn bases(self) -> NonZeroU64 {
        self.bases
    }

    pub fn interval(self, contig_length: u64) -> Result<GenomicInterval, CanonicalTileError> {
        let start = self
            .coordinate
            .checked_mul(self.bases.get())
            .ok_or(CanonicalTileError::CoordinateOverflow)?;
        if start >= contig_length {
            return Err(CanonicalTileError::OutsideContig {
                start,
                contig_length,
            });
        }
        let end = start.saturating_add(self.bases.get()).min(contig_length);
        GenomicInterval::new(self.contig, start, end)
            .map_err(|_| CanonicalTileError::CoordinateOverflow)
    }

    /// Tests the canonical tile's nominal span without requiring a contig-length lookup.
    #[must_use]
    pub fn overlaps(self, interval: GenomicInterval) -> bool {
        if self.contig != interval.contig() {
            return false;
        }
        let Some(start) = self.coordinate.checked_mul(self.bases.get()) else {
            return false;
        };
        let end = start.saturating_add(self.bases.get());
        start < interval.end() && end > interval.start()
    }

    pub fn covering(
        interval: GenomicInterval,
        bases: NonZeroU64,
        contig_length: u64,
    ) -> Result<Vec<Self>, CanonicalTileError> {
        if interval.end() > contig_length {
            return Err(CanonicalTileError::OutsideContig {
                start: interval.end(),
                contig_length,
            });
        }
        let first = interval.start() / bases.get();
        let last = interval.end().saturating_sub(1) / bases.get();
        let capacity = usize::try_from(last.saturating_sub(first).saturating_add(1))
            .map_err(|_| CanonicalTileError::CoordinateOverflow)?;
        let mut tiles = Vec::with_capacity(capacity);
        for coordinate in first..=last {
            let tile = Self::new(interval.contig(), coordinate, bases);
            tile.interval(contig_length)?;
            tiles.push(tile);
        }
        Ok(tiles)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CanonicalTileError {
    #[error("canonical tile coordinate overflows genomic coordinates")]
    CoordinateOverflow,
    #[error("canonical tile starts at {start}, outside contig length {contig_length}")]
    OutsideContig { start: u64, contig_length: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_width_is_explicit_and_terminal_tile_is_clipped() {
        let tile = CanonicalGenomicTile::new(ContigId::new(2), 2, NonZeroU64::new(100).unwrap());
        assert_eq!(
            tile.interval(250).unwrap(),
            GenomicInterval::new(ContigId::new(2), 200, 250).unwrap()
        );
    }

    #[test]
    fn tile_outside_contig_is_rejected() {
        let tile = CanonicalGenomicTile::new(ContigId::new(0), 3, NonZeroU64::new(100).unwrap());
        assert_eq!(
            tile.interval(250),
            Err(CanonicalTileError::OutsideContig {
                start: 300,
                contig_length: 250,
            })
        );
    }
}
