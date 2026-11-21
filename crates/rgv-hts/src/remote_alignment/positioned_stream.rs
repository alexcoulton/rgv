//! Resumable positioned acquisition for the pure-Rust alignment decoders (local CRAM, remote BAM
//! and remote CRAM). It mirrors the local BAM positioned stream: one checkpoint performs a bounded
//! physical unit of source work and returns the eligible records of one physical owner chunk.

use std::collections::{BTreeMap, VecDeque};

use noodles::{bam, bgzf::VirtualPosition, cram, sam};
use rgv_core::GenomicInterval;

use super::{
    BamBlockStream, CramSliceSpan, RemoteIndex, SelectedCramSlices, bam_placement_intersects,
    bam_placement_starts_at_or_after_end, bam_record_placement, select_cram_containers,
    visit_cram_container_records,
};
use crate::{
    AlignmentError, AlignmentSourcePosition, PositionedAlignmentBlock,
    PositionedAlignmentStreamCheckpoint, PositionedAlignmentStreamCompletion,
    PositionedChunkReceipt, QueryOptions,
    backend_record::{AsBackendRecord, PositionedAdmission, PositionedBlockBuilder},
    positioned_resume::{ChunkCompletion, ChunkExtent},
};

pub(super) struct NoodlesPositionedStream {
    admission: PositionedAdmission,
    work: StreamWork,
    ready: VecDeque<(PositionedAlignmentBlock, Option<PositionedChunkReceipt>)>,
    /// Receipt of the block the last checkpoint returned.
    receipt: Option<PositionedChunkReceipt>,
}

enum StreamWork {
    Bam {
        chunks: VecDeque<(VirtualPosition, VirtualPosition)>,
        resume: Option<VirtualPosition>,
        /// Reading never resumes before this offset: everything up to it was skipped.
        floor: VirtualPosition,
        /// The record at `resume` that the last checkpoint read and found to belong to the
        /// next owner block. It is kept instead of being read again, so a stream that is
        /// simply continued never seeks backwards (a seek restarts a multithreaded reader).
        held: Option<HeldBamRecord>,
    },
    Cram {
        containers: VecDeque<(u64, SelectedCramSlices)>,
    },
}

/// A decoded record together with the virtual offsets at which it starts and ends.
struct HeldBamRecord {
    record: bam::Record,
    before: VirtualPosition,
    after: VirtualPosition,
}

