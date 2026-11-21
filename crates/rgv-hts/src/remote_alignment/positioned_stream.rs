//! Resumable positioned acquisition for the pure-Rust alignment decoders (local CRAM, remote BAM
//! and remote CRAM). It mirrors the local BAM positioned stream: one checkpoint performs a bounded
//! physical unit of source work and returns the eligible records of one physical owner chunk.

use std::collections::{BTreeMap, VecDeque};

use noodles::{bam, bgzf::VirtualPosition, cram, sam};
use rgv_core::GenomicInterval;

use super::{
    CramSliceSpan, Eligibility, NoodlesReader, RemoteIndex, SelectedCramSlices, eligibility,
    noodles_record_intersects, noodles_record_starts_at_or_after_end, normalize_record,
    record_reference_end, select_cram_containers, visit_cram_container_records,
};
use crate::{
    AlignmentError, AlignmentSourcePosition, PositionedAlignmentBlock, PositionedAlignmentRecord,
    PositionedAlignmentStreamCheckpoint, PositionedAlignmentStreamCompletion, QueryDetail,
    QueryOptions, QueryStats, local::finalize_positioned_stream_stats,
};

pub(super) struct NoodlesPositionedStream {
    interval: GenomicInterval,
    admission_interval: GenomicInterval,
    options: QueryOptions,
    stats: QueryStats,
    work: StreamWork,
    ready: VecDeque<PositionedAlignmentBlock>,
}

enum StreamWork {
    Bam {
        chunks: VecDeque<(VirtualPosition, VirtualPosition)>,
        resume: Option<VirtualPosition>,
    },
    Cram {
        containers: VecDeque<(u64, SelectedCramSlices)>,
    },
}

#[derive(Default)]
struct BlockBuilder {
    records: Vec<PositionedAlignmentRecord>,
    span: Option<(u64, u64)>,
}

impl BlockBuilder {
    fn finish(mut self, owner_offset: u64, interval: GenomicInterval) -> PositionedAlignmentBlock {
        self.records.sort_by(|left, right| {
            (
                left.record.start,
                left.record.end,
                &left.record.read_name,
                left.source_position,
            )
                .cmp(&(
                    right.record.start,
                    right.record.end,
                    &right.record.read_name,
                    right.source_position,
                ))
        });
        PositionedAlignmentBlock {
            compressed_offset: owner_offset,
            records: self.records,
            genomic_span: self
                .span
                .and_then(|(start, end)| GenomicInterval::new(interval.contig(), start, end).ok()),
        }
    }
}

impl NoodlesPositionedStream {
    fn new(
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
        work: StreamWork,
    ) -> Result<Self, AlignmentError> {
        if !interval.contains_interval(admission_interval) {
            return Err(AlignmentError::AdmissionIntervalOutsideQuery {
                query: interval,
                admission: admission_interval,
            });
        }
        Ok(Self {
            interval,
            admission_interval,
            stats: QueryStats {
                complete_read_limit: options.max_records.get(),
                ..QueryStats::default()
            },
            options,
            work,
            ready: VecDeque::new(),
        })
    }

    pub(super) fn begin_bam(
        index: &RemoteIndex,
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
        region: &noodles::core::Region,
    ) -> Result<Self, AlignmentError> {
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
        Self::new(
            interval,
            admission_interval,
            options,
            StreamWork::Bam {
                chunks: merged.into(),
                resume: None,
            },
        )
    }

    pub(super) fn begin_cram<R>(
        reader: &cram::io::IndexedReader<R>,
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
    ) -> Result<Self, AlignmentError>
    where
        R: std::io::Read,
    {
        let containers = select_cram_containers(reader, interval)?;
        Self::new(
            interval,
            admission_interval,
            options,
            StreamWork::Cram {
                containers: containers.into_iter().collect(),
            },
        )
    }

    fn complete(mut self) -> PositionedAlignmentStreamCheckpoint {
        finalize_positioned_stream_stats(&mut self.stats);
        PositionedAlignmentStreamCheckpoint::Complete(PositionedAlignmentStreamCompletion {
            interval: self.interval,
            stats: self.stats,
        })
    }

    /// Applies the exact local-BAM eligibility, admission and retention rules to one record.
    fn process<R>(
        &mut self,
        record: &R,
        header: &sam::Header,
        source_position: AlignmentSourcePosition,
        builder: &mut BlockBuilder,
    ) -> Result<(), AlignmentError>
    where
        R: sam::alignment::Record,
    {
        self.stats.records_seen = self.stats.records_seen.saturating_add(1);
        let record_span = record
            .alignment_start()
            .transpose()?
            .map(|position| position.get() as u64 - 1)
            .map(|start| record_reference_end(record, start).map(|end| (start, end)))
            .transpose()?
            .filter(|(start, end)| end > start);
        if let Some((start, end)) = record_span {
            builder.span = Some(builder.span.map_or((start, end), |(minimum, maximum)| {
                (minimum.min(start), maximum.max(end))
            }));
        }
        match eligibility(record, header, &self.options)? {
            Eligibility::Eligible => {}
            Eligibility::Unmapped => {
                self.stats.records_filtered_unmapped =
                    self.stats.records_filtered_unmapped.saturating_add(1);
                return Ok(());
            }
            Eligibility::Secondary => {
                self.stats.records_filtered_secondary =
                    self.stats.records_filtered_secondary.saturating_add(1);
                return Ok(());
            }
            Eligibility::Supplementary => {
                self.stats.records_filtered_supplementary =
                    self.stats.records_filtered_supplementary.saturating_add(1);
                return Ok(());
            }
            Eligibility::UserFilter {
                duplicate,
                qc_failed,
            } => {
                self.stats.records_filtered_user =
                    self.stats.records_filtered_user.saturating_add(1);
                self.stats.records_filtered_duplicate = self
                    .stats
                    .records_filtered_duplicate
                    .saturating_add(usize::from(duplicate));
                self.stats.records_filtered_qc_failed = self
                    .stats
                    .records_filtered_qc_failed
                    .saturating_add(usize::from(qc_failed));
                return Ok(());
            }
        }
        self.stats.records_eligible = self.stats.records_eligible.saturating_add(1);
        if record_span.is_some_and(|(start, end)| {
            start < self.admission_interval.end() && end > self.admission_interval.start()
        }) {
            self.stats.complete_read_records_eligible =
                self.stats.complete_read_records_eligible.saturating_add(1);
        }
        if self.stats.complete_read_records_eligible > self.options.max_records.get() {
            return Ok(());
        }
        let (detail, tags): (QueryDetail, &[[u8; 2]]) = if self.options.collect_modifications {
            (QueryDetail::Full, &[])
        } else {
            (self.options.detail, &self.options.retained_auxiliary_tags)
        };
        builder.records.push(PositionedAlignmentRecord {
            record: normalize_record(record, header, detail, tags)?,
            source_position,
        });
        self.stats.records_emitted = self.stats.records_emitted.saturating_add(1);
        Ok(())
    }
}

