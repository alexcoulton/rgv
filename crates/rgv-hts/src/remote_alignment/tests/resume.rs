//! Resumed positioned streams: a stream told which chunks the caller already holds returns
//! exactly what a fresh stream would, minus those chunks, with identical accounting, and does
//! not fetch them.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use noodles::{
    core::Position,
    cram, fasta,
    sam::{
        self,
        alignment::{
            RecordBuf,
            io::Write as _,
            record::{
                Flags, MappingQuality,
                cigar::{Op, op::Kind as CigarKind},
            },
            record_buf::{QualityScores, Sequence},
        },
        header::record::value::{Map, map::ReferenceSequence},
    },
};
use rgv_core::GenomicInterval;

use super::{BamFixture, differential::interval, write_dense_bam};
use crate::{
    AlignmentOpenOptions, PositionedAlignmentBlock, PositionedAlignmentStreamCheckpoint,
    PositionedChunkReceipt, ProductionAlignmentSource, QueryDetail, QueryOptions, QueryStats,
};

type Chunk = (PositionedAlignmentBlock, Option<PositionedChunkReceipt>);

fn drain(
    source: &mut ProductionAlignmentSource,
    target: GenomicInterval,
    admission: GenomicInterval,
    options: &QueryOptions,
    skip: Vec<PositionedChunkReceipt>,
) -> (Vec<Chunk>, QueryStats, usize) {
    let honoured = source
        .begin_positioned_stream_resuming(target, admission, options.clone(), skip)
        .unwrap();
    let mut chunks = Vec::new();
    loop {
        match source.next_positioned_stream_checkpoint(&|| true).unwrap() {
            PositionedAlignmentStreamCheckpoint::Block(block) => {
                let receipt = source.take_positioned_chunk_receipt();
                assert!(source.take_positioned_chunk_receipt().is_none());
                if let Some(receipt) = &receipt {
                    assert_eq!(receipt.compressed_offset(), block.compressed_offset);
                    assert_eq!(receipt.decoded_interval(), target);
                }
                chunks.push((block, receipt));
            }
            PositionedAlignmentStreamCheckpoint::Complete(completion) => {
                assert!(source.take_positioned_chunk_receipt().is_none());
                return (chunks, completion.stats, honoured);
            }
            PositionedAlignmentStreamCheckpoint::Cancelled(_) => panic!("stream was cancelled"),
        }
    }
}

fn chunk_key(chunk: &Chunk) -> (u64, Option<u64>) {
    (
        chunk.0.compressed_offset,
        chunk
            .1
            .as_ref()
            .and_then(PositionedChunkReceipt::cram_slice_landmark),
    )
}

/// Runs `resumed` against `held` (the chunks of an earlier stream) and checks it against a
/// fresh stream of the same target. Returns the keys of the chunks that were skipped.
fn assert_resume_matches_fresh(
    source: &mut ProductionAlignmentSource,
    held: &[Chunk],
    target: GenomicInterval,
    admission: GenomicInterval,
    options: &QueryOptions,
    context: &str,
) -> BTreeSet<(u64, Option<u64>)> {
    resume_matching_fresh(source, held, target, admission, options, context).0
}