impl NoodlesPositionedStream {
    pub(super) fn begin_bam(
        index: &RemoteIndex,
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
        region: &noodles::core::Region,
        skip: Vec<PositionedChunkReceipt>,
    ) -> Result<(Self, usize), AlignmentError> {
        let mut admission = PositionedAdmission::new(interval, admission_interval, options)?;
        let reference_sequence_id = usize::try_from(interval.contig().get())
            .map_err(|_| AlignmentError::CoordinateOverflow)?;
        let mut chunks = index
            .query(reference_sequence_id, region.interval())?
            .into_iter()
            .map(|chunk| (chunk.start(), chunk.end()))
            .collect::<Vec<_>>();
        // Visit BGZF blocks monotonically so one physical block is returned as one owner chunk.
        chunks.sort_unstable();
        let mut merged = Vec::<(VirtualPosition, VirtualPosition)>::with_capacity(chunks.len());
        for (start, end) in chunks {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        // This decoder recognises a skipped chunk before reading it, by where it starts.
        let skipped = admission.set_skipped(skip, PositionedChunkReceipt::has_bam_start);
        Ok((
            Self {
                admission,
                work: StreamWork::Bam {
                    chunks: merged.into(),
                    resume: None,
                    floor: VirtualPosition::from(0),
                    held: None,
                },
                ready: VecDeque::new(),
                receipt: None,
            },
            skipped,
        ))
    }

    pub(super) fn begin_cram<R>(
        reader: &cram::io::IndexedReader<R>,
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
        skip: Vec<PositionedChunkReceipt>,
    ) -> Result<(Self, usize), AlignmentError>
    where
        R: std::io::Read,
    {
        let mut admission = PositionedAdmission::new(interval, admission_interval, options)?;
        let mut containers = select_cram_containers(reader, interval)?;
        let skipped = admission.set_skipped(skip, PositionedChunkReceipt::is_cram);
        // A skipped slice is never selected: it is neither fetched nor decoded.
        for (container, landmark) in admission.resume().skipped_keys() {
            let Some(landmark) = landmark else { continue };
            if let Some(slices) = containers.get_mut(&container) {
                slices.remove(&landmark);
                if slices.is_empty() {
                    containers.remove(&container);
                }
            }
        }
        Ok((
            Self {
                admission,
                work: StreamWork::Cram {
                    containers: containers.into_iter().collect(),
                },
                ready: VecDeque::new(),
                receipt: None,
            },
            skipped,
        ))
    }

    pub(super) const fn take_receipt(&mut self) -> Option<PositionedChunkReceipt> {
        self.receipt.take()
    }

    fn complete(self) -> PositionedAlignmentStreamCheckpoint {
        let (interval, stats) = self.admission.finish();
        PositionedAlignmentStreamCheckpoint::Complete(PositionedAlignmentStreamCompletion {
            interval,
            stats,
        })
    }

    /// Applies the shared eligibility, admission and retention rules to one record.
    fn process<R>(
        &mut self,
        record: &R,
        header: &sam::Header,
        source_position: AlignmentSourcePosition,
        owner: u64,
        builder: &mut PositionedBlockBuilder,
    ) -> Result<(), AlignmentError>
    where
        R: AsBackendRecord,
    {
        self.admission.note_seen();
        self.admission.process(
            &record.backend_record(header),
            source_position,
            owner,
            builder,
        )
    }
}

/// The index chunks still to read, as virtual-offset ranges without the skipped extents, so
/// the range reader plans (and fetches) only what will be decoded.
fn bam_read_plan(
    chunks: &VecDeque<(VirtualPosition, VirtualPosition)>,
    resume: Option<VirtualPosition>,
    floor: VirtualPosition,
    skipped: impl Iterator<Item = (u64, u64)>,
) -> Vec<(u64, u64)> {
    let mut plan = chunks
        .iter()
        .enumerate()
        .map(|(index, (start, end))| {
            let start = resume.filter(|_| index == 0).unwrap_or(*start).max(floor);
            (u64::from(start), u64::from(*end))
        })
        .filter(|(start, end)| start < end)
        .collect::<Vec<_>>();
    for (skip_start, skip_end) in skipped {
        let mut remaining = Vec::with_capacity(plan.len() + 1);
        for (start, end) in plan {
            if skip_end <= start || end <= skip_start {
                remaining.push((start, end));
                continue;
            }
            if start < skip_start {
                remaining.push((start, skip_start));
            }
            if skip_end < end {
                remaining.push((skip_end, end));
            }
        }
        plan = remaining;
    }
    plan
}

/// Performs at most one BGZF owner block of BAM work.
#[allow(clippy::too_many_lines)]
pub(super) fn next_bam_checkpoint<B: BamBlockStream>(
    slot: &mut Option<NoodlesPositionedStream>,
    reader: &mut B,
    header: &sam::Header,
) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
    let mut state = slot
        .take()
        .ok_or(AlignmentError::NoActivePositionedStream)?;
    state.receipt = None;
    let interval = state.admission.interval;
    let skipping = state.admission.resume().has_skipped();
    let mut builder = PositionedBlockBuilder::default();
    let mut current_block = None::<u64>;
    // Virtual offsets of the first and just past the last record processed into the block.
    let mut block_start = None::<u64>;
    let mut block_end = 0_u64;
    // A record of the target beyond the block being built is known to exist.
    let mut later_record = false;
    let bgzf_reader = reader;
    if let StreamWork::Bam {
        chunks,
        resume,
        floor,
        ..
    } = &state.work
    {
        // The chunks still to be read (the first from where the last checkpoint stopped) are
        // what the range reader should fetch; a narrowed plan keeps its cached bytes.
        bgzf_reader.plan_reads(bam_read_plan(
            chunks,
            *resume,
            *floor,
            state.admission.resume().skipped_extents(),
        ));
    }
    let mut record = bam::Record::default();
    'chunks: loop {
        let StreamWork::Bam {
            chunks,
            resume,
            floor,
            held,
        } = &mut state.work
        else {
            return Err(AlignmentError::NoActivePositionedStream);
        };
        let Some(&(chunk_start, chunk_end)) = chunks.front() else {
            break;
        };
        let position = resume.unwrap_or(chunk_start).max(*floor);
        if position >= chunk_end {
            chunks.pop_front();
            *resume = None;
            continue;
        }
        if skipping {
            let evidence = state
                .admission
                .skipped_target_record_at(u64::from(position));
            if let Some(target) = state.admission.skip_from_position(u64::from(position)) {
                // The caller already holds the chunks lying here: continue past them.
                *floor = VirtualPosition::from(target);
                *resume = Some(*floor);
                later_record |= evidence;
                if current_block.is_some() {
                    break 'chunks;
                }
                continue;
            }
        }
        // The record held from the last checkpoint is the one at `position`, unless reading
        // moved on (a skip); reading then continues just past it.
        let mut held = held.take().filter(|held| held.before == position);
        let read_from = held.as_ref().map_or(position, |held| held.after);
        if bgzf_reader.virtual_position() != read_from {
            bgzf_reader.seek_to_virtual_position(read_from)?;
        }
        let mut direct_reader = bam::io::Reader::from(&mut *bgzf_reader);
        loop {
            let before = held.as_ref().map_or_else(
                || direct_reader.get_ref().virtual_position(),
                |held| held.before,
            );
            let StreamWork::Bam {
                chunks,
                resume,
                floor,
                held: next_held,
            } = &mut state.work
            else {
                unreachable!("stream work kind is fixed");
            };
            if before >= chunk_end {
                chunks.pop_front();
                *resume = None;
                continue 'chunks;
            }
            if skipping {
                let evidence = state.admission.skipped_target_record_at(u64::from(before));
                if let Some(target) = state.admission.skip_from_position(u64::from(before)) {
                    *floor = VirtualPosition::from(target);
                    *resume = Some(*floor);
                    later_record |= evidence;
                    if current_block.is_some() {
                        break 'chunks;
                    }
                    continue 'chunks;
                }
            }
            let after = if let Some(held) = held.take() {
                record = held.record;
                held.after
            } else {
                if direct_reader.read_record(&mut record)? == 0 {
                    chunks.clear();
                    *resume = None;
                    break 'chunks;
                }
                direct_reader.get_ref().virtual_position()
            };
            let end_position = u64::from(after);
            let block = end_position >> 16;
            if current_block.is_some_and(|current| current != block) {
                // The record belongs to the next physical owner; the next checkpoint starts
                // with it. It lies in the target unless it starts at or beyond its end.
                *resume = Some(before);
                later_record |= bam_record_placement(&record)?.is_some_and(|(contig, start, _)| {
                    contig == interval.contig() && start < interval.end()
                });
                *next_held = Some(HeldBamRecord {
                    record: std::mem::take(&mut record),
                    before,
                    after,
                });
                break 'chunks;
            }
            *resume = Some(after);
            let placement = bam_record_placement(&record)?;
            if !bam_placement_intersects(placement, interval) {
                if bam_placement_starts_at_or_after_end(placement, interval) {
                    // Coordinate order: no later record can intersect; the remaining chunks
                    // are never fetched.
                    chunks.clear();
                    *resume = None;
                    break 'chunks;
                }
                continue;
            }
            if current_block.is_none() {
                later_record = false;
                block_start = Some(u64::from(before));
            }
            current_block = Some(block);
            block_end = end_position;
            state.process(
                &record,
                header,
                AlignmentSourcePosition::bam_virtual_offset(end_position),
                block,
                &mut builder,
            )?;
        }
    }
    match current_block {
        Some(block) => {
            let finished = builder.finish(block, interval);
            state.receipt = Some(state.admission.issue_receipt(
                &mut builder,
                ChunkCompletion {
                    owner: block,
                    cram_slice_landmark: None,
                    extent: Some(ChunkExtent {
                        start: block_start,
                        end: block_end,
                    }),
                    left_complete: None,
                    right_complete: later_record,
                },
            ));
            *slot = Some(state);
            Ok(PositionedAlignmentStreamCheckpoint::Block(finished))
        }
        None => Ok(state.complete()),
    }
}

