use std::{num::NonZeroUsize, sync::Arc};

use rgv_core::{GenomicInterval, OverlappingMatePolicy};

use crate::{
    AlignmentBatch, AlignmentFilter, AlignmentHeaderMetadata, AlignmentReadGroup, AlignmentRecord,
    AuxValue, JunctionSummary, PairSummary, PileupSummary, ReferenceSlice,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QueryDetail {
    Layout,
    /// Layout columns plus sequence/qualities for fixed-position base sorting,
    /// without retaining unrelated auxiliary payload.
    Sequence,
    Full,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq)]
pub struct QueryOptions {
    pub detail: QueryDetail,
    /// Auxiliary tags retained by layout-only queries because the active row
    /// policy uses them. Full-detail queries retain every auxiliary field.
    pub retained_auxiliary_tags: Vec<[u8; 2]>,
    /// The active row policy needs sequence (currently fixed-position base
    /// sorting), so a layout-horizon query cannot omit it.
    pub layout_requires_sequence: bool,
    pub max_records: NonZeroUsize,
    pub cancellation_chunk: NonZeroUsize,
    pub coverage_bins: NonZeroUsize,
    pub collect_alleles: bool,
    pub collect_modifications: bool,
    pub overlapping_mate_policy: OverlappingMatePolicy,
    pub minimum_base_quality: u8,
    pub minimum_junction_flank: u32,
    pub include_secondary: bool,
    pub include_supplementary: bool,
    pub include_reference: bool,
    pub filter: AlignmentFilter,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            detail: QueryDetail::Layout,
            retained_auxiliary_tags: Vec::new(),
            layout_requires_sequence: false,
            max_records: NonZeroUsize::new(50_000).unwrap(),
            cancellation_chunk: NonZeroUsize::new(256).unwrap(),
            coverage_bins: NonZeroUsize::new(1_024).unwrap(),
            collect_alleles: false,
            collect_modifications: false,
            overlapping_mate_policy: OverlappingMatePolicy::default(),
            minimum_base_quality: 0,
            minimum_junction_flank: 0,
            include_secondary: false,
            include_supplementary: true,
            include_reference: false,
            filter: AlignmentFilter::default(),
        }
    }
}