/// [`assert_resume_matches_fresh`], also returning the resumed stream's chunks.
fn resume_matching_fresh(
    source: &mut ProductionAlignmentSource,
    held: &[Chunk],
    target: GenomicInterval,
    admission: GenomicInterval,
    options: &QueryOptions,
    context: &str,
) -> (BTreeSet<(u64, Option<u64>)>, Vec<Chunk>) {
    let (fresh, fresh_stats, _) = drain(source, target, admission, options, Vec::new());
    let reusable = held
        .iter()
        .filter(|chunk| {
            chunk
                .1
                .as_ref()
                .is_some_and(|receipt| receipt.can_resume(target, options))
        })
        .collect::<Vec<_>>();
    let skip = reusable
        .iter()
        .filter_map(|chunk| chunk.1.clone())
        .collect::<Vec<_>>();
    let skipped_keys = reusable
        .iter()
        .map(|chunk| chunk_key(chunk))
        .collect::<BTreeSet<_>>();
    let (resumed, resumed_stats, honoured) = drain(source, target, admission, options, skip);
    assert_each_record_once(&resumed, context);
    assert_eq!(honoured, skipped_keys.len(), "receipts honoured: {context}");
    assert_eq!(
        resumed_stats, fresh_stats,
        "admission accounting: {context}"
    );

    // The resumed stream is the fresh stream without the skipped chunks, in the same order.
    let expected = fresh
        .iter()
        .filter(|chunk| !skipped_keys.contains(&chunk_key(chunk)))
        .map(|chunk| &chunk.0)
        .collect::<Vec<_>>();
    let actual = resumed.iter().map(|chunk| &chunk.0).collect::<Vec<_>>();
    assert_eq!(actual.len(), expected.len(), "blocks returned: {context}");
    for (actual, expected) in actual.iter().zip(&expected) {
        assert!(
            actual == expected,
            "block {}: {context}",
            expected.compressed_offset
        );
    }
    assert!(
        resumed
            .iter()
            .all(|chunk| !skipped_keys.contains(&chunk_key(chunk))),
        "a skipped chunk was decoded again: {context}"
    );

    // What the caller holds for a skipped chunk, cut to the new target, is what the fresh
    // stream decoded for it: skipping lost nothing and kept nothing extra. This holds while
    // the fresh stream retained records (it stops retaining past the Complete-read limit).
    if fresh_stats.complete_read_records_eligible <= options.max_records.get() {
        for held_chunk in &reusable {
            let kept = held_chunk
                .0
                .records
                .iter()
                .filter(|positioned| {
                    positioned.record.start < target.end() && positioned.record.end > target.start()
                })
                .collect::<Vec<_>>();
            let decoded = fresh
                .iter()
                .find(|chunk| chunk_key(chunk) == chunk_key(held_chunk))
                .map(|chunk| chunk.0.records.iter().collect::<Vec<_>>())
                .unwrap_or_default();
            assert!(
                kept == decoded,
                "held records of chunk {:?}: {context}",
                chunk_key(held_chunk)
            );
        }
    }
    (skipped_keys, resumed)
}

fn options() -> QueryOptions {
    QueryOptions {
        detail: QueryDetail::Full,
        ..QueryOptions::default()
    }
}