/// Performs at most one CRAM container of work and returns one slice per checkpoint.
pub(super) fn next_cram_checkpoint<R>(
    slot: &mut Option<NoodlesPositionedStream>,
    reader: &mut cram::io::IndexedReader<R>,
    header: &sam::Header,
    slice_span: &CramSliceSpan,
) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError>
where
    R: std::io::Read + std::io::Seek,
{
    let mut state = slot
        .take()
        .ok_or(AlignmentError::NoActivePositionedStream)?;
    state.receipt = None;
    loop {
        if let Some((block, receipt)) = state.ready.pop_front() {
            state.receipt = receipt;
            *slot = Some(state);
            return Ok(PositionedAlignmentStreamCheckpoint::Block(block));
        }
        let StreamWork::Cram { containers } = &mut state.work else {
            return Err(AlignmentError::NoActivePositionedStream);
        };
        let Some((container_offset, landmarks)) = containers.pop_front() else {
            return Ok(state.complete());
        };
        let interval = state.admission.interval;
        let mut slices = BTreeMap::<u64, PositionedBlockBuilder>::new();
        visit_cram_container_records(
            reader,
            header,
            interval,
            container_offset,
            &landmarks,
            slice_span,
            |record, position| {
                let AlignmentSourcePosition::CramRecord { slice_landmark, .. } = position else {
                    return Err(AlignmentError::PositionedStreamUnsupported);
                };
                // Skipped slices lying before this one count first, as a fresh stream would
                // have counted them.
                state
                    .admission
                    .account_skipped_before((container_offset, slice_landmark));
                let builder = slices.entry(slice_landmark).or_default();
                state.process(&record, header, position, container_offset, builder)
            },
        )?;
        let reference_sequence_id = usize::try_from(interval.contig().get()).ok();
        for (landmark, mut builder) in slices {
            let block = builder.finish(container_offset, interval);
            // A slice whose indexed span lies inside the target holds no record the target
            // does not return, so any later target can reuse it.
            let contained = reference_sequence_id
                .and_then(|id| landmarks.get(&landmark)?.get(&id))
                .is_some_and(|(start, end)| interval.start() <= *start && *end <= interval.end());
            let receipt = state.admission.issue_receipt(
                &mut builder,
                ChunkCompletion {
                    owner: container_offset,
                    cram_slice_landmark: Some(landmark),
                    extent: None,
                    left_complete: Some(contained),
                    right_complete: contained,
                },
            );
            state.ready.push_back((block, Some(receipt)));
        }
        if state.ready.is_empty() {
            // Nothing in this container intersected the target: report bounded progress.
            *slot = Some(state);
            return Ok(PositionedAlignmentStreamCheckpoint::Block(
                PositionedAlignmentBlock {
                    compressed_offset: container_offset,
                    records: Vec::new(),
                    genomic_span: None,
                },
            ));
        }
    }
}
