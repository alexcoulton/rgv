use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use noodles::{
    bam as noodles_bam,
    core::Position,
    csi::{self, BinningIndex},
};
use rgv_core::{ContigId, ReferenceDictionary};

/// A navigation-oriented alignment density map derived only from index
/// metadata. Values estimate compressed alignment occupancy rather than exact
/// base coverage, which keeps source opening independent of file size.
pub const ALIGNMENT_INDEX_OVERVIEW_BINS: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentIndexOverview {
    contigs: Vec<Vec<u64>>,
    mapped_record_counts: Vec<Option<u64>>,
}

impl AlignmentIndexOverview {
    pub(crate) fn from_binning_index(
        index: &dyn BinningIndex,
        dictionary: &ReferenceDictionary,
    ) -> Self {
        let contigs = dictionary
            .iter()
            .enumerate()
            .map(|(reference_sequence_id, contig)| {
                index_density_bins(index, reference_sequence_id, contig.length())
            })
            .collect();
        let mapped_record_counts = dictionary
            .iter()
            .enumerate()
            .map(|(reference_sequence_id, _)| {
                index
                    .reference_sequences()
                    .nth(reference_sequence_id)
                    .and_then(|reference| {
                        reference
                            .metadata()
                            .map(|metadata| metadata.mapped_record_count())
                    })
            })
            .collect();
        Self {
            contigs,
            mapped_record_counts,
        }
    }

    pub fn contig_bins(&self, contig: ContigId) -> Option<&[u64]> {
        usize::try_from(contig.get())
            .ok()
            .and_then(|index| self.contigs.get(index))
            .map(Vec::as_slice)
    }

    /// Exact mapped-record total stored in the BAI/CSI metadata for one
    /// reference sequence. This is independent of any regional record query
    /// and does not reflect viewer-side filters.
    pub fn contig_mapped_record_count(&self, contig: ContigId) -> Option<u64> {
        usize::try_from(contig.get())
            .ok()
            .and_then(|index| self.mapped_record_counts.get(index))
            .copied()
            .flatten()
    }
}

pub(crate) fn load_local_alignment_index_overview(
    alignment_path: &Path,
    index_override: Option<&Path>,
    dictionary: &ReferenceDictionary,
) -> Option<AlignmentIndexOverview> {
    local_index_candidates(alignment_path, index_override)
        .into_iter()
        .find_map(|path| read_index_overview(&path, dictionary))
}

fn read_index_overview(
    path: &Path,
    dictionary: &ReferenceDictionary,
) -> Option<AlignmentIndexOverview> {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase());
    match extension.as_deref() {
        Some("csi") => csi::fs::read(path)
            .ok()
            .map(|index| AlignmentIndexOverview::from_binning_index(&index, dictionary)),
        Some("bai") => noodles_bam::bai::fs::read(path)
            .ok()
            .map(|index| AlignmentIndexOverview::from_binning_index(&index, dictionary)),
        _ => noodles_bam::bai::fs::read(path)
            .ok()
            .map(|index| AlignmentIndexOverview::from_binning_index(&index, dictionary))
            .or_else(|| {
                csi::fs::read(path)
                    .ok()
                    .map(|index| AlignmentIndexOverview::from_binning_index(&index, dictionary))
            }),
    }
}

fn local_index_candidates(alignment_path: &Path, index_override: Option<&Path>) -> Vec<PathBuf> {
    if let Some(index) = index_override {
        return vec![index.to_owned()];
    }
    let mut candidates = Vec::with_capacity(4);
    for suffix in [".bai", ".csi"] {
        let mut appended = OsString::from(alignment_path.as_os_str());
        appended.push(suffix);
        candidates.push(PathBuf::from(appended));
        candidates.push(alignment_path.with_extension(&suffix[1..]));
    }
    candidates.dedup();
    candidates
}

fn index_density_bins(
    index: &dyn BinningIndex,
    reference_sequence_id: usize,
    contig_length: u64,
) -> Vec<u64> {
    let mut densities = vec![0_u64; ALIGNMENT_INDEX_OVERVIEW_BINS];
    if contig_length == 0 {
        return densities;
    }
    for (bin_index, density) in densities.iter_mut().enumerate() {
        let start = scaled_position(bin_index, contig_length);
        let end = scaled_position(bin_index + 1, contig_length);
        if end <= start {
            continue;
        }
        let Some(start) = usize::try_from(start.saturating_add(1))
            .ok()
            .and_then(|value| Position::try_from(value).ok())
        else {
            continue;
        };
        let Some(end) = usize::try_from(end)
            .ok()
            .and_then(|value| Position::try_from(value).ok())
        else {
            continue;
        };
        let Ok(chunks) = index.query(reference_sequence_id, (start..=end).into()) else {
            continue;
        };
        *density = chunks.iter().fold(0_u64, |total, chunk| {
            total.saturating_add(chunk_compressed_weight(*chunk))
        });
    }
    densities
}

fn scaled_position(bin_boundary: usize, contig_length: u64) -> u64 {
    u64::try_from(
        (bin_boundary as u128 * u128::from(contig_length)) / ALIGNMENT_INDEX_OVERVIEW_BINS as u128,
    )
    .unwrap_or(u64::MAX)
}

fn chunk_compressed_weight(
    chunk: noodles::csi::binning_index::index::reference_sequence::bin::Chunk,
) -> u64 {
    let (start_block, start_offset): (u64, u16) = chunk.start().into();
    let (end_block, end_offset): (u64, u16) = chunk.end().into();
    if end_block > start_block {
        end_block - start_block
    } else {
        u64::from(end_offset > start_offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlignmentSource as _;

    #[test]
    fn bundled_bai_produces_whole_contig_navigation_density() {
        let alignment =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !alignment.exists() {
            return;
        }
        let source =
            crate::LocalAlignmentSource::open(&alignment, &crate::AlignmentOpenOptions::default())
                .unwrap();
        let overview = source.index_overview().expect("mixed BAM has a BAI");
        let bins = overview.contig_bins(ContigId::new(1)).unwrap();

        assert_eq!(bins.len(), ALIGNMENT_INDEX_OVERVIEW_BINS);
        assert!(bins.iter().any(|density| *density > 0));
        assert!(bins.iter().filter(|density| **density > 0).count() > 1);
        assert!(
            overview
                .contig_mapped_record_count(ContigId::new(1))
                .is_some()
        );
    }

    #[test]
    fn conventional_index_candidates_cover_appended_and_replaced_extensions() {
        let candidates = local_index_candidates(Path::new("reads.bam"), None);
        assert_eq!(
            candidates,
            [
                PathBuf::from("reads.bam.bai"),
                PathBuf::from("reads.bai"),
                PathBuf::from("reads.bam.csi"),
                PathBuf::from("reads.csi"),
            ]
        );
    }
}