/// Pans, zooms and a too-dense target against chunks held from `[1.0 Mb, 2.0 Mb)`.
fn assert_bam_resume_scenarios(source: &mut ProductionAlignmentSource) {
    let held_target = interval(0, 1_000_000, 2_000_000);
    let (held, _, _) = drain(source, held_target, held_target, &options(), Vec::new());
    assert!(held.len() > 20, "the fixture must span many BGZF blocks");
    for (name, target, minimum_skipped) in [
        ("identical", held_target, held.len()),
        ("zoom in", interval(0, 1_200_000, 1_600_000), 5),
        ("pan right", interval(0, 1_500_000, 2_500_000), 5),
        ("pan left", interval(0, 500_000, 1_500_000), 5),
        ("zoom out", interval(0, 800_000, 2_200_000), held.len() - 2),
        ("disjoint", interval(0, 3_000_000, 3_200_000), 0),
    ] {
        let skipped = assert_resume_matches_fresh(source, &held, target, target, &options(), name);
        assert!(
            skipped.len() >= minimum_skipped,
            "{name}: only {} of {} held chunks were reusable",
            skipped.len(),
            held.len()
        );
    }
    // Admission over part of the target, and a Complete-read limit the target exceeds: the
    // refusal and every count are those of a fresh stream.
    let target = interval(0, 1_500_000, 2_500_000);
    let admission = interval(0, 1_900_000, 2_100_000);
    assert_resume_matches_fresh(
        source,
        &held,
        target,
        admission,
        &options(),
        "partial admission",
    );
    let dense = QueryOptions {
        max_records: NonZeroUsize::new(3_000).unwrap(),
        ..options()
    };
    assert_resume_matches_fresh(source, &held, target, target, &dense, "too dense");
    let dense_admission = QueryOptions {
        max_records: NonZeroUsize::new(1_500).unwrap(),
        ..options()
    };
    assert_resume_matches_fresh(
        source,
        &held,
        target,
        admission,
        &dense_admission,
        "too dense inside the admission interval",
    );

    // Chunks decoded under other eligibility rules, or refused by the limit, are not reusable.
    let other_rules = QueryOptions {
        include_supplementary: false,
        ..options()
    };
    assert!(
        held.iter()
            .filter_map(|chunk| chunk.1.as_ref())
            .all(|receipt| !receipt.can_resume(held_target, &other_rules))
    );
    let (refused, refused_stats, _) = drain(source, held_target, held_target, &dense, Vec::new());
    assert!(refused_stats.truncated);
    assert!(
        refused
            .iter()
            .filter_map(|chunk| chunk.1.as_ref())
            .any(|receipt| !receipt.can_resume(held_target, &dense))
    );
    // Receipts chained from a resumed stream are reusable in turn.
    let pan = interval(0, 1_500_000, 2_500_000);
    let skip = held
        .iter()
        .filter_map(|chunk| chunk.1.clone())
        .filter(|receipt| receipt.can_resume(pan, &options()))
        .collect::<Vec<_>>();
    let (second, _, _) = drain(source, pan, pan, &options(), skip);
    let mut both = held.clone();
    both.extend(second);
    let skipped = assert_resume_matches_fresh(
        source,
        &both,
        interval(0, 1_100_000, 2_400_000),
        interval(0, 1_100_000, 2_400_000),
        &options(),
        "receipts of two streams",
    );
    assert!(skipped.len() > held.len());
}

fn dense_bam(directory: &Path) -> (PathBuf, PathBuf) {
    let path = directory.join("dense.bam");
    write_dense_bam(&path, 40_000);
    let index = PathBuf::from(format!("{}.bai", path.display()));
    (path, index)
}

#[test]
fn a_resumed_local_bam_stream_equals_a_fresh_stream_minus_the_skipped_chunks() {
    let directory = tempfile::tempdir().unwrap();
    let (path, _) = dense_bam(directory.path());
    let mut source =
        ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    assert!(matches!(source, ProductionAlignmentSource::Htslib(_)));
    assert_bam_resume_scenarios(&mut source);
}

#[test]
fn a_resumed_local_noodles_bam_stream_equals_a_fresh_stream_minus_the_skipped_chunks() {
    let directory = tempfile::tempdir().unwrap();
    let (path, _) = dense_bam(directory.path());
    for threads in [0, 3] {
        let mut source = super::differential::open_local_noodles(&path, threads);
        assert_bam_resume_scenarios(&mut source);
    }
}

#[test]
fn a_resumed_remote_bam_stream_equals_a_fresh_stream_minus_the_skipped_chunks() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index) = dense_bam(directory.path());
    let Some(fixture) = BamFixture::start(fs::read(&path).unwrap(), fs::read(&index).unwrap())
    else {
        return;
    };
    let mut source = ProductionAlignmentSource::open(
        Path::new(&fixture.bam_url),
        &AlignmentOpenOptions::default(),
    )
    .unwrap();
    assert!(matches!(source, ProductionAlignmentSource::RemoteBam(_)));
    assert_bam_resume_scenarios(&mut source);
}

