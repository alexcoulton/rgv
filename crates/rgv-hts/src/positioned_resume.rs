//! Chunk receipts: what a positioned stream remembers about each owner chunk it returned, so
//! a later stream over an overlapping target can seek past that chunk instead of decoding it
//! again, while accounting its records exactly as if it had.

use std::sync::atomic::{AtomicU64, Ordering};

use rgv_core::GenomicInterval;

use crate::{QueryOptions, QueryStats, backend_record::Eligibility};

/// One record a stream saw in a chunk: its placement and how eligibility classified it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChunkEntry {
    start: u64,
    /// Reference bases consumed, saturated; zero for a placed record that consumes none.
    reference_length: u32,
    class: EntryClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryClass {
    Eligible,
    Unmapped,
    Secondary,
    Supplementary,
    User { duplicate: bool, qc_failed: bool },
}

impl ChunkEntry {
    pub(crate) fn new(start: u64, reference_length: u64, eligibility: Eligibility) -> Self {
        Self {
            start,
            reference_length: u32::try_from(reference_length).unwrap_or(u32::MAX),
            class: match eligibility {
                Eligibility::Eligible => EntryClass::Eligible,
                Eligibility::Unmapped => EntryClass::Unmapped,
                Eligibility::Secondary => EntryClass::Secondary,
                Eligibility::Supplementary => EntryClass::Supplementary,
                Eligibility::UserFilter {
                    duplicate,
                    qc_failed,
                } => EntryClass::User {
                    duplicate,
                    qc_failed,
                },
            },
        }
    }

    fn end(self) -> u64 {
        self.start.saturating_add(u64::from(self.reference_length))
    }

    /// Whether a regional query of `interval` returns this record: a record consuming no
    /// reference base occupies one, as in `HTSlib`.
    fn returned_by(self, interval: GenomicInterval) -> bool {
        self.start < interval.end()
            && interval.start() < self.end().max(self.start.saturating_add(1))
    }

    /// Whether the record counts against the Complete-read limit of `admission`.
    fn admitted_by(self, admission: GenomicInterval) -> bool {
        self.reference_length > 0 && self.start < admission.end() && self.end() > admission.start()
    }
}

/// Where a BAM chunk lies in the file, in BGZF virtual offsets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChunkExtent {
    /// Offset of the first record the stream saw in the chunk, when the decoder knows it.
    pub start: Option<u64>,
    /// Offset just past the last record the stream saw in the chunk.
    pub end: u64,
}

/// Receipt of one owner chunk returned by a positioned stream (a BGZF block of a BAM, a slice
/// of a CRAM). It identifies the chunk exactly as the stream reported it and carries what a
/// later stream needs to skip the chunk: where it lies, and the placement and eligibility of
/// every record seen in it.
///
/// Obtain one with `take_positioned_chunk_receipt` after a `Block` checkpoint and hand a set of
/// them to `begin_positioned_stream_resuming`. A receipt is only meaningful for the source that
/// issued it (or another source opened on the same unchanged object).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PositionedChunkReceipt {
    owner: u64,
    cram_slice_landmark: Option<u64>,
    stream: u64,
    ordinal: u64,
    interval: GenomicInterval,
    eligibility: u64,
    /// No record of the chunk lies wholly left of `interval`.
    left_complete: bool,
    /// No record of the chunk starts at or beyond the end of `interval`.
    right_complete: bool,
    /// Every eligible record seen was retained in the returned block.
    records_complete: bool,
    extent: Option<ChunkExtent>,
    entries: Box<[ChunkEntry]>,
}

