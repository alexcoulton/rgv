use std::borrow::Cow;

use rgv_core::{ContigId, OverlappingMatePolicy, ReferenceDictionary};
use rgv_model::pileup::MAX_PENDING_PAIR_OVERLAPS;

use super::*;
use crate::{
    AlignmentFilter, AlignmentFlags, AuxField, AuxValue, CigarOpKind, CoverageTileId,
    backend_record::BackendRecord,
};

/// A record that has only what coverage may read. Touching its sequence, qualities or
/// auxiliary payload fails the test: the accumulator must not decode or copy them.
struct PayloadFree<'a> {
    name: &'a [u8],
    flags: u16,
    start: u64,
    mapping_quality: u8,
    mate_start: Option<u64>,
    cigar: &'a [CigarOp],
}

impl BackendRecord for PayloadFree<'_> {
    fn flags(&self) -> Result<AlignmentFlags, AlignmentError> {
        Ok(AlignmentFlags::from_bits(self.flags))
    }

    fn contig(&self) -> Result<Option<ContigId>, AlignmentError> {
        Ok(Some(ContigId::new(0)))
    }

    fn start(&self) -> Result<Option<u64>, AlignmentError> {
        Ok(Some(self.start))
    }

    fn mapping_quality(&self) -> Result<Option<u8>, AlignmentError> {
        Ok(Some(self.mapping_quality))
    }

    fn mate_contig(&self) -> Result<Option<ContigId>, AlignmentError> {
        Ok(self.mate_start.map(|_| ContigId::new(0)))
    }

    fn mate_start(&self) -> Result<Option<u64>, AlignmentError> {
        Ok(self.mate_start)
    }

    fn template_length(&self) -> Result<i64, AlignmentError> {
        panic!("coverage must not read the template length")
    }

    fn read_name(&self) -> &[u8] {
        self.name
    }

    fn cigar_into(&self, cigar: &mut Vec<CigarOp>) -> Result<(), AlignmentError> {
        cigar.clear();
        cigar.extend_from_slice(self.cigar);
        Ok(())
    }

    fn sequence_len(&self) -> usize {
        panic!("coverage must not read the sequence")
    }

    fn bases(&self) -> Vec<u8> {
        panic!("coverage must not decode bases")
    }

    fn qualities(&self) -> Result<Cow<'_, [u8]>, AlignmentError> {
        panic!("coverage must not decode qualities")
    }

    fn auxiliary(&self, _tag: [u8; 2]) -> Result<Option<AuxValue>, AlignmentError> {
        panic!("coverage must not read auxiliary fields the filter does not name")
    }

    fn auxiliary_fields(
        &self,
        _retain: &dyn Fn([u8; 2]) -> bool,
    ) -> Result<Vec<AuxField>, AlignmentError> {
        panic!("coverage must not copy auxiliary fields")
    }
}

const CONTIG_LENGTH: u64 = 65_536;

fn dictionary() -> ReferenceDictionary {
    ReferenceDictionary::from_contigs([("deep".to_owned(), CONTIG_LENGTH)])
}

fn whole_contig(eligibility: CoverageEligibility) -> CoverageTileRequest {
    let parameters = CoveragePyramidParameters::DEFAULT;
    CoverageTileRequest {
        parameters,
        contig: ContigId::new(0),
        level: 0,
        tiles: parameters.tile_range(0, 0, CONTIG_LENGTH, CONTIG_LENGTH),
        eligibility,
    }
}