/// The request log: a resumed remote stream does not ask the server for the skipped chunks.
#[test]
#[allow(clippy::too_many_lines)]
fn a_resumed_remote_bam_stream_does_not_fetch_the_skipped_blocks() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index) = dense_bam(directory.path());
    let bam = fs::read(&path).unwrap();
    let Some(fixture) = BamFixture::start(bam, fs::read(&index).unwrap()) else {
        return;
    };
    let open = || {
        ProductionAlignmentSource::open(
            Path::new(&fixture.bam_url),
            &AlignmentOpenOptions::default(),
        )
        .unwrap()
    };
    let held_target = interval(0, 1_000_000, 3_000_000);
    let (held, _, _) = drain(
        &mut open(),
        held_target,
        held_target,
        &options(),
        Vec::new(),
    );
    let owners = held
        .iter()
        .map(|chunk| chunk.0.compressed_offset)
        .collect::<Vec<_>>();

    // Pan right: the first half of the new target is held.
    let target = interval(0, 2_000_000, 4_000_000);
    let skip = held
        .iter()
        .filter_map(|chunk| chunk.1.clone())
        .filter(|receipt| receipt.can_resume(target, &options()))
        .collect::<Vec<_>>();
    let honourable = skip.len();

    // Fresh sources each time: nothing is cached from an earlier stream.
    let mut fresh_source = open();
    let before = fixture.bam_ranges_after(0).len();
    let (fresh, fresh_stats, _) = drain(&mut fresh_source, target, target, &options(), Vec::new());
    let fresh_ranges = fixture.bam_ranges_after(before);
    // The held chunks the new target actually touches.
    let fresh_owners = fresh
        .iter()
        .map(|chunk| chunk.0.compressed_offset)
        .collect::<BTreeSet<_>>();
    let skipped_owners = skip
        .iter()
        .map(PositionedChunkReceipt::compressed_offset)
        .filter(|owner| fresh_owners.contains(owner))
        .collect::<Vec<_>>();
    assert!(skipped_owners.len() > 20);
    let (first_skipped, last_skipped) = (
        *skipped_owners.iter().min().unwrap(),
        *skipped_owners.iter().max().unwrap(),
    );
    assert!(
        last_skipped - first_skipped > 1_000_000,
        "the skipped run spans megabytes"
    );
    let mut resumed_source = open();
    let before = fixture.bam_ranges_after(0).len();
    let (resumed, resumed_stats, honoured) =
        drain(&mut resumed_source, target, target, &options(), skip);
    let resumed_ranges = fixture.bam_ranges_after(before);
    assert_eq!(honoured, honourable);
    assert_eq!(resumed_stats, fresh_stats);
    assert_eq!(resumed.len() + skipped_owners.len(), fresh.len());

    // The fresh stream read the skipped blocks; the resumed stream requested no byte before
    // the block in which the last skipped chunk ends (that block also starts what follows).
    let overlaps_skipped = |ranges: &[(u64, u64)]| {
        ranges
            .iter()
            .filter(|(start, end)| *start < last_skipped && *end >= first_skipped)
            .count()
    };
    assert!(overlaps_skipped(&fresh_ranges) > 0);
    assert_eq!(
        overlaps_skipped(&resumed_ranges),
        0,
        "skipped blocks {first_skipped}..{last_skipped} were requested: {resumed_ranges:?}"
    );
    let bytes = |ranges: &[(u64, u64)]| {
        ranges
            .iter()
            .map(|(start, end)| end - start + 1)
            .sum::<u64>()
    };
    assert!(
        bytes(&fresh_ranges) - bytes(&resumed_ranges) >= last_skipped - first_skipped,
        "fresh {} bytes, resumed {} bytes",
        bytes(&fresh_ranges),
        bytes(&resumed_ranges)
    );

    // Zoom in: everything the target needs is held, so nothing at all is decoded and at
    // most the boundary block is requested.
    let inside = interval(0, 1_500_000, 2_500_000);
    let skip = held
        .iter()
        .filter_map(|chunk| chunk.1.clone())
        .filter(|receipt| receipt.can_resume(inside, &options()))
        .collect::<Vec<_>>();
    assert_eq!(skip.len(), owners.len());
    let mut source = open();
    let before = fixture.bam_ranges_after(0).len();
    let (blocks, _, _) = drain(&mut source, inside, inside, &options(), skip);
    assert!(blocks.is_empty());
    let ranges = fixture.bam_ranges_after(before);
    assert!(
        bytes(&ranges) <= 4 * 64 * 1024,
        "a fully held target requested {} bytes: {ranges:?}",
        bytes(&ranges)
    );
}