/// The options that decide which records are eligible, and nothing else: detail and retained
/// payload do not change what a chunk contributes to admission.
fn eligibility_fingerprint(options: &QueryOptions) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in [
        u8::from(options.include_secondary),
        u8::from(options.include_supplementary),
    ]
    .into_iter()
    .chain(options.filter.fingerprint().to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl PositionedChunkReceipt {
    /// The `compressed_offset` of the block this receipt was issued for.
    #[must_use]
    pub const fn compressed_offset(&self) -> u64 {
        self.owner
    }

    /// The slice landmark within the container, for a CRAM chunk.
    #[must_use]
    pub const fn cram_slice_landmark(&self) -> Option<u64> {
        self.cram_slice_landmark
    }

    /// The target of the stream that decoded the chunk.
    #[must_use]
    pub const fn decoded_interval(&self) -> GenomicInterval {
        self.interval
    }

    /// Records the stream saw in the chunk, eligible or not.
    #[must_use]
    pub fn records_seen(&self) -> usize {
        self.entries.len()
    }

    /// Whether a stream over `target` with `options` may skip this chunk: the same contig and
    /// eligibility rules, every eligible record retained when it was decoded, and proof that
    /// every record of the chunk which `target` returns was seen then. The proof holds when
    /// `target` does not extend beyond the decoded interval on a side, or the chunk is known to
    /// hold no record beyond that side (it is interior to the decoded stream).
    ///
    /// Payload compatibility (detail, retained tags, modifications) is the caller's to check:
    /// the skipped chunk's records are whatever the caller kept from the earlier block.
    #[must_use]
    pub fn can_resume(&self, target: GenomicInterval, options: &QueryOptions) -> bool {
        self.interval.contig() == target.contig()
            && self.eligibility == eligibility_fingerprint(options)
            && self.records_complete
            && (target.start() >= self.interval.start() || self.left_complete)
            && (target.end() <= self.interval.end() || self.right_complete)
    }

    /// Heap and inline bytes this receipt retains.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(
            self.entries
                .len()
                .saturating_mul(std::mem::size_of::<ChunkEntry>()),
        )
    }

    /// A BAM chunk whose end offset is known: any BAM decoder can seek past it.
    pub(crate) const fn is_bam(&self) -> bool {
        self.cram_slice_landmark.is_none() && self.extent.is_some()
    }

    /// A BAM chunk whose start offset is known too, so it can be recognised before reading.
    pub(crate) fn has_bam_start(&self) -> bool {
        self.cram_slice_landmark.is_none()
            && self.extent.is_some_and(|extent| extent.start.is_some())
    }

    pub(crate) const fn is_cram(&self) -> bool {
        self.cram_slice_landmark.is_some()
    }

    fn key(&self) -> (u64, u64) {
        (self.owner, self.cram_slice_landmark.unwrap_or(0))
    }
}

/// Facts about a finished chunk that only its decoder knows.
#[derive(Clone, Copy)]
pub(crate) struct ChunkCompletion {
    pub owner: u64,
    pub cram_slice_landmark: Option<u64>,
    pub extent: Option<ChunkExtent>,
    /// Overrides the stream's own left-edge evidence (CRAM knows the slice span).
    pub left_complete: Option<bool>,
    pub right_complete: bool,
}

struct SkippedChunk {
    receipt: PositionedChunkReceipt,
    accounted: bool,
}

/// Receipt bookkeeping of one positioned stream: issues receipts for the chunks it returns and
/// accounts the chunks it was told to skip.
pub(crate) struct ResumeLedger {
    stream: u64,
    next_ordinal: u64,
    eligibility: u64,
    /// Owner of the first seen record that starts at or after the target start. Every record
    /// of a later owner starts there too (coordinate order), so intersects the target or lies
    /// right of it.
    first_in_range_owner: Option<u64>,
    /// Chunks to skip, sorted by owner (and so by file position).
    skipped: Vec<SkippedChunk>,
}

static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);

impl ResumeLedger {
    pub(crate) fn new(options: &QueryOptions) -> Self {
        Self {
            stream: NEXT_STREAM.fetch_add(1, Ordering::Relaxed),
            next_ordinal: 0,
            eligibility: eligibility_fingerprint(options),
            first_in_range_owner: None,
            skipped: Vec::new(),
        }
    }

    /// Installs the chunks to skip, keeping those valid for `target` that the decoder can
    /// physically skip (`usable`). Returns how many.
    pub(crate) fn set_skipped(
        &mut self,
        receipts: Vec<PositionedChunkReceipt>,
        target: GenomicInterval,
        options: &QueryOptions,
        usable: impl Fn(&PositionedChunkReceipt) -> bool,
    ) -> usize {
        let mut skipped = receipts
            .into_iter()
            .filter(|receipt| usable(receipt) && receipt.can_resume(target, options))
            .map(|receipt| SkippedChunk {
                receipt,
                accounted: false,
            })
            .collect::<Vec<_>>();
        skipped.sort_by_key(|chunk| chunk.receipt.key());
        skipped.dedup_by_key(|chunk| chunk.receipt.key());
        self.skipped = skipped;
        self.skipped.len()
    }

