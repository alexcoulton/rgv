//! Differential suite: the same alignment bytes decoded by `rust-htslib` (local path), by
//! `noodles` over the local file (with and without BGZF worker threads) and by `noodles` over
//! the local HTTP fixture must normalise to identical records and derive identical statistics,
//! pileups, junctions, pairs and modification summaries.

use std::{
    fmt::Write as _,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use rgv_core::{ContigId, GenomicInterval, OverlappingMatePolicy};
use rust_htslib::bam::{
    Format, Header, Record, Writer,
    header::HeaderRecord,
    record::{Aux, Cigar, CigarString},
};

use super::BamFixture;
use crate::{
    AlignmentFilter, AlignmentOpenOptions, AlignmentQuery, AlignmentRecord, AlignmentSource,
    AuxValue, LocalAlignmentSource, LocalBamAlignmentSource, LocalCramAlignmentSource,
    PositionedAlignmentBlock, PositionedAlignmentStreamCheckpoint, ProductionAlignmentSource,
    QueryDetail, QueryOptions, QueryStats, RemoteBamAlignmentSource,
};

/// The one `H`-typed field of the fixture; see `assert_records_identical`.
const HEX_TAG: [u8; 2] = *b"Xi";
pub(super) const CHR1_LENGTH: u64 = 100_000;
const CHR2_LENGTH: u64 = 500;
const PAIRED: u16 = 0x1;
const PROPER: u16 = 0x2;
const UNMAPPED: u16 = 0x4;
const MATE_UNMAPPED: u16 = 0x8;
const REVERSE: u16 = 0x10;
const MATE_REVERSE: u16 = 0x20;
const FIRST: u16 = 0x40;
const LAST: u16 = 0x80;
const SECONDARY: u16 = 0x100;
const QC_FAILED: u16 = 0x200;
const DUPLICATE: u16 = 0x400;
const SUPPLEMENTARY: u16 = 0x800;

fn header() -> Header {
    let mut header = Header::new();
    header.push_record(
        HeaderRecord::new(b"HD")
            .push_tag(b"VN", "1.6")
            .push_tag(b"SO", "coordinate"),
    );
    for (name, length) in [("chr1", CHR1_LENGTH), ("chr2", CHR2_LENGTH)] {
        header.push_record(
            HeaderRecord::new(b"SQ")
                .push_tag(b"SN", name)
                .push_tag(b"LN", length),
        );
    }
    header.push_record(
        HeaderRecord::new(b"RG")
            .push_tag(b"ID", "group-a")
            .push_tag(b"PL", "ILLUMINA"),
    );
    header
}

fn query_length(cigar: &[Cigar]) -> usize {
    cigar
        .iter()
        .map(|operation| match operation {
            Cigar::Match(length)
            | Cigar::Ins(length)
            | Cigar::SoftClip(length)
            | Cigar::Equal(length)
            | Cigar::Diff(length) => *length as usize,
            Cigar::Del(_) | Cigar::RefSkip(_) | Cigar::HardClip(_) | Cigar::Pad(_) => 0,
        })
        .sum()
}

/// A mapped record with a deterministic sequence and per-base qualities cycling 2..=41, so a
/// base-quality threshold filters some bases of every read.
fn mapped(name: &str, tid: i32, position: i64, flags: u16, mapq: u8, cigar: Vec<Cigar>) -> Record {
    let length = query_length(&cigar);
    let seed = name.bytes().fold(7_usize, |seed, byte| {
        seed.wrapping_mul(31).wrapping_add(usize::from(byte))
    });
    let sequence = (0..length)
        .map(|index| b"ACGT"[(seed.wrapping_add(index.wrapping_mul(7)) >> 1) % 4])
        .collect::<Vec<_>>();
    let qualities = (0..length)
        .map(|index| 2 + ((seed + index) % 40) as u8)
        .collect::<Vec<_>>();
    let mut record = Record::new();
    record.set(
        name.as_bytes(),
        Some(&CigarString(cigar)),
        &sequence,
        &qualities,
    );
    record.set_tid(tid);
    record.set_pos(position);
    record.set_mapq(mapq);
    record.set_flags(flags);
    record.set_mtid(-1);
    record.set_mpos(-1);
    record.set_insert_size(0);
    record
}

fn with_mate(
    mut record: Record,
    mate_tid: i32,
    mate_position: i64,
    template_length: i64,
) -> Record {
    record.set_mtid(mate_tid);
    record.set_mpos(mate_position);
    record.set_insert_size(template_length);
    record
}

/// Records every backend must agree on. `cram_safe` leaves out what `HTSlib`'s CRAM writer
/// rewrites or cannot store unchanged.
#[allow(clippy::too_many_lines)]
pub(super) fn fixture_records(cram_safe: bool) -> Vec<Record> {
    let mut records = Vec::new();

    // Reads at the contig ends.
    records.push(mapped(
        "at-contig-start",
        0,
        0,
        0,
        60,
        vec![Cigar::Match(40)],
    ));
    records.push(mapped(
        "at-contig-end",
        0,
        i64::try_from(CHR1_LENGTH - 50).unwrap(),
        REVERSE,
        60,
        vec![Cigar::SoftClip(5), Cigar::Match(50)],
    ));
    records.push(mapped(
        "whole-small-contig",
        1,
        0,
        0,
        37,
        vec![Cigar::Equal(250), Cigar::Diff(1), Cigar::Equal(249)],
    ));

    if !cram_safe {
        // More than 65,535 CIGAR operations: the BAM writer stores the `kSmN` placeholder and
        // the real CIGAR in a `CG:B,I` field.
        let mut long_cigar = Vec::with_capacity(70_001);
        for _ in 0..35_000 {
            long_cigar.push(Cigar::Match(1));
            long_cigar.push(Cigar::Ins(1));
        }
        long_cigar.push(Cigar::Match(10));
        let mut long_read = mapped("long-cigar", 0, 1_000, 0, 60, long_cigar);
        long_read.push_aux(b"HP", Aux::U8(1)).unwrap();
        records.push(long_read);
    }

    // Overlapping mates, each with an indel inside the overlap.
    let mut first = with_mate(
        mapped(
            "overlap",
            0,
            5_000,
            PAIRED | PROPER | MATE_REVERSE | FIRST,
            60,
            vec![
                Cigar::Match(30),
                Cigar::Ins(2),
                Cigar::Match(10),
                Cigar::Del(3),
                Cigar::Match(18),
            ],
        ),
        0,
        5_020,
        75,
    );
    first.push_aux(b"HP", Aux::U8(1)).unwrap();
    records.push(first);
    let mut last = with_mate(
        mapped(
            "overlap",
            0,
            5_020,
            PAIRED | PROPER | REVERSE | LAST,
            60,
            vec![
                Cigar::Match(12),
                Cigar::Del(2),
                Cigar::Match(8),
                Cigar::Ins(3),
                Cigar::Match(33),
            ],
        ),
        0,
        5_000,
        -75,
    );
    last.push_aux(b"HP", Aux::U8(2)).unwrap();
    records.push(last);
    // Mates starting at the same position.
    for (flags, template_length) in [
        (PAIRED | PROPER | MATE_REVERSE | FIRST, 40_i64),
        (PAIRED | PROPER | REVERSE | LAST, -40),
    ] {
        records.push(with_mate(
            mapped("same-start", 0, 5_200, flags, 50, vec![Cigar::Match(40)]),
            0,
            5_200,
            template_length,
        ));
    }

    // Secondary and supplementary alignments, duplicates and QC failures.
    records.push(mapped(
        "secondary",
        0,
        6_000,
        SECONDARY,
        0,
        vec![Cigar::Match(30)],
    ));
    let mut no_sequence = Record::new();
    no_sequence.set(
        b"secondary-without-sequence",
        Some(&CigarString(vec![Cigar::Match(30)])),
        b"",
        b"",
    );
    no_sequence.set_tid(0);
    no_sequence.set_pos(6_010);
    no_sequence.set_mapq(0);
    no_sequence.set_flags(SECONDARY | REVERSE);
    no_sequence.set_mtid(-1);
    no_sequence.set_mpos(-1);
    records.push(no_sequence);
    records.push(mapped(
        "duplicate",
        0,
        6_050,
        DUPLICATE,
        60,
        vec![Cigar::Match(30)],
    ));
    records.push(mapped(
        "qc-failed",
        0,
        6_060,
        QC_FAILED | REVERSE,
        20,
        vec![Cigar::Match(30)],
    ));

    // A hard-clipped supplementary alignment carrying base modifications with `MN`.
    let mut supplementary = mapped(
        "supplementary",
        0,
        6_100,
        SUPPLEMENTARY,
        60,
        vec![Cigar::HardClip(7), Cigar::Match(20), Cigar::HardClip(13)],
    );
    supplementary
        .push_aux(b"MM", Aux::String("C+m,0,1;"))
        .unwrap();
    supplementary
        .push_aux(b"ML", Aux::ArrayU8((&[200_u8, 10][..]).into()))
        .unwrap();
    supplementary.push_aux(b"MN", Aux::I32(40)).unwrap();
    supplementary
        .push_aux(b"SA", Aux::String("chr1,6501,+,7S20M13S,60,0;"))
        .unwrap();
    records.push(supplementary);

    // Base modifications: called, empty groups, and an empty `ML` array.
    let mut modified = mapped("modified", 0, 6_200, 0, 60, vec![Cigar::Match(60)]);
    modified
        .push_aux(b"MM", Aux::String("C+m?,0,2;A+a.,1;"))
        .unwrap();
    modified
        .push_aux(b"ML", Aux::ArrayU8((&[255_u8, 128, 3][..]).into()))
        .unwrap();
    modified.push_aux(b"MN", Aux::I32(60)).unwrap();
    modified.push_aux(b"HP", Aux::U8(2)).unwrap();
    records.push(modified);
    let mut empty_groups = mapped(
        "empty-mm-groups",
        0,
        6_220,
        REVERSE,
        60,
        vec![Cigar::Match(60)],
    );
    empty_groups
        .push_aux(b"MM", Aux::String("C+m?;C+h;"))
        .unwrap();
    empty_groups
        .push_aux(b"ML", Aux::ArrayU8((&[][..]).into()))
        .unwrap();
    records.push(empty_groups);

    // A mapped read whose mate is unmapped, and that mate placed at its partner.
    records.push(with_mate(
        mapped(
            "unmapped-mate",
            0,
            7_000,
            PAIRED | MATE_UNMAPPED | FIRST,
            60,
            vec![Cigar::Match(50)],
        ),
        0,
        7_000,
        0,
    ));
    let mut placed_unmapped = Record::new();
    placed_unmapped.set(b"unmapped-mate", None, b"ACGTACGTAC", &[30; 10]);
    placed_unmapped.set_tid(0);
    placed_unmapped.set_pos(7_000);
    placed_unmapped.set_mapq(0);
    placed_unmapped.set_flags(PAIRED | UNMAPPED | LAST);
    placed_unmapped.set_mtid(0);
    placed_unmapped.set_mpos(7_000);
    records.push(placed_unmapped);
    // A mate on another contig.
    records.push(with_mate(
        mapped(
            "other-contig-mate",
            0,
            7_100,
            PAIRED | MATE_REVERSE | LAST,
            255,
            vec![Cigar::Match(50)],
        ),
        1,
        100,
        0,
    ));

    // A spliced read with strand evidence, and one of every auxiliary value type.
    let mut spliced = mapped(
        "spliced",
        0,
        8_000,
        0,
        60,
        vec![Cigar::Match(25), Cigar::RefSkip(400), Cigar::Match(25)],
    );
    spliced.push_aux(b"XS", Aux::Char(b'-')).unwrap();
    records.push(spliced);
    let mut typed = mapped("typed-aux", 0, 8_500, 0, 60, vec![Cigar::Match(30)]);
    typed.push_aux(b"RG", Aux::String("group-a")).unwrap();
    typed.push_aux(b"Xa", Aux::Char(b'q')).unwrap();
    typed.push_aux(b"Xb", Aux::I8(-5)).unwrap();
    typed.push_aux(b"Xc", Aux::U8(200)).unwrap();
    typed.push_aux(b"Xd", Aux::I16(-3_000)).unwrap();
    typed.push_aux(b"Xe", Aux::U16(60_000)).unwrap();
    typed.push_aux(b"Xf", Aux::I32(-70_000)).unwrap();
    typed.push_aux(b"Xg", Aux::U32(4_000_000_000)).unwrap();
    typed.push_aux(b"Xh", Aux::Float(1.5)).unwrap();
    typed
        .push_aux(&HEX_TAG, Aux::HexByteArray("1AE301"))
        .unwrap();
    if !cram_safe {
        typed
            .push_aux(b"Xj", Aux::ArrayI8((&[-1_i8, 2][..]).into()))
            .unwrap();
        typed
            .push_aux(b"Xk", Aux::ArrayI16((&[-300_i16, 300][..]).into()))
            .unwrap();
        typed
            .push_aux(b"Xl", Aux::ArrayU16((&[1_u16, 65_535][..]).into()))
            .unwrap();
        typed
            .push_aux(b"Xm", Aux::ArrayI32((&[-70_000_i32, 70_000][..]).into()))
            .unwrap();
        typed
            .push_aux(b"Xn", Aux::ArrayU32((&[1_u32, 4_000_000_000][..]).into()))
            .unwrap();
        typed
            .push_aux(b"Xo", Aux::ArrayFloat((&[0.25_f32, -2.0][..]).into()))
            .unwrap();
    }
    records.push(typed);
    // No stored base qualities.
    let mut no_qualities = Record::new();
    no_qualities.set(
        b"no-qualities",
        Some(&CigarString(vec![Cigar::Match(20)])),
        b"ACGTACGTACGTACGTACGT",
        &[0xff; 20],
    );
    no_qualities.set_tid(0);
    no_qualities.set_pos(8_600);
    no_qualities.set_mapq(13);
    no_qualities.set_flags(0);
    no_qualities.set_mtid(-1);
    no_qualities.set_mpos(-1);
    records.push(no_qualities);

    records.sort_by_key(|record| (record.tid(), record.pos()));
    records
}

pub(super) fn write_alignment(
    path: &Path,
    format: Format,
    reference: Option<&Path>,
    records: &[Record],
) {
    let mut writer = Writer::from_path(path, &header(), format).unwrap();
    if let Some(reference) = reference {
        writer.set_reference(reference).unwrap();
    }
    for record in records {
        writer.write(record).unwrap();
    }
}

pub(super) fn write_bam_fixture(directory: &Path) -> (PathBuf, PathBuf) {
    let path = directory.join("differential.bam");
    write_alignment(&path, Format::Bam, None, &fixture_records(false));
    let index = directory.join("differential.bam.bai");
    rust_htslib::bam::index::build(&path, Some(&index), rust_htslib::bam::index::Type::Bai, 1)
        .unwrap();
    (path, index)
}

pub(super) fn interval(contig: u32, start: u64, end: u64) -> GenomicInterval {
    GenomicInterval::new(ContigId::new(contig), start, end).unwrap()
}

pub(super) fn intervals() -> Vec<GenomicInterval> {
    vec![
        interval(0, 0, CHR1_LENGTH),
        interval(0, 0, 1),
        interval(0, 39, 40),
        interval(0, 40, 41),
        interval(0, 999, 1_000),
        interval(0, 1_000, 1_001),
        interval(0, 20_000, 20_010),
        interval(0, 36_009, 36_010),
        interval(0, 36_010, 36_011),
        interval(0, 4_990, 5_300),
        interval(0, 5_020, 5_030),
        interval(0, 5_900, 6_400),
        interval(0, 6_999, 7_000),
        interval(0, 7_000, 7_001),
        interval(0, 7_001, 7_002),
        interval(0, 6_990, 7_200),
        interval(0, 8_000, 8_700),
        interval(0, 8_100, 8_200),
        interval(0, CHR1_LENGTH - 1, CHR1_LENGTH),
        interval(1, 0, CHR2_LENGTH),
        interval(1, CHR2_LENGTH - 1, CHR2_LENGTH),
    ]
}

pub(super) fn option_variants() -> Vec<(&'static str, QueryOptions)> {
    let everything = QueryOptions {
        detail: QueryDetail::Full,
        collect_alleles: true,
        collect_modifications: true,
        include_secondary: true,
        include_supplementary: true,
        ..QueryOptions::default()
    };
    vec![
        ("layout defaults", QueryOptions::default()),
        ("everything", everything.clone()),
        (
            "count both mates",
            QueryOptions {
                overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
                ..everything.clone()
            },
        ),
        (
            "base quality threshold",
            QueryOptions {
                minimum_base_quality: 20,
                ..everything.clone()
            },
        ),
        (
            "sequence detail with retained tags",
            QueryOptions {
                detail: QueryDetail::Sequence,
                retained_auxiliary_tags: vec![*b"HP", *b"CG", *b"ML"],
                include_supplementary: false,
                minimum_junction_flank: 5,
                ..QueryOptions::default()
            },
        ),
        (
            "user filter",
            QueryOptions {
                filter: AlignmentFilter {
                    minimum_mapping_quality: Some(30),
                    required_flags: 0,
                    excluded_flags: DUPLICATE,
                    auxiliary: Vec::new(),
                },
                ..everything.clone()
            },
        ),
        (
            "auxiliary filters",
            QueryOptions {
                filter: AlignmentFilter {
                    auxiliary: vec!["HP>=1".parse().unwrap()],
                    ..AlignmentFilter::default()
                },
                ..everything.clone()
            },
        ),
        (
            "CG tag filter",
            QueryOptions {
                filter: AlignmentFilter {
                    auxiliary: vec!["CG".parse().unwrap()],
                    ..AlignmentFilter::default()
                },
                ..everything.clone()
            },
        ),
        (
            "too dense",
            QueryOptions {
                max_records: NonZeroUsize::new(2).unwrap(),
                ..everything
            },
        ),
    ]
}

/// Field-wise record comparison, so a failure names the field instead of printing a 70,000
/// operation CIGAR twice.
pub(super) fn assert_records_identical(
    expected: &AlignmentRecord,
    actual: &AlignmentRecord,
    context: &str,
) {
    let name = String::from_utf8_lossy(&expected.read_name).into_owned();
    assert_eq!(expected.read_name, actual.read_name, "read name: {context}");
    assert_eq!(
        (expected.contig, expected.start, expected.end),
        (actual.contig, actual.start, actual.end),
        "placement of {name}: {context}"
    );
    assert_eq!(expected.flags, actual.flags, "flags of {name}: {context}");
    assert_eq!(
        expected.mapping_quality, actual.mapping_quality,
        "mapping quality of {name}: {context}"
    );
    assert_eq!(
        (
            expected.mate_contig,
            expected.mate_start,
            expected.template_length
        ),
        (
            actual.mate_contig,
            actual.mate_start,
            actual.template_length
        ),
        "mate fields of {name}: {context}"
    );
    assert!(expected.cigar == actual.cigar, "CIGAR of {name}: {context}");
    assert!(
        expected.sequence == actual.sequence,
        "sequence or qualities of {name}: {context}"
    );
    let tags = |record: &AlignmentRecord| {
        record
            .auxiliary_fields
            .iter()
            .map(|field| String::from_utf8_lossy(&field.tag).into_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        tags(expected),
        tags(actual),
        "auxiliary tags of {name}: {context}"
    );
    for (expected, actual) in expected
        .auxiliary_fields
        .iter()
        .zip(&actual.auxiliary_fields)
    {
        if expected.tag == HEX_TAG && expected.value != actual.value {
            // Known and unfixable without `unsafe`: rust-htslib 1.0.1 decodes an `H` field as
            // `Aux::String`, so the HTSlib path cannot tell `H` from `Z`. noodles reports the
            // stored type. This pins the divergence so it is noticed when it goes away.
            assert_eq!(
                expected.value,
                AuxValue::String("1AE301".to_owned()),
                "{context}"
            );
            assert_eq!(
                actual.value,
                AuxValue::HexByteArray("1AE301".to_owned()),
                "{context}"
            );
            continue;
        }
        assert_eq!(expected, actual, "auxiliary field of {name}: {context}");
    }
}

pub(super) fn assert_queries_identical(
    expected: &AlignmentQuery,
    actual: &AlignmentQuery,
    context: &str,
) {
    assert_eq!(expected.stats, actual.stats, "statistics: {context}");
    assert_eq!(expected.header, actual.header, "header metadata: {context}");
    assert_eq!(
        expected.batch.len(),
        actual.batch.len(),
        "record count: {context}"
    );
    for index in 0..expected.batch.len() {
        assert_records_identical(
            &expected.batch.materialize_record(index).unwrap(),
            &actual.batch.materialize_record(index).unwrap(),
            context,
        );
        assert_eq!(
            expected.batch.source_position(index),
            actual.batch.source_position(index),
            "source position of record {index}: {context}"
        );
    }
    assert_eq!(expected.pairs, actual.pairs, "pair summary: {context}");
    assert_eq!(
        expected.junctions, actual.junctions,
        "junction summary: {context}"
    );
    assert_eq!(
        expected.pileup.modifications, actual.pileup.modifications,
        "modification summary: {context}"
    );
    assert_eq!(
        expected.pileup.provenance, actual.pileup.provenance,
        "pileup provenance: {context}"
    );
    assert_eq!(expected.pileup, actual.pileup, "pileup: {context}");
    // Whatever the field-wise comparison above did not name (filtered-record spans).
    if !has_hex_field(expected) || has_hex_field_of_one_type(expected, actual) {
        assert!(expected == actual, "whole query: {context}");
    }
}

fn has_hex_field(query: &AlignmentQuery) -> bool {
    (0..query.batch.len()).any(|index| {
        query
            .batch
            .materialize_record(index)
            .unwrap()
            .auxiliary_fields
            .iter()
            .any(|field| field.tag == HEX_TAG)
    })
}

/// Both queries came from decoders that report the `H` field with the same type.
fn has_hex_field_of_one_type(expected: &AlignmentQuery, actual: &AlignmentQuery) -> bool {
    let hex = |query: &AlignmentQuery| {
        (0..query.batch.len())
            .flat_map(|index| {
                query
                    .batch
                    .materialize_record(index)
                    .unwrap()
                    .auxiliary_fields
            })
            .find(|field| field.tag == HEX_TAG)
            .map(|field| field.value)
    };
    hex(expected) == hex(actual)
}

fn drain_stream(
    source: &mut ProductionAlignmentSource,
    target: GenomicInterval,
    admission: GenomicInterval,
    options: &QueryOptions,
) -> (Vec<PositionedAlignmentBlock>, QueryStats) {
    source
        .begin_positioned_stream_with_admission(target, admission, options.clone())
        .unwrap();
    let mut blocks = Vec::new();
    loop {
        match source.next_positioned_stream_checkpoint(&|| true).unwrap() {
            PositionedAlignmentStreamCheckpoint::Block(block) => blocks.push(block),
            PositionedAlignmentStreamCheckpoint::Complete(completion) => {
                assert_eq!(completion.interval, target);
                return (blocks, completion.stats);
            }
            PositionedAlignmentStreamCheckpoint::Cancelled(_) => {
                panic!("an uncancelled stream reported cancellation")
            }
        }
    }
}

/// The `noodles` local-file BAM front end with `threads` BGZF workers; none means the plain
/// single-threaded BGZF reader. The lease comes from an allowance of the test's own, so the
/// thread count does not depend on what other tests hold.
pub(super) fn open_local_noodles(path: &Path, threads: u32) -> ProductionAlignmentSource {
    static ALLOWANCE: crate::local::DecompressionThreadAllowance =
        crate::local::DecompressionThreadAllowance::new();
    let source = LocalBamAlignmentSource::open_with_lease(
        path,
        &AlignmentOpenOptions::default(),
        ALLOWANCE.lease(threads, u32::MAX),
    )
    .unwrap();
    assert_eq!(source.decompression_threads(), threads);
    ProductionAlignmentSource::LocalBam(Box::new(source))
}

fn assert_backends_identical(
    local: &mut dyn AlignmentSource,
    remote: &mut dyn AlignmentSource,
    intervals: &[GenomicInterval],
    variants: usize,
) {
    for (name, options) in option_variants().into_iter().take(variants) {
        for interval in intervals {
            let context = format!("{name}, {interval:?}");
            let expected = local.query(*interval, options.clone(), &|| true).unwrap();
            let actual = remote.query(*interval, options.clone(), &|| true).unwrap();
            assert_queries_identical(&expected, &actual, &context);
        }
    }
}

fn assert_streams_identical(
    local: &mut ProductionAlignmentSource,
    remote: &mut ProductionAlignmentSource,
    intervals: &[GenomicInterval],
    variants: usize,
) {
    for (name, options) in option_variants().into_iter().take(variants) {
        for target in intervals {
            // Admission over the whole target, and over its first half only.
            let half = GenomicInterval::new(
                target.contig(),
                target.start(),
                target.start() + (target.end() - target.start()).div_ceil(2),
            )
            .unwrap();
            for admission in [*target, half] {
                let context = format!("{name}, {target:?}, admission {admission:?}");
                let (expected_blocks, expected_stats) =
                    drain_stream(local, *target, admission, &options);
                let (actual_blocks, actual_stats) =
                    drain_stream(remote, *target, admission, &options);
                assert_eq!(expected_stats, actual_stats, "stream statistics: {context}");
                assert_eq!(
                    expected_blocks.len(),
                    actual_blocks.len(),
                    "owner block count: {context}"
                );
                for (expected, actual) in expected_blocks.iter().zip(&actual_blocks) {
                    assert_eq!(
                        (expected.compressed_offset, expected.genomic_span),
                        (actual.compressed_offset, actual.genomic_span),
                        "owner block identity: {context}"
                    );
                    assert_eq!(expected.records.len(), actual.records.len(), "{context}");
                    for (expected, actual) in expected.records.iter().zip(&actual.records) {
                        assert_records_identical(&expected.record, &actual.record, &context);
                        assert_eq!(
                            expected.source_position, actual.source_position,
                            "source position: {context}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn htslib_and_noodles_agree_on_every_edge_case_record() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index) = write_bam_fixture(directory.path());
    let Some(fixture) = BamFixture::start(fs::read(&path).unwrap(), fs::read(&index).unwrap())
    else {
        return;
    };
    let mut local = LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    let mut remote =
        RemoteBamAlignmentSource::open(&fixture.bam_url, &AlignmentOpenOptions::default()).unwrap();
    assert_eq!(local.reference_dictionary(), remote.reference_dictionary());
    assert_eq!(
        local.normalization_mapped_records(),
        remote.normalization_mapped_records
    );

    // The fixture really holds the cases it is named for.
    let everything = &option_variants()[1].1;
    let whole = local
        .query(interval(0, 0, CHR1_LENGTH), everything.clone(), &|| true)
        .unwrap();
    let long = (0..whole.batch.len())
        .map(|index| whole.batch.materialize_record(index).unwrap())
        .find(|record| record.read_name == b"long-cigar")
        .unwrap();
    assert_eq!(long.cigar.len(), 70_001);
    assert_eq!(long.end, 1_000 + 35_010);
    assert!(
        long.auxiliary_fields
            .iter()
            .all(|field| field.tag != *b"CG")
    );
    assert!(whole.pileup.provenance.collapsed_mate_bases > 0);
    assert!(!whole.pileup.modifications.series.is_empty());
    assert_eq!(whole.stats.records_filtered_unmapped, 1);

    assert_backends_identical(&mut local, &mut remote, &intervals(), usize::MAX);

    // The noodles decoder over the local file, with and without BGZF worker threads.
    let production = ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default());
    let production = production.unwrap();
    for threads in [0, 1, 4] {
        let mut local_noodles = open_local_noodles(&path, threads);
        assert_eq!(
            local.reference_dictionary(),
            local_noodles.reference_dictionary()
        );
        assert_eq!(
            local.normalization_mapped_records(),
            local_noodles.normalization_mapped_records()
        );
        assert_eq!(production.source_stamp(), local_noodles.source_stamp());
        assert_eq!(production.index_overview(), local_noodles.index_overview());
        assert!(local_noodles.index_overview().is_some());
        assert_eq!(
            production.header_metadata(),
            local_noodles.header_metadata()
        );
        assert_backends_identical(&mut local, &mut local_noodles, &intervals(), usize::MAX);
        assert_whole_queries_identical(&mut local_noodles, &mut remote, &intervals());
    }
}

/// Two front ends of the one noodles decoder: nothing may differ, the `H` field included.
fn assert_whole_queries_identical(
    left: &mut dyn AlignmentSource,
    right: &mut dyn AlignmentSource,
    intervals: &[GenomicInterval],
) {
    for (name, options) in option_variants() {
        for interval in intervals {
            let expected = left.query(*interval, options.clone(), &|| true).unwrap();
            let actual = right.query(*interval, options.clone(), &|| true).unwrap();
            assert!(
                expected == actual,
                "noodles front ends: {name}, {interval:?}"
            );
        }
    }
}

/// The open option routes a local `.bam` to the noodles front end, and only when asked.
#[test]
fn the_local_bam_backend_is_htslib_unless_noodles_is_requested() {
    let directory = tempfile::tempdir().unwrap();
    let (path, _) = write_bam_fixture(directory.path());
    assert_eq!(
        AlignmentOpenOptions::default().local_bam_backend,
        crate::AlignmentBackendPreference::Htslib
    );
    let default = ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    assert!(matches!(default, ProductionAlignmentSource::Htslib(_)));
    let options = AlignmentOpenOptions {
        local_bam_backend: crate::AlignmentBackendPreference::Noodles,
        ..AlignmentOpenOptions::default()
    };
    let mut noodles = ProductionAlignmentSource::open(&path, &options).unwrap();
    assert!(matches!(noodles, ProductionAlignmentSource::LocalBam(_)));
    assert!(!noodles.is_remote() && !noodles.is_cram());
    assert!(noodles.remote_io_stats().is_none());
    let mut htslib = default;
    assert_backends_identical(&mut htslib, &mut noodles, &intervals(), usize::MAX);

    // An explicit index path, BAI or CSI, is honoured.
    let csi = directory.path().join("elsewhere.csi");
    rust_htslib::bam::index::build(&path, Some(&csi), rust_htslib::bam::index::Type::Csi(14), 1)
        .unwrap();
    let mut with_csi = ProductionAlignmentSource::open(
        &path,
        &AlignmentOpenOptions {
            index_path: Some(csi),
            ..options.clone()
        },
    )
    .unwrap();
    assert_backends_identical(&mut htslib, &mut with_csi, &intervals(), 2);

    // Without an index the open fails; it does not scan the file.
    let unindexed = directory.path().join("unindexed.bam");
    fs::copy(&path, &unindexed).unwrap();
    assert!(matches!(
        ProductionAlignmentSource::open(&unindexed, &options),
        Err(crate::AlignmentError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound
    ));
}

/// A file cut short inside a BGZF block is an error for either BGZF reader, never a silently
/// shorter result (the multithreaded reader alone would report the end of the stream).
#[test]
fn a_truncated_local_bam_is_an_error_not_a_short_result() {
    let Some(corpus) = corpus_path("mixed.bam") else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("truncated.bam");
    let bytes = fs::read(&corpus).unwrap();
    fs::write(&path, &bytes[..bytes.len() / 2 + 1_234]).unwrap();
    fs::copy(
        format!("{}.bai", corpus.display()),
        directory.path().join("truncated.bam.bai"),
    )
    .unwrap();
    let whole = |source: &mut ProductionAlignmentSource| {
        let contig = source.reference_dictionary().iter().next().unwrap();
        let target = interval(0, 0, contig.length());
        source.query(target, QueryOptions::default(), &|| true)
    };
    for threads in [0, 4] {
        let mut source = open_local_noodles(&path, threads);
        assert!(whole(&mut source).is_err(), "{threads} threads");
        let contig_length = source
            .reference_dictionary()
            .iter()
            .next()
            .unwrap()
            .length();
        let target = interval(0, 0, contig_length);
        source
            .begin_positioned_stream_with_admission(target, target, QueryOptions::default())
            .unwrap();
        let failed = loop {
            match source.next_positioned_stream_checkpoint(&|| true) {
                Ok(PositionedAlignmentStreamCheckpoint::Block(_)) => {}
                Ok(_) => break false,
                Err(_) => break true,
            }
        };
        assert!(failed, "{threads} threads: the stream completed");
    }
}

fn corpus_path(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../benchmark-data")
        .join(name);
    path.exists().then_some(path)
}

#[test]
fn htslib_and_noodles_positioned_streams_agree_on_every_edge_case_record() {
    let directory = tempfile::tempdir().unwrap();
    let (path, index) = write_bam_fixture(directory.path());
    let Some(fixture) = BamFixture::start(fs::read(&path).unwrap(), fs::read(&index).unwrap())
    else {
        return;
    };
    let mut local =
        ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    let mut remote = ProductionAlignmentSource::open(
        Path::new(&fixture.bam_url),
        &AlignmentOpenOptions::default(),
    )
    .unwrap();
    assert!(matches!(local, ProductionAlignmentSource::Htslib(_)));
    assert!(matches!(remote, ProductionAlignmentSource::RemoteBam(_)));
    assert_streams_identical(&mut local, &mut remote, &intervals(), usize::MAX);
    for threads in [0, 4] {
        let mut local_noodles = open_local_noodles(&path, threads);
        assert_streams_identical(&mut local, &mut local_noodles, &intervals(), usize::MAX);
        assert_streams_identical(&mut local_noodles, &mut remote, &intervals(), usize::MAX);
    }
}

/// The corpora run the first variants only (layout defaults and everything), which is where
/// the decoders are exercised hardest; the full matrix runs on the edge-case fixture.
const CORPUS_VARIANTS: usize = 2;

/// The repository corpora: real multi-block BAMs with many BGZF owner blocks.
#[test]
fn htslib_and_noodles_agree_on_the_benchmark_corpora() {
    for (name, intervals) in [
        (
            "mixed.bam",
            vec![
                interval(0, 0, 50_000),
                interval(0, 900_000, 920_000),
                interval(0, 1_000_000, 1_000_001),
                interval(1, 1_400_000, 1_500_000),
            ],
        ),
        (
            "variants.bam",
            vec![interval(0, 0, 20_000), interval(0, 9_999, 10_001)],
        ),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data")
            .join(name);
        if !path.exists() {
            continue;
        }
        let index = fs::read(format!("{}.bai", path.display())).unwrap();
        let Some(fixture) = BamFixture::start(fs::read(&path).unwrap(), index) else {
            return;
        };
        let mut local =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let mut remote =
            RemoteBamAlignmentSource::open(&fixture.bam_url, &AlignmentOpenOptions::default())
                .unwrap();
        let intervals = intervals
            .into_iter()
            .filter(|interval| {
                local
                    .reference_dictionary()
                    .get(interval.contig())
                    .is_some_and(|contig| contig.length() >= interval.end())
            })
            .collect::<Vec<_>>();
        assert!(!intervals.is_empty(), "{name} has no queryable interval");
        assert_backends_identical(&mut local, &mut remote, &intervals, CORPUS_VARIANTS);
        let mut local =
            ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let mut remote = ProductionAlignmentSource::open(
            Path::new(&fixture.bam_url),
            &AlignmentOpenOptions::default(),
        )
        .unwrap();
        assert_streams_identical(&mut local, &mut remote, &intervals, CORPUS_VARIANTS);
        for threads in [0, 4] {
            let mut local_noodles = open_local_noodles(&path, threads);
            assert_backends_identical(&mut local, &mut local_noodles, &intervals, CORPUS_VARIANTS);
            assert_streams_identical(&mut local, &mut local_noodles, &intervals, CORPUS_VARIANTS);
            // A one-shot query between two checkpoints moves the reader; the stream must
            // not depend on where it was left.
            assert_stream_survives_interleaved_queries(&mut local_noodles, &intervals);
        }
    }
}

/// Writes the CRAM-safe fixture records as an indexed CRAM with its indexed reference.
pub(super) fn write_cram_fixture(directory: &Path) -> (PathBuf, PathBuf) {
    let reference = directory.join("reference.fa");
    let mut fasta = Vec::new();
    let mut fai = String::new();
    for (name, length) in [("chr1", CHR1_LENGTH), ("chr2", CHR2_LENGTH)] {
        fasta.extend_from_slice(format!(">{name}\n").as_bytes());
        writeln!(
            fai,
            "{name}\t{length}\t{}\t{length}\t{}",
            fasta.len(),
            length + 1
        )
        .unwrap();
        fasta.extend((0..length).map(|index| b"ACGT"[(index % 4) as usize]));
        fasta.push(b'\n');
    }
    fs::write(&reference, fasta).unwrap();
    fs::write(directory.join("reference.fa.fai"), fai).unwrap();
    let path = directory.join("differential.cram");
    write_alignment(
        &path,
        Format::Cram,
        Some(&reference),
        &fixture_records(true),
    );
    rust_htslib::bam::index::build(&path, None, rust_htslib::bam::index::Type::Bai, 1).unwrap();
    assert!(directory.join("differential.cram.crai").is_file());
    (path, reference)
}

/// The same records written as CRAM: `HTSlib`'s CRAM decoder against the noodles one.
#[test]
fn htslib_and_noodles_agree_on_cram() {
    let directory = tempfile::tempdir().unwrap();
    let (path, reference) = write_cram_fixture(directory.path());

    let options = AlignmentOpenOptions {
        reference_path: Some(reference),
        ..AlignmentOpenOptions::default()
    };
    let mut oracle = LocalAlignmentSource::open(&path, &options).unwrap();
    let mut decoded = LocalCramAlignmentSource::open(&path, &options).unwrap();
    assert_eq!(
        oracle.reference_dictionary(),
        decoded.reference_dictionary()
    );
    for (name, query_options) in option_variants() {
        for interval in intervals() {
            let context = format!("CRAM, {name}, {interval:?}");
            let expected = oracle
                .query(interval, query_options.clone(), &|| true)
                .unwrap();
            let actual = decoded
                .query(interval, query_options.clone(), &|| true)
                .unwrap();
            assert_eq!(expected.stats, actual.stats, "statistics: {context}");
            assert_eq!(expected.batch.len(), actual.batch.len(), "{context}");
            for index in 0..expected.batch.len() {
                assert_records_identical(
                    &expected.batch.materialize_record(index).unwrap(),
                    &actual.batch.materialize_record(index).unwrap(),
                    &context,
                );
            }
            assert_eq!(expected.pairs, actual.pairs, "pair summary: {context}");
            assert_eq!(expected.junctions, actual.junctions, "junctions: {context}");
            assert_eq!(expected.pileup, actual.pileup, "pileup: {context}");
        }
    }
}

/// Runs a positioned stream with a one-shot query of another region between every two
/// checkpoints, and requires the blocks of an undisturbed stream.
fn assert_stream_survives_interleaved_queries(
    source: &mut ProductionAlignmentSource,
    intervals: &[GenomicInterval],
) {
    let options = QueryOptions::default();
    let target = intervals[0];
    let elsewhere = *intervals.last().unwrap();
    let (expected, expected_stats) = drain_stream(source, target, target, &options);
    source
        .begin_positioned_stream_with_admission(target, target, options.clone())
        .unwrap();
    let mut blocks = Vec::new();
    let stats = loop {
        match source.next_positioned_stream_checkpoint(&|| true).unwrap() {
            PositionedAlignmentStreamCheckpoint::Block(block) => blocks.push(block),
            PositionedAlignmentStreamCheckpoint::Complete(completion) => break completion.stats,
            PositionedAlignmentStreamCheckpoint::Cancelled(_) => panic!("cancelled"),
        }
        source.query(elsewhere, options.clone(), &|| true).unwrap();
    };
    assert_eq!(expected_stats, stats);
    assert!(expected == blocks, "blocks of an interleaved stream");
}