/// An indexed CRAM of `records` 50 bp reads, 7 bp apart, in several slices.
fn write_sliced_cram(directory: &Path, records: usize) -> (PathBuf, PathBuf) {
    const CONTIG_BASES: usize = 200_000;
    let bases = (0..CONTIG_BASES)
        .map(|index| b"ACGT"[index % 4])
        .collect::<Vec<_>>();
    let reference = directory.join("sliced.fa");
    let mut fasta_bytes = b">chr1\n".to_vec();
    fasta_bytes.extend_from_slice(&bases);
    fasta_bytes.push(b'\n');
    fs::write(&reference, fasta_bytes).unwrap();
    fs::write(
        directory.join("sliced.fa.fai"),
        format!(
            "chr1\t{CONTIG_BASES}\t6\t{CONTIG_BASES}\t{}\n",
            CONTIG_BASES + 1
        ),
    )
    .unwrap();
    let header = sam::Header::builder()
        .add_reference_sequence(
            "chr1",
            Map::<ReferenceSequence>::new(std::num::NonZero::new(CONTIG_BASES).unwrap()),
        )
        .build();
    let repository = fasta::Repository::new(vec![fasta::Record::new(
        fasta::record::Definition::new("chr1", None),
        fasta::record::Sequence::from(bases.clone()),
    )]);
    let path = directory.join("sliced.cram");
    let mut writer = cram::io::writer::Builder::default()
        .set_reference_sequence_repository(repository)
        .build_from_writer(File::create(&path).unwrap());
    writer.write_header(&header).unwrap();
    for index in 0..records {
        let start = 1 + index * 7;
        let record = RecordBuf::builder()
            .set_name(format!("read{index}"))
            .set_flags(Flags::empty())
            .set_reference_sequence_id(0)
            .set_alignment_start(Position::try_from(start).unwrap())
            .set_mapping_quality(MappingQuality::new(60).unwrap())
            .set_cigar([Op::new(CigarKind::Match, 50)].into_iter().collect())
            .set_sequence(Sequence::from(&bases[start - 1..start + 49]))
            .set_quality_scores(QualityScores::from(vec![30; 50]))
            .build();
        writer.write_alignment_record(&header, &record).unwrap();
    }
    writer.try_finish(&header).unwrap();
    drop(writer);
    let index = cram::fs::index(&path).unwrap();
    let mut index_writer =
        cram::crai::io::Writer::new(File::create(format!("{}.crai", path.display())).unwrap());
    index_writer.write_index(&index).unwrap();
    index_writer.finish().unwrap();
    (path, reference)
}