    pub(crate) fn has_skipped(&self) -> bool {
        !self.skipped.is_empty()
    }

    pub(crate) fn skipped_keys(&self) -> impl Iterator<Item = (u64, Option<u64>)> + '_ {
        self.skipped
            .iter()
            .map(|chunk| (chunk.receipt.owner, chunk.receipt.cram_slice_landmark))
    }

    /// Virtual-offset ranges of the skipped BAM chunks whose start is known.
    pub(crate) fn skipped_extents(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.skipped.iter().filter_map(|chunk| {
            let extent = chunk.receipt.extent?;
            Some((extent.start?, extent.end))
        })
    }

    pub(crate) fn note_seen_start(&mut self, owner: u64, start: u64, target: GenomicInterval) {
        if start >= target.start() && self.first_in_range_owner.is_none_or(|first| owner < first) {
            self.first_in_range_owner = Some(owner);
        }
    }

    /// The ordinal of a chunk the stream starts reading now. Ordinals follow file order:
    /// two chunks a stream returns are consecutive only when it read nothing and skipped
    /// nothing between them.
    pub(crate) fn reserve_ordinal(&mut self) -> u64 {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        ordinal
    }

    /// Issues the receipt of a finished chunk, with the ordinal reserved when its first
    /// record was read (one is reserved now when none was).
    pub(crate) fn issue(
        &mut self,
        completion: ChunkCompletion,
        target: GenomicInterval,
        entries: Vec<ChunkEntry>,
        records_complete: bool,
        ordinal: Option<u64>,
    ) -> PositionedChunkReceipt {
        let ordinal = ordinal.unwrap_or_else(|| self.reserve_ordinal());
        let left_complete = completion.left_complete.unwrap_or_else(|| {
            target.start() == 0
                || self
                    .first_in_range_owner
                    .is_some_and(|first| first < completion.owner)
        });
        PositionedChunkReceipt {
            owner: completion.owner,
            cram_slice_landmark: completion.cram_slice_landmark,
            stream: self.stream,
            ordinal,
            interval: target,
            eligibility: self.eligibility,
            left_complete,
            right_complete: completion.right_complete,
            records_complete,
            extent: completion.extent,
            entries: entries.into_boxed_slice(),
        }
    }

    /// The skipped chunk that owns a just-decoded record, for a decoder that only learns the
    /// owner after reading (`HTSlib`).
    pub(crate) fn skipped_owner(&self, owner: u64) -> Option<usize> {
        self.skipped
            .binary_search_by_key(&owner, |chunk| chunk.receipt.owner)
            .ok()
    }

    /// Whether the skipped chunk at `index` extends to the virtual offset `record_end`.
    pub(crate) fn covers(&self, index: usize, record_end: u64) -> bool {
        self.skipped[index]
            .receipt
            .extent
            .is_some_and(|extent| record_end <= extent.end)
    }

    /// The skipped chunk whose known extent contains the virtual offset of the next record.
    pub(crate) fn skipped_at(&self, position: u64) -> Option<usize> {
        let index = self.skipped.partition_point(|chunk| {
            chunk
                .receipt
                .extent
                .is_none_or(|extent| extent.end <= position)
        });
        let extent = self.skipped.get(index)?.receipt.extent?;
        (extent.start? <= position).then_some(index)
    }

    /// Last chunk of the run of skipped chunks starting at `index` that one stream returned
    /// consecutively: nothing the target returns lies between them, so a decoder may seek from
    /// the first straight past the last.
    fn run_end(&self, index: usize) -> usize {
        let mut last = index;
        while let Some(next) = self.skipped.get(last + 1) {
            let current = &self.skipped[last].receipt;
            if next.receipt.stream != current.stream || next.receipt.ordinal != current.ordinal + 1
            {
                break;
            }
            last += 1;
        }
        last
    }

    /// Accounts the run of skipped chunks starting at `index` and returns the virtual offset
    /// just past it, where decoding resumes. `None` when the run has no file extent (CRAM).
    pub(crate) fn account_run(
        &mut self,
        index: usize,
        target: GenomicInterval,
        admission: GenomicInterval,
        stats: &mut QueryStats,
        maximum_records: usize,
    ) -> Option<u64> {
        let last = self.run_end(index);
        for chunk in index..=last {
            self.account(chunk, target, admission, stats, maximum_records);
        }
        // The chunks this stream returns on either side of the skipped run are not
        // consecutive: the run lies between them. Leaving an ordinal out records that, so a
        // later stream offered those two receipts without the run's never treats them as one
        // run and seeks over records it does not hold. (A chunk being built when the skip
        // happened reserved its ordinal at its first record, before this gap.)
        self.next_ordinal += 1;
        self.skipped[last].receipt.extent.map(|extent| extent.end)
    }

    /// Accounts every skipped chunk ordered before `(owner, landmark)`.
    pub(crate) fn account_before(
        &mut self,
        key: (u64, u64),
        target: GenomicInterval,
        admission: GenomicInterval,
        stats: &mut QueryStats,
        maximum_records: usize,
    ) {
        let end = self
            .skipped
            .partition_point(|chunk| chunk.receipt.key() < key);
        for chunk in 0..end {
            self.account(chunk, target, admission, stats, maximum_records);
        }
    }

    /// Whether a skipped chunk at `index` holds a record starting before `end`: evidence that
    /// the chunk decoded just before it is not the last one of the target.
    pub(crate) fn starts_before(&self, index: usize, end: u64) -> bool {
        self.skipped[index]
            .receipt
            .entries
            .iter()
            .any(|entry| entry.start < end)
    }

    /// Adds to `stats` exactly what decoding the chunk under this stream would have added.
    fn account(
        &mut self,
        index: usize,
        target: GenomicInterval,
        admission: GenomicInterval,
        stats: &mut QueryStats,
        maximum_records: usize,
    ) {
        let chunk = &mut self.skipped[index];
        if std::mem::replace(&mut chunk.accounted, true) {
            return;
        }
        let owner = chunk.receipt.owner;
        let mut in_range_start = false;
        for entry in chunk
            .receipt
            .entries
            .iter()
            .filter(|entry| entry.returned_by(target))
        {
            in_range_start |= entry.start >= target.start();
            stats.records_seen = stats.records_seen.saturating_add(1);
            match entry.class {
                EntryClass::Eligible => {
                    stats.records_eligible = stats.records_eligible.saturating_add(1);
                    if entry.admitted_by(admission) {
                        stats.complete_read_records_eligible =
                            stats.complete_read_records_eligible.saturating_add(1);
                    }
                    if stats.complete_read_records_eligible <= maximum_records {
                        stats.records_emitted = stats.records_emitted.saturating_add(1);
                    }
                }
                EntryClass::Unmapped => {
                    stats.records_filtered_unmapped =
                        stats.records_filtered_unmapped.saturating_add(1);
                }
                EntryClass::Secondary => {
                    stats.records_filtered_secondary =
                        stats.records_filtered_secondary.saturating_add(1);
                }
                EntryClass::Supplementary => {
                    stats.records_filtered_supplementary =
                        stats.records_filtered_supplementary.saturating_add(1);
                }
                EntryClass::User {
                    duplicate,
                    qc_failed,
                } => {
                    stats.records_filtered_user = stats.records_filtered_user.saturating_add(1);
                    stats.records_filtered_duplicate = stats
                        .records_filtered_duplicate
                        .saturating_add(usize::from(duplicate));
                    stats.records_filtered_qc_failed = stats
                        .records_filtered_qc_failed
                        .saturating_add(usize::from(qc_failed));
                }
            }
        }
        if in_range_start && self.first_in_range_owner.is_none_or(|first| owner < first) {
            self.first_in_range_owner = Some(owner);
        }
    }

    /// Accounts whatever was not reached (chunks holding nothing the target returns).
    pub(crate) fn account_remaining(
        &mut self,
        target: GenomicInterval,
        admission: GenomicInterval,
        stats: &mut QueryStats,
        maximum_records: usize,
    ) {
        for chunk in 0..self.skipped.len() {
            self.account(chunk, target, admission, stats, maximum_records);
        }
    }
}