impl QueryOptions {
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut write = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        write(&[self.detail as u8]);
        for tag in &self.retained_auxiliary_tags {
            write(tag);
        }
        write(&[0xff]);
        write(&[u8::from(self.layout_requires_sequence)]);
        write(&self.max_records.get().to_le_bytes());
        write(&self.coverage_bins.get().to_le_bytes());
        write(&[
            u8::from(self.collect_alleles),
            u8::from(self.collect_modifications),
            self.overlapping_mate_policy as u8,
            self.minimum_base_quality,
        ]);
        write(&self.minimum_junction_flank.to_le_bytes());
        write(&[
            u8::from(self.include_secondary),
            u8::from(self.include_supplementary),
            u8::from(self.include_reference),
        ]);
        write(&self.filter.fingerprint().to_le_bytes());
        hash
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IndividualReadStatus {
    #[default]
    Complete,
    TooDense {
        limit: usize,
    },
}

impl IndividualReadStatus {
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueryStats {
    pub records_seen: usize,
    /// Unmapped records ignored even when an index returns a placed record.
    pub records_filtered_unmapped: usize,
    /// Secondary records excluded by the dedicated visibility option.
    pub records_filtered_secondary: usize,
    /// Supplementary records excluded by the dedicated visibility option.
    pub records_filtered_supplementary: usize,
    /// Records rejected by the user-configured flag, quality, or tag filter.
    pub records_filtered_user: usize,
    /// User-filtered records carrying the duplicate flag. This is a subset of
    /// `records_filtered_user` and is therefore not additive with it.
    pub records_filtered_duplicate: usize,
    /// User-filtered records carrying the QC-failed flag. This is a subset of
    /// `records_filtered_user` and is therefore not additive with it.
    pub records_filtered_qc_failed: usize,
    /// Records that passed flags and user filters in the backing query interval.
    pub records_eligible: usize,
    /// Eligible records overlapping the exact foreground admission interval. This equals
    /// `records_eligible` for ordinary exact queries, but excludes offscreen records acquired
    /// only for a tiled or speculative browsing horizon.
    pub complete_read_records_eligible: usize,
    /// Records loaded for individual layout/inspection. This equals
    /// `records_eligible` for a complete result and is zero for Too dense.
    pub records_emitted: usize,
    /// Legacy compatibility flag. A true value now means the individual-read
    /// result is Too dense and the committed batch is empty; it never denotes
    /// a retained partial batch.
    pub truncated: bool,
    /// Actual foreground/horizon admission limit applied to this query after
    /// workspace allocation.
    pub complete_read_limit: usize,
    pub individual_reads: IndividualReadStatus,
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentQuery {
    pub interval: GenomicInterval,
    pub header: Arc<AlignmentHeaderMetadata>,
    pub batch: AlignmentBatch,
    pub pileup: PileupSummary,
    pub junctions: JunctionSummary,
    pub pairs: PairSummary,
    pub reference: Option<ReferenceSlice>,
    pub stats: QueryStats,
}

impl AlignmentQuery {
    #[must_use]
    pub const fn individual_reads_complete(&self) -> bool {
        self.stats.individual_reads.is_complete()
    }

    pub fn estimated_bytes(&self) -> usize {
        self.batch
            .estimated_bytes()
            .saturating_add(self.pileup.estimated_bytes())
            .saturating_add(self.junctions.estimated_bytes())
            .saturating_add(self.pairs.estimated_bytes())
            .saturating_add(
                self.reference
                    .as_ref()
                    .map_or(0, |reference| reference.bases.len()),
            )
    }

    /// Bytes newly allocated by this query beyond any Arc-backed packed storage it references.
    pub fn incremental_estimated_bytes(&self) -> usize {
        self.batch
            .incremental_estimated_bytes()
            .saturating_add(self.pileup.estimated_bytes())
            .saturating_add(self.junctions.estimated_bytes())
            .saturating_add(self.pairs.estimated_bytes())
            .saturating_add(
                self.reference
                    .as_ref()
                    .map_or(0, |reference| reference.bases.len()),
            )
    }

    /// Per-query evidence and projection bytes when its packed arenas are charged elsewhere.
    pub fn shared_view_estimated_bytes(&self) -> usize {
        self.batch
            .shared_view_estimated_bytes()
            .saturating_add(self.pileup.estimated_bytes())
            .saturating_add(self.junctions.estimated_bytes())
            .saturating_add(self.pairs.estimated_bytes())
            .saturating_add(
                self.reference
                    .as_ref()
                    .map_or(0, |reference| reference.bases.len()),
            )
    }

    /// Expected packed/evidence bytes if this logical population were decoded
    /// into its own resident interval. Unlike `estimated_bytes`, this does not
    /// charge unrelated records retained by a zero-copy backing arena.
    pub fn logical_decoded_estimated_bytes(&self) -> usize {
        self.batch
            .logical_decoded_estimated_bytes()
            .saturating_add(self.pileup.estimated_bytes())
            .saturating_add(self.junctions.estimated_bytes())
            .saturating_add(self.pairs.estimated_bytes())
            .saturating_add(
                self.reference
                    .as_ref()
                    .map_or(0, |reference| reference.bases.len()),
            )
    }

    pub fn materialize_record(
        &self,
        index: usize,
    ) -> Result<AlignmentRecord, crate::PackedRecordError> {
        self.batch.materialize_record(index)
    }

    pub fn read_group(&self, index: usize) -> Option<&AlignmentReadGroup> {
        let field = self.batch.auxiliary_field(index, *b"RG").ok()??;
        let AuxValue::String(id) = field.value else {
            return None;
        };
        self.header.read_group(id.as_bytes())
    }
}