#[test]
fn a_resumed_cram_stream_equals_a_fresh_stream_minus_the_skipped_slices() {
    let directory = tempfile::tempdir().unwrap();
    let (path, reference) = write_sliced_cram(directory.path(), 25_000);
    let open_options = AlignmentOpenOptions {
        reference_path: Some(reference),
        ..AlignmentOpenOptions::default()
    };
    let mut source = ProductionAlignmentSource::open(&path, &open_options).unwrap();
    assert!(matches!(source, ProductionAlignmentSource::LocalCram(_)));
    let whole = interval(0, 0, 200_000);
    let (all, _, _) = drain(&mut source, whole, whole, &options(), Vec::new());
    assert!(
        all.len() >= 3,
        "the fixture must hold several slices, not {}",
        all.len()
    );

    let held_target = interval(0, 20_000, 150_000);
    let (held, _, _) = drain(
        &mut source,
        held_target,
        held_target,
        &options(),
        Vec::new(),
    );
    for (name, target, expect_all) in [
        ("identical", held_target, true),
        ("zoom in", interval(0, 60_000, 100_000), true),
        ("pan right", interval(0, 60_000, 175_000), false),
        ("zoom out", whole, false),
    ] {
        let skipped =
            assert_resume_matches_fresh(&mut source, &held, target, target, &options(), name);
        let (fresh, _, _) = drain(&mut source, target, target, &options(), Vec::new());
        let fresh_skipped = fresh
            .iter()
            .filter(|chunk| skipped.contains(&chunk_key(chunk)))
            .count();
        if expect_all {
            assert_eq!(fresh_skipped, fresh.len(), "{name}");
        } else {
            // Slices reaching beyond the held target are decoded again; interior ones are not.
            assert!(fresh_skipped > 0 && fresh_skipped < fresh.len(), "{name}");
        }
    }
    let dense = QueryOptions {
        max_records: NonZeroUsize::new(2_000).unwrap(),
        ..options()
    };
    let target = interval(0, 60_000, 175_000);
    assert_resume_matches_fresh(
        &mut source,
        &held,
        target,
        interval(0, 100_000, 160_000),
        &dense,
        "too dense",
    );
}

/// The benchmark corpus: long reads, a deep hotspot, spliced and supplementary records, so
/// chunk edges fall inside reads of very different lengths.
#[test]
fn resumed_streams_equal_fresh_streams_on_the_benchmark_corpus() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
    if !path.exists() {
        return;
    }
    let Some(fixture) = BamFixture::start(
        fs::read(&path).unwrap(),
        fs::read(format!("{}.bai", path.display())).unwrap(),
    ) else {
        return;
    };
    let mut local =
        ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    let mut remote = ProductionAlignmentSource::open(
        Path::new(&fixture.bam_url),
        &AlignmentOpenOptions::default(),
    )
    .unwrap();
    let everything = QueryOptions {
        include_secondary: true,
        max_records: NonZeroUsize::new(1_000_000).unwrap(),
        ..options()
    };
    let mut local_noodles = super::differential::open_local_noodles(&path, 4);
    for source in [&mut local, &mut remote, &mut local_noodles] {
        for (contig, held_target, targets) in [
            (
                0,
                (850_000, 950_000),
                vec![
                    (800_000, 910_000),
                    (905_000, 1_000_000),
                    (890_000, 930_000),
                    (700_000, 1_100_000),
                ],
            ),
            (
                1,
                (400_000, 1_200_000),
                vec![
                    (0, 800_000),
                    (800_000, 2_000_000),
                    (600_000, 1_000_000),
                    (100_000, 1_900_000),
                ],
            ),
        ] {
            let held_target = interval(contig, held_target.0, held_target.1);
            let (held, _, _) = drain(source, held_target, held_target, &everything, Vec::new());
            let mut reused = 0;
            for (start, end) in targets {
                let target = interval(contig, start, end);
                reused += assert_resume_matches_fresh(
                    source,
                    &held,
                    target,
                    target,
                    &everything,
                    &format!("{target:?} from {held_target:?}"),
                )
                .len();
            }
            assert!(reused > 0, "nothing was reusable on contig {contig}");
        }
    }
}

/// Bases of the long-read fixture's first contig that carry reads.
const LONG_READ_REGION: u64 = 200_000;