/// Streams overlapping pairs at `depth` over the whole contig, generating each record on the
/// stack and dropping it at once: the fixture itself retains nothing.
fn stream_at_depth(depth: u64) -> (CoverageTileProduction, u64) {
    const READ: u64 = 150;
    const STEP: u64 = 50;
    const MATE_OFFSET: u64 = 100;
    let request = whole_contig(CoverageEligibility::default());
    let mut stream = CoverageStream::new(&request, &dictionary()).unwrap();
    let cigar = [CigarOp {
        kind: CigarOpKind::Match,
        len: u32::try_from(READ).unwrap(),
    }];
    // Each start position carries first mates (mate 100 bases on) and the second mates of
    // the templates that began 100 bases back. Every base is under three first mates and
    // three second mates, of which the overlapping third is collapsed.
    let copies = depth / 5;
    let mut name = Vec::with_capacity(32);
    let mut records = 0_u64;
    for start in (0..CONTIG_LENGTH - READ).step_by(usize::try_from(STEP).unwrap()) {
        for copy in 0..copies {
            if start >= MATE_OFFSET {
                name.clear();
                name.extend_from_slice(b"t");
                name.extend_from_slice((start - MATE_OFFSET).to_string().as_bytes());
                name.extend_from_slice(b"/");
                name.extend_from_slice(copy.to_string().as_bytes());
                let second = PayloadFree {
                    name: &name,
                    flags: 0x1 | 0x10 | 0x80,
                    start,
                    mapping_quality: 60,
                    mate_start: Some(start - MATE_OFFSET),
                    cigar: &cigar,
                };
                assert!(stream.add(&second, &|| true).unwrap());
                records += 1;
            }
            name.clear();
            name.extend_from_slice(b"t");
            name.extend_from_slice(start.to_string().as_bytes());
            name.extend_from_slice(b"/");
            name.extend_from_slice(copy.to_string().as_bytes());
            let first = PayloadFree {
                name: &name,
                flags: 0x1 | 0x20 | 0x40,
                start,
                mapping_quality: 60,
                mate_start: Some(start + MATE_OFFSET),
                cigar: &cigar,
            };
            assert!(stream.add(&first, &|| true).unwrap());
            records += 1;
        }
    }
    let CoverageTileOutcome::Complete(production) = stream.finish().unwrap() else {
        panic!("not cancelled");
    };
    (production, records)
}

#[test]
fn ten_thousand_fold_depth_needs_the_same_working_memory_as_thirty_fold() {
    let (shallow, shallow_records) = stream_at_depth(30);
    let (deep, deep_records) = stream_at_depth(10_000);
    assert_eq!(deep.records_seen, deep_records);
    assert_eq!(deep.records_eligible, deep_records);
    assert!(deep_records > 300 * shallow_records);
    assert!(deep_records > 5_000_000);

    // It really is ultra-deep, and exact: away from the contig ends every base is under three
    // first mates and three second mates per copy, the 50-base mate overlap counted once.
    let interior = |production: &CoverageTileProduction| {
        let tile = &production.tiles[1];
        assert_eq!(tile.id(), CoverageTileId::new(ContigId::new(0), 0, 1));
        tile.aggregate(tile.span().0, tile.span().1).unwrap().0
    };
    let deep_interior = interior(&deep);
    assert_eq!(deep_interior.minimum_depth, 10_000);
    assert_eq!(deep_interior.maximum_depth, 10_000);
    assert_eq!(interior(&shallow).maximum_depth, 30);
    assert!(deep.overlap_collapse_complete);
    assert!(deep.collapsed_mate_bases > 0);

    // The depth state (bins, pending depth boundaries, CIGAR scratch) is byte-for-byte the
    // same size at 30x and at 10,000x.
    assert_eq!(deep.telemetry.bins, shallow.telemetry.bins);
    assert_eq!(
        deep.telemetry.peak_pending_boundaries,
        shallow.telemetry.peak_pending_boundaries
    );
    assert_eq!(
        deep.telemetry.peak_depth_state_bytes(),
        shallow.telemetry.peak_depth_state_bytes()
    );
    assert!(deep.telemetry.peak_pending_boundaries <= 8);
    assert_eq!(deep.telemetry.bins, 4_096);
    assert!(deep.telemetry.peak_depth_state_bytes() < 80 * 1024);

    // The count-once window is the only state that follows depth. It holds the templates whose
    // second mate is still to come (two start positions' worth), far below its hard cap, and
    // nowhere near the five million records streamed.
    assert_eq!(deep.telemetry.peak_pending_mates, 2 * 2_000);
    assert!(deep.telemetry.peak_pending_mates < MAX_PENDING_PAIR_OVERLAPS);
}

