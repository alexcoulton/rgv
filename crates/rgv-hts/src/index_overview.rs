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

use crate::AlignmentSourceFormat;

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
        // One pass over the index: `nth` per contig would walk the reference list again for
        // every contig, which is quadratic on assemblies with many scaffolds.
        let mut references = index.reference_sequences();
        let mapped_record_counts = dictionary
            .iter()
            .map(|_| {
                references.next().and_then(|reference| {
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
    local_alignment_index_candidates(alignment_path, index_override, AlignmentSourceFormat::Bam)
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

pub(crate) fn local_alignment_index_candidates(
    alignment_path: &Path,
    index_override: Option<&Path>,
    format: AlignmentSourceFormat,
) -> Vec<PathBuf> {
    if let Some(index) = index_override {
        return vec![index.to_owned()];
    }
    let suffixes: &[&str] = match format {
        AlignmentSourceFormat::Bam => &[".bai", ".csi"],
        AlignmentSourceFormat::Cram => &[".crai"],
    };
    let mut candidates = Vec::with_capacity(suffixes.len().saturating_mul(2));
    for suffix in suffixes {
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
    // An index answers a region query from its smallest-bin windows alone (`2^min_shift`
    // bases): both the overlapping bins and the linear-index bound depend only on the first
    // and last window touched. Overview bins of a short contig mostly share windows, so the
    // previous answer is reused instead of asking the same question up to 256 times.
    let window_shift = u32::from(index.min_shift());
    let mut previous: Option<((u64, u64), u64)> = None;
    for (bin_index, density) in densities.iter_mut().enumerate() {
        let start = scaled_position(bin_index, contig_length);
        let end = scaled_position(bin_index + 1, contig_length);
        if end <= start {
            continue;
        }
        let windows = (
            start.checked_shr(window_shift).unwrap_or(0),
            (end - 1).checked_shr(window_shift).unwrap_or(0),
        );
        if let Some((_, known)) = previous.filter(|(known, _)| *known == windows) {
            *density = known;
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
        previous = Some((windows, *density));
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

    /// The straightforward definition: one index query per overview bin.
    fn density_bins_by_query(
        index: &dyn BinningIndex,
        reference_sequence_id: usize,
        contig_length: u64,
    ) -> Vec<u64> {
        (0..ALIGNMENT_INDEX_OVERVIEW_BINS)
            .map(|bin_index| {
                let start = scaled_position(bin_index, contig_length);
                let end = scaled_position(bin_index + 1, contig_length);
                if end <= start {
                    return 0;
                }
                let start = Position::try_from(usize::try_from(start + 1).unwrap()).unwrap();
                let end = Position::try_from(usize::try_from(end).unwrap()).unwrap();
                index
                    .query(reference_sequence_id, (start..=end).into())
                    .unwrap()
                    .iter()
                    .map(|chunk| chunk_compressed_weight(*chunk))
                    .sum()
            })
            .collect()
    }

    #[test]
    fn window_reuse_gives_the_same_density_as_one_query_per_bin() {
        let index_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam.bai");
        if !index_path.exists() {
            return;
        }
        let index = noodles_bam::bai::fs::read(index_path).unwrap();
        let references = BinningIndex::reference_sequences(&index).count();
        assert!(references > 0);
        let mut populated = 0;
        for reference_sequence_id in 0..references {
            // Short contigs share windows between bins; long ones do not. Lengths that are not
            // multiples of the window size exercise the boundary arithmetic.
            for contig_length in [1, 255, 16_384, 100_003, 2_000_000, 250_000_000] {
                let expected = density_bins_by_query(&index, reference_sequence_id, contig_length);
                assert_eq!(
                    index_density_bins(&index, reference_sequence_id, contig_length),
                    expected,
                    "reference {reference_sequence_id}, length {contig_length}"
                );
                populated += usize::from(expected.iter().any(|density| *density > 0));
            }
        }
        assert!(
            populated > 0,
            "the fixture index should describe some alignments"
        );
    }

    #[test]
    fn conventional_index_candidates_cover_appended_and_replaced_extensions() {
        let candidates = local_alignment_index_candidates(
            Path::new("reads.bam"),
            None,
            AlignmentSourceFormat::Bam,
        );
        assert_eq!(
            candidates,
            [
                PathBuf::from("reads.bam.bai"),
                PathBuf::from("reads.bai"),
                PathBuf::from("reads.bam.csi"),
                PathBuf::from("reads.csi"),
            ]
        );
        assert_eq!(
            local_alignment_index_candidates(
                Path::new("reads.cram"),
                None,
                AlignmentSourceFormat::Cram,
            ),
            [
                PathBuf::from("reads.cram.crai"),
                PathBuf::from("reads.crai")
            ]
        );
    }
}