/// A nanopore-like run: a read every 200 bp spanning 1 to 30 kb, with CIGARs of hundreds to
/// about a thousand operations; one read in twenty carries its whole sequence (and qualities),
/// tens of kilobytes that span BGZF blocks, the others stretch over long deletions. Reads of
/// such different lengths fall into BAI bins of several levels, so the index chunks of any
/// window are many, interleaved, not adjacent, and hold several records each: the reader jumps
/// between them, and so does a resumed stream.
fn write_long_read_bam(path: &Path) {
    use rust_htslib::bam::{
        Format, Header, Record, Writer,
        header::HeaderRecord,
        record::{Cigar, CigarString},
    };

    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", "chr1")
            .push_tag(b"LN", 2_000_000),
    );
    let mut writer = Writer::from_path(path, &header, Format::Bam).unwrap();
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut random = move |bound: u32| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        u32::try_from(state >> 33).unwrap() % bound
    };
    for (index, position) in (0..LONG_READ_REGION).step_by(200).enumerate() {
        let span = 1_000 + random(29_000);
        let full_sequence = index % 20 == 7;
        let mut cigar = Vec::new();
        let mut covered = 0;
        while covered < span {
            let matched = 10 + random(50);
            cigar.push(Cigar::Match(matched));
            covered += matched;
            match random(if full_sequence { 3 } else { 4 }) {
                0 => cigar.push(Cigar::Ins(1 + random(4))),
                1 => {
                    let deleted = 1 + random(4);
                    cigar.push(Cigar::Del(deleted));
                    covered += deleted;
                }
                3 => {
                    let deleted = 100 + random(400);
                    cigar.push(Cigar::Del(deleted));
                    covered += deleted;
                }
                _ => {}
            }
        }
        cigar.push(Cigar::Match(10));
        let query_length = cigar
            .iter()
            .map(|op| match op {
                Cigar::Match(length) | Cigar::Ins(length) => *length as usize,
                _ => 0,
            })
            .sum::<usize>();
        let sequence = (0..query_length)
            .map(|_| b"ACGT"[random(4) as usize])
            .collect::<Vec<_>>();
        let qualities = (0..query_length)
            .map(|_| u8::try_from(random(41)).unwrap())
            .collect::<Vec<_>>();
        let mut record = Record::new();
        record.set(
            format!("read{index}").as_bytes(),
            Some(&CigarString(cigar)),
            &sequence,
            &qualities,
        );
        record.set_tid(0);
        record.set_pos(i64::try_from(position).unwrap());
        record.set_mapq(60);
        record.set_flags(0);
        writer.write(&record).unwrap();
    }
    drop(writer);
    rust_htslib::bam::index::build(
        path,
        Some(Path::new(&format!("{}.bai", path.display()))),
        rust_htslib::bam::index::Type::Bai,
        1,
    )
    .unwrap();
}

/// Every physical record a stream returned, once: no block repeats a source position, and no
/// two blocks of the stream share one.
fn assert_each_record_once(chunks: &[Chunk], context: &str) {
    let mut positions = BTreeSet::new();
    for (block, _) in chunks {
        for positioned in &block.records {
            assert!(
                positions.insert(positioned.source_position),
                "{:?} returned twice (block {}): {context}",
                positioned.source_position,
                block.compressed_offset
            );
        }
    }
}