#[test]
fn eligibility_is_the_shared_rule_of_the_read_level_paths() {
    let cigar = [CigarOp {
        kind: CigarOpKind::Match,
        len: 100,
    }];
    let record = |flags, mapping_quality, start| PayloadFree {
        name: b"read",
        flags,
        start,
        mapping_quality,
        mate_start: None,
        cigar: &cigar,
    };
    let records = [
        record(0, 60, 1_000),
        record(0x400, 60, 1_000),
        record(0x200, 60, 1_000),
        record(0x100, 60, 1_000),
        record(0x800, 60, 1_000),
        record(0x4, 60, 1_000),
        record(0, 5, 1_000),
    ];
    let depth_with = |eligibility: CoverageEligibility| {
        let mut stream = CoverageStream::new(&whole_contig(eligibility), &dictionary()).unwrap();
        for record in &records {
            stream.add(record, &|| true).unwrap();
        }
        let CoverageTileOutcome::Complete(production) = stream.finish().unwrap() else {
            panic!("not cancelled");
        };
        assert_eq!(production.records_seen, 7);
        (
            production.tiles[0].maximum_depth(),
            production.records_eligible,
        )
    };
    let defaults = CoverageEligibility::default();
    // Defaults: secondary hidden, supplementary shown, no flag or quality filter.
    assert_eq!(depth_with(defaults.clone()), (5, 5));
    assert_eq!(
        depth_with(CoverageEligibility {
            include_secondary: true,
            ..defaults.clone()
        }),
        (6, 6)
    );
    assert_eq!(
        depth_with(CoverageEligibility {
            include_supplementary: false,
            ..defaults.clone()
        }),
        (4, 4)
    );
    assert_eq!(
        depth_with(CoverageEligibility {
            filter: AlignmentFilter {
                excluded_flags: 0x400 | 0x200,
                ..AlignmentFilter::default()
            },
            ..defaults.clone()
        }),
        (3, 3)
    );
    assert_eq!(
        depth_with(CoverageEligibility {
            filter: AlignmentFilter {
                minimum_mapping_quality: Some(20),
                excluded_flags: 0x400 | 0x200,
                ..AlignmentFilter::default()
            },
            overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
            ..defaults
        }),
        (2, 2)
    );
}