/// Performs at most one BGZF owner block of remote BAM work.
pub(super) fn next_bam_checkpoint(
    slot: &mut Option<NoodlesPositionedStream>,
    reader: &mut NoodlesReader,
    header: &sam::Header,
) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
    use noodles::bgzf::io::Seek as _;

    let mut state = slot
        .take()
        .ok_or(AlignmentError::NoActivePositionedStream)?;
    let interval = state.interval;
    let mut builder = BlockBuilder::default();
    let mut current_block = None::<u64>;
    let bgzf_reader = reader;
    if let StreamWork::Bam { chunks, resume } = &state.work {
        // The chunks still to be read (the first from where the last checkpoint stopped) are
        // what the range reader should fetch; a narrowed plan keeps its cached bytes.
        bgzf_reader
            .get_mut()
            .set_bgzf_read_plan(chunks.iter().enumerate().map(|(index, (start, end))| {
                let start = resume.filter(|_| index == 0).unwrap_or(*start);
                (u64::from(start), u64::from(*end))
            }));
    }
    let mut record = bam::Record::default();
    'chunks: loop {
        let StreamWork::Bam { chunks, resume } = &mut state.work else {
            return Err(AlignmentError::NoActivePositionedStream);
        };
        let Some(&(chunk_start, chunk_end)) = chunks.front() else {
            break;
        };
        let position = resume.unwrap_or(chunk_start);
        if position >= chunk_end {
            chunks.pop_front();
            *resume = None;
            continue;
        }
        bgzf_reader.seek_to_virtual_position(position)?;
        let mut direct_reader = bam::io::Reader::from(&mut *bgzf_reader);
        loop {
            let before = direct_reader.get_ref().virtual_position();
            if before >= chunk_end {
                let StreamWork::Bam { chunks, resume } = &mut state.work else {
                    unreachable!("stream work kind is fixed");
                };
                chunks.pop_front();
                *resume = None;
                continue 'chunks;
            }
            if direct_reader.read_record(&mut record)? == 0 {
                let StreamWork::Bam { chunks, resume } = &mut state.work else {
                    unreachable!("stream work kind is fixed");
                };
                chunks.clear();
                *resume = None;
                break 'chunks;
            }
            let after = direct_reader.get_ref().virtual_position();
            let end_position = u64::from(after);
            let block = end_position >> 16;
            if current_block.is_some_and(|current| current != block) {
                // The record belongs to the next physical owner; re-read it at the next checkpoint.
                let StreamWork::Bam { resume, .. } = &mut state.work else {
                    unreachable!("stream work kind is fixed");
                };
                *resume = Some(before);
                break 'chunks;
            }
            {
                let StreamWork::Bam { resume, .. } = &mut state.work else {
                    unreachable!("stream work kind is fixed");
                };
                *resume = Some(after);
            }
            if !noodles_record_intersects(&record, header, interval)? {
                if noodles_record_starts_at_or_after_end(&record, header, interval)? {
                    // Coordinate order: no later record can intersect; the remaining chunks
                    // are never fetched.
                    let StreamWork::Bam { chunks, resume } = &mut state.work else {
                        unreachable!("stream work kind is fixed");
                    };
                    chunks.clear();
                    *resume = None;
                    break 'chunks;
                }
                continue;
            }
            current_block = Some(block);
            state.process(
                &record,
                header,
                AlignmentSourcePosition::bam_virtual_offset(end_position),
                &mut builder,
            )?;
        }
    }
    match current_block {
        Some(block) => {
            *slot = Some(state);
            Ok(PositionedAlignmentStreamCheckpoint::Block(
                builder.finish(block, interval),
            ))
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
    loop {
        if let Some(block) = state.ready.pop_front() {
            *slot = Some(state);
            return Ok(PositionedAlignmentStreamCheckpoint::Block(block));
        }
        let StreamWork::Cram { containers } = &mut state.work else {
            return Err(AlignmentError::NoActivePositionedStream);
        };
        let Some((container_offset, landmarks)) = containers.pop_front() else {
            return Ok(state.complete());
        };
        let interval = state.interval;
        let mut slices = BTreeMap::<u64, BlockBuilder>::new();
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
                let builder = slices.entry(slice_landmark).or_default();
                state.process(&record, header, position, builder)
            },
        )?;
        state.ready.extend(
            slices
                .into_values()
                .map(|builder| builder.finish(container_offset, interval)),
        );
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