/// A browsing session over the long-read fixture as the desktop drives it: pans and zooms
/// resumed from receipts of several earlier streams (all held chunks the target touches, only
/// the last stream's, or an arbitrary subset), with streams retargeted after a few blocks. Each
/// resumed stream must equal a fresh one minus the skipped chunks, with every record once.
fn assert_long_read_session(source: &mut ProductionAlignmentSource, backend: &str) {
    let options = QueryOptions {
        include_secondary: true,
        max_records: NonZeroUsize::new(1_000_000).unwrap(),
        ..options()
    };
    // The chunks the caller holds, by owner: the newest decode of each block wins.
    let mut held = std::collections::BTreeMap::<(u64, Option<u64>), Chunk>::new();
    let mut last = Vec::<Chunk>::new();
    let mut subset_state = 0x9e37_79b9_u64;
    let targets: [(u64, u64); 13] = [
        (120_000, 150_000),
        (130_000, 170_000),
        (100_000, 175_000),
        (120_000, 150_000),
        (130_000, 160_000),
        (110_000, 140_000),
        (60_000, 200_000),
        (115_000, 155_000),
        (80_000, 190_000),
        (40_000, 95_000),
        (60_000, 195_000),
        (140_000, 146_000),
        (0, LONG_READ_REGION),
    ];
    let mut skipped_total = 0;
    for (step, (start, end)) in targets.into_iter().enumerate() {
        let target = interval(0, start, end);
        let context = format!("{backend} step {step} {start}..{end}");
        let candidates = match step % 3 {
            // Everything held that touches the target, as the desktop offers it.
            0 => held
                .values()
                .filter(|(block, _)| {
                    block
                        .genomic_span
                        .is_some_and(|span| span.start() < end && span.end() > start)
                })
                .cloned()
                .collect::<Vec<_>>(),
            // Only the previous stream's chunks: the earlier streams' chunks were evicted.
            1 => last.clone(),
            // An arbitrary half of what is held.
            _ => held
                .values()
                .filter(|_| {
                    subset_state = subset_state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    subset_state >> 63 == 1
                })
                .cloned()
                .collect::<Vec<_>>(),
        };
        let (skipped, resumed) =
            resume_matching_fresh(source, &candidates, target, target, &options, &context);
        skipped_total += skipped.len();
        for chunk in &resumed {
            held.insert(chunk_key(chunk), chunk.clone());
        }
        last = resumed;

        // Retarget: a stream abandoned after a few blocks leaves usable receipts behind.
        if step % 4 == 1 {
            let abandoned = interval(0, start.saturating_sub(20_000), end.saturating_sub(5_000));
            let skip = held
                .values()
                .filter_map(|chunk| chunk.1.clone())
                .filter(|receipt| receipt.can_resume(abandoned, &options))
                .collect::<Vec<_>>();
            source
                .begin_positioned_stream_resuming(abandoned, abandoned, options.clone(), skip)
                .unwrap();
            let mut partial = Vec::new();
            for _ in 0..5 {
                match source.next_positioned_stream_checkpoint(&|| true).unwrap() {
                    PositionedAlignmentStreamCheckpoint::Block(block) => {
                        let receipt = source.take_positioned_chunk_receipt();
                        partial.push((block, receipt));
                    }
                    _ => break,
                }
            }
            assert_each_record_once(&partial, &format!("{context}, abandoned stream"));
            for chunk in partial {
                held.insert(chunk_key(&chunk), chunk);
            }
        }
    }
    assert!(
        skipped_total > 100,
        "{backend}: the session resumed too little ({skipped_total})"
    );
}

fn long_read_bam(directory: &Path) -> PathBuf {
    let path = directory.join("long-reads.bam");
    write_long_read_bam(&path);
    path
}

#[test]
fn resumed_local_bam_streams_return_each_long_read_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = long_read_bam(directory.path());
    let mut htslib =
        ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    assert!(matches!(htslib, ProductionAlignmentSource::Htslib(_)));
    assert_long_read_session(&mut htslib, "htslib");
    let mut noodles = super::differential::open_local_noodles(&path, 2);
    assert_long_read_session(&mut noodles, "noodles");
}

#[test]
fn resumed_remote_bam_streams_return_each_long_read_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = long_read_bam(directory.path());
    let index = PathBuf::from(format!("{}.bai", path.display()));
    let Some(fixture) = BamFixture::start(fs::read(&path).unwrap(), fs::read(&index).unwrap())
    else {
        return;
    };
    let mut source = ProductionAlignmentSource::open(
        Path::new(&fixture.bam_url),
        &AlignmentOpenOptions::default(),
    )
    .unwrap();
    assert!(matches!(source, ProductionAlignmentSource::RemoteBam(_)));
    assert_long_read_session(&mut source, "remote");
}