#[test]
fn an_unsorted_stream_is_an_error_not_a_wrong_answer() {
    let cigar = [CigarOp {
        kind: CigarOpKind::Match,
        len: 100,
    }];
    let record = |start| PayloadFree {
        name: b"read",
        flags: 0,
        start,
        mapping_quality: 60,
        mate_start: None,
        cigar: &cigar,
    };
    let mut stream =
        CoverageStream::new(&whole_contig(CoverageEligibility::default()), &dictionary()).unwrap();
    stream.add(&record(2_000), &|| true).unwrap();
    assert!(matches!(
        stream.add(&record(1_999), &|| true),
        Err(AlignmentError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidData
    ));
    let unknown = CoverageTileRequest {
        contig: ContigId::new(7),
        ..whole_contig(CoverageEligibility::default())
    };
    assert!(matches!(
        CoverageStream::new(&unknown, &dictionary()),
        Err(AlignmentError::UnknownContig(7))
    ));
}

/// One amplicon at `depth` templates: every first mate covers `[1024, 1184)` and every second
/// mate `[1120, 1280)` (bin-aligned, 64 bases shared), so all first mates wait for theirs at
/// the same time. Returns the outcome and
/// the telemetry of every pass made.
fn amplicon_at_depth(depth: u64) -> (CoverageTileProduction, Vec<CoverageAccumulatorTelemetry>) {
    const READ: u32 = 160;
    let request = whole_contig(CoverageEligibility::default());
    let cigar = [CigarOp {
        kind: CigarOpKind::Match,
        len: READ,
    }];
    let mut passes = Vec::new();
    let outcome = with_mate_overlap_fallback(&request, |request| {
        let mut stream = CoverageStream::new(request, &dictionary())?;
        let mut name = Vec::with_capacity(16);
        for (flags, start, mate_start) in [
            (0x1 | 0x20 | 0x40, 1_024, 1_120),
            (0x1 | 0x10 | 0x80, 1_120, 1_024),
        ] {
            for template in 0..depth {
                name.clear();
                name.extend_from_slice(b"t");
                name.extend_from_slice(template.to_string().as_bytes());
                let record = PayloadFree {
                    name: &name,
                    flags,
                    start,
                    mapping_quality: 60,
                    mate_start: Some(mate_start),
                    cigar: &cigar,
                };
                assert!(stream.add(&record, &|| true)?);
            }
        }
        let outcome = stream.finish()?;
        if let CoverageTileOutcome::Complete(production) = &outcome {
            passes.push(production.telemetry);
        }
        Ok(outcome)
    })
    .unwrap();
    let CoverageTileOutcome::Complete(production) = outcome else {
        panic!("not cancelled");
    };
    (production, passes)
}

fn amplicon_depths(production: &CoverageTileProduction) -> [u32; 4] {
    let tile = &production.tiles[0];
    [(896, 1_024), (1_024, 1_120), (1_120, 1_184), (1_184, 1_280)].map(|(start, end)| {
        let (bin, _) = tile.aggregate(start, end).unwrap();
        assert_eq!(bin.minimum_depth, bin.maximum_depth);
        bin.maximum_depth
    })
}

#[test]
fn a_two_hundred_thousand_fold_amplicon_gets_coverage_flagged_as_not_collapsed() {
    // Below the window every overlap is collapsed, exactly, and nothing is flagged.
    let (shallow, passes) = amplicon_at_depth(50_000);
    assert!(shallow.overlap_collapse_complete && !shallow.mate_overlap_not_collapsed);
    assert_eq!(amplicon_depths(&shallow), [0, 50_000, 50_000, 50_000]);
    assert_eq!(shallow.collapsed_mate_bases, 50_000 * 64);
    assert_eq!(passes.len(), 1);

    // At 200,000 templates twice as many mates are pending as the window holds. Coverage is
    // still produced: every mate counted independently, and flagged as exactly that.
    let (deep, passes) = amplicon_at_depth(200_000);
    assert_eq!(deep.tiles.len(), shallow.tiles.len());
    assert!(deep.mate_overlap_not_collapsed);
    assert!(!deep.overlap_collapse_complete);
    assert_eq!(deep.records_seen, 400_000);
    assert_eq!(deep.records_eligible, 400_000);
    // Both mates cover the 64 shared bases: 400,000 there, 200,000 on either side.
    assert_eq!(amplicon_depths(&deep), [0, 200_000, 400_000, 200_000]);
    assert_eq!(deep.collapsed_mate_bases, 0);

    // In bounded memory: the first pass never held more mates than the window, the second
    // held none, and the depth state of both is as small as at any other depth.
    assert_eq!(passes.len(), 2);
    assert_eq!(passes[0].peak_pending_mates, MAX_PENDING_PAIR_OVERLAPS);
    assert_eq!(passes[1].peak_pending_mates, 0);
    assert_eq!(deep.telemetry, passes[1]);
    for pass in &passes {
        assert_eq!(pass.bins, shallow.telemetry.bins);
        assert!(pass.peak_pending_boundaries <= 8);
        assert!(pass.peak_depth_state_bytes() < 80 * 1024);
    }

    // A request that already counts both mates has nothing to degrade to and is not flagged.
    let both = whole_contig(CoverageEligibility {
        overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
        ..CoverageEligibility::default()
    });
    assert_eq!(
        both,
        whole_contig(CoverageEligibility::default()).counting_mates_independently()
    );
    let mut calls = 0;
    let outcome = with_mate_overlap_fallback(&both, |request| {
        calls += 1;
        CoverageStream::new(request, &dictionary())?.finish()
    })
    .unwrap();
    let CoverageTileOutcome::Complete(production) = outcome else {
        panic!("not cancelled");
    };
    assert_eq!(calls, 1);
    assert!(!production.mate_overlap_not_collapsed);

    // A cancelled second pass is a cancelled request, not a half-answer.
    let mut calls = 0;
    let outcome =
        with_mate_overlap_fallback(&whole_contig(CoverageEligibility::default()), |request| {
            calls += 1;
            if calls == 2 {
                return Ok(CoverageTileOutcome::Cancelled);
            }
            let mut production = match CoverageStream::new(request, &dictionary())?.finish()? {
                CoverageTileOutcome::Complete(production) => production,
                CoverageTileOutcome::Cancelled => unreachable!(),
            };
            production.overlap_collapse_complete = false;
            Ok(CoverageTileOutcome::Complete(production))
        })
        .unwrap();
    assert_eq!((calls, outcome), (2, CoverageTileOutcome::Cancelled));
}
