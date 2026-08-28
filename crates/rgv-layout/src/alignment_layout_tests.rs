use std::sync::Arc;

use crate::{READ_DETAIL_OBJECT_BIT, RectShape, record_object_id};
use rgv_core::{ContigId, GenomicInterval};
use rgv_hts::{
    AlignmentBatch, AlignmentFlags, AlignmentHeaderMetadata, AlignmentReadGroup, AlignmentRecord,
    AlleleSupport, AuxField, AuxValue, BaseModificationStrand, CigarOp, ModificationProvenance,
    ModificationSeries, ModificationSeriesKey, QueryStats, ReferenceSlice, SequenceData,
    bisulfite_context_matches,
};

use super::technologies::bisulfite_color;
use super::*;

fn row_test_record(name: &[u8], start: u64, end: u64) -> AlignmentRecord {
    AlignmentRecord {
        contig: ContigId::new(0),
        start,
        end,
        flags: AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: name.to_vec(),
        cigar: vec![CigarOp {
            kind: CigarOpKind::Match,
            len: u32::try_from(end - start).unwrap(),
        }],
        sequence: SequenceData::NotLoaded,
        auxiliary_fields: Vec::new(),
    }
}

fn row_test_query(records: &[AlignmentRecord]) -> AlignmentQuery {
    let mut batch = AlignmentBatch::default();
    for record in records {
        batch.push(record).unwrap();
    }
    AlignmentQuery {
        interval: GenomicInterval::new(ContigId::new(0), 0, 200).unwrap(),
        header: Arc::default(),
        batch,
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats {
            records_seen: records.len(),
            records_eligible: records.len(),
            records_emitted: records.len(),
            ..QueryStats::default()
        },
    }
}

#[test]
fn too_dense_read_layout_emits_only_a_non_interactive_refusal_placeholder() {
    let query = row_test_query(&[
        row_test_record(b"one", 20, 60),
        row_test_record(b"two", 80, 120),
    ]);
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 0, 200).unwrap(),
        800.0,
    )
    .unwrap();
    let options = AlignmentLayoutOptions {
        show_reference: false,
        show_coverage: false,
        show_junctions: false,
        read_refusal: Some(IndividualReadRefusal {
            eligible_records: 50_001,
            limit: 50_000,
            scope: IndividualReadRefusalScope::Foreground,
        }),
        ..AlignmentLayoutOptions::default()
    };

    let layout = layout_alignment_query(&query, viewport, options);
    assert_eq!(layout.stats.rendered_alignments, 0);
    assert_eq!(layout.stats.hidden_alignments, 50_001);
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .all(|primitive| primitive.object_id == 0)
    );
    assert!(layout.display_list.texts.iter().any(|text| {
        text.text.contains("50001") && text.text.contains("complete-read limit is 50000")
    }));
    assert!(
        layout
            .display_list
            .texts
            .iter()
            .any(|text| { text.text.contains("Zoom in") && text.text.contains("raise the limit") })
    );
}

#[test]
fn bisulfite_contexts_match_both_reference_strands_exactly() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 20).unwrap();
    let reference = ReferenceSlice {
        interval,
        bases: b"ACGACAGACATAGCATAAAA".to_vec(),
    };

    assert!(bisulfite_context_matches(
        &reference,
        1,
        false,
        BisulfiteContext::Cg
    ));
    assert!(bisulfite_context_matches(
        &reference,
        4,
        false,
        BisulfiteContext::Chg
    ));
    assert!(bisulfite_context_matches(
        &reference,
        8,
        false,
        BisulfiteContext::Chh
    ));
    assert!(bisulfite_context_matches(
        &reference,
        1,
        false,
        BisulfiteContext::Hcg
    ));
    assert!(bisulfite_context_matches(
        &reference,
        13,
        false,
        BisulfiteContext::Gch
    ));
    assert!(bisulfite_context_matches(
        &reference,
        1,
        false,
        BisulfiteContext::Wcg
    ));
    // The G paired with the first forward-strand C is a reverse-strand CG cytosine.
    assert!(bisulfite_context_matches(
        &reference,
        2,
        true,
        BisulfiteContext::Cg
    ));
    assert!(!bisulfite_context_matches(
        &reference,
        4,
        false,
        BisulfiteContext::Cg
    ));
}

#[test]
fn bisulfite_observations_distinguish_protected_and_converted_on_both_strands() {
    let reference_bases = b"ACGTCGAA";
    let mut forward = row_test_record(b"forward", 0, reference_bases.len() as u64);
    let mut forward_bases = reference_bases.to_vec();
    forward_bases[4] = b'T';
    forward.sequence = SequenceData::Present {
        bases: forward_bases,
        qualities: None,
    };
    let mut reverse = row_test_record(b"reverse", 0, reference_bases.len() as u64);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    let mut reverse_display = reference_bases.to_vec();
    reverse_display[5] = b'A';
    reverse.sequence = SequenceData::Present {
        bases: reverse_display,
        qualities: None,
    };
    let mut query = row_test_query(&[forward, reverse]);
    query.reference = Some(ReferenceSlice {
        interval: GenomicInterval::new(ContigId::new(0), 0, reference_bases.len() as u64).unwrap(),
        bases: reference_bases.to_vec(),
    });

    assert_eq!(
        bisulfite_observation(&query, 0, BisulfiteContext::Cg, 1, b'C'),
        Some(BisulfiteObservation::Protected)
    );
    assert_eq!(
        bisulfite_observation(&query, 0, BisulfiteContext::Cg, 4, b'T'),
        Some(BisulfiteObservation::Converted)
    );
    assert_eq!(
        bisulfite_observation(&query, 1, BisulfiteContext::Cg, 2, b'G'),
        Some(BisulfiteObservation::Protected)
    );
    assert_eq!(
        bisulfite_observation(&query, 1, BisulfiteContext::Cg, 5, b'A'),
        Some(BisulfiteObservation::Converted)
    );
    assert_eq!(
        bisulfite_observation(&query, 0, BisulfiteContext::Cg, 3, b'T'),
        None
    );
}

#[test]
fn bisulfite_conversion_mismatch_is_specific_to_the_biological_strand() {
    let reference_bases = b"ACG";
    let forward = row_test_record(b"forward", 0, reference_bases.len() as u64);
    let mut reverse = row_test_record(b"reverse", 0, reference_bases.len() as u64);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    let mut query = row_test_query(&[forward, reverse]);
    query.reference = Some(ReferenceSlice {
        interval: GenomicInterval::new(ContigId::new(0), 0, reference_bases.len() as u64).unwrap(),
        bases: reference_bases.to_vec(),
    });

    assert!(bisulfite_conversion_mismatch(&query, 0, 1, b'T'));
    assert!(!bisulfite_conversion_mismatch(&query, 0, 1, b'A'));
    assert!(bisulfite_conversion_mismatch(&query, 1, 2, b'A'));
    assert!(!bisulfite_conversion_mismatch(&query, 1, 2, b'T'));
}

#[test]
fn bisulfite_render_budget_records_every_omitted_mark() {
    let mut budget = BisulfiteRenderBudget::new(1);
    assert!(budget.take());
    assert!(!budget.take());
    assert!(!budget.take());
    assert_eq!(budget.omitted, 2);
}

#[test]
fn bisulfite_marks_remain_visible_across_base_and_read_detail_levels() {
    let reference_bases = b"ACGTCGAA";
    let mut record = row_test_record(b"converted", 0, reference_bases.len() as u64);
    let mut observed = reference_bases.to_vec();
    observed[4] = b'T';
    record.sequence = SequenceData::Present {
        bases: observed,
        qualities: None,
    };
    let mut query = row_test_query(&[record]);
    query.reference = Some(ReferenceSlice {
        interval: GenomicInterval::new(ContigId::new(0), 0, reference_bases.len() as u64).unwrap(),
        bases: reference_bases.to_vec(),
    });
    let options = AlignmentLayoutOptions {
        reference_top: 0.0,
        coverage_top: 0.0,
        reads_top: 0.0,
        show_reference: false,
        show_coverage: false,
        show_junctions: false,
        show_modification_frequency: false,
        show_modified_bases: false,
        color_mode: AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
        read_base_display_mode: ReadBaseDisplayMode::Hidden,
        ..AlignmentLayoutOptions::default()
    };
    let rows = assign_alignment_rows(&query, options, None);
    for viewport in [
        Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(), 80.0).unwrap(),
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
            500.0,
        )
        .unwrap(),
    ] {
        let layout = layout_alignment_query_with_rows(&query, viewport, options, &rows);
        let protected = layout
            .display_list
            .rectangles
            .iter()
            .filter(|rectangle| {
                (rectangle.color.red - 0.86).abs() < 1.0e-6
                    && (rectangle.color.green - 0.12).abs() < 1.0e-6
            })
            .count();
        let converted = layout
            .display_list
            .rectangles
            .iter()
            .filter(|rectangle| {
                (rectangle.color.red - 0.12).abs() < 1.0e-6
                    && (rectangle.color.blue - 0.88).abs() < 1.0e-6
            })
            .count();
        assert_eq!((protected, converted), (1, 1));
        assert_eq!(layout.stats.bisulfite_marks_omitted, 0);
        assert!(layout.display_list.texts.is_empty());
    }
}

#[test]
fn bisulfite_mode_suppresses_ordinary_mismatches_outside_selected_context() {
    let reference_bases = b"ACGTCACG";
    let mut record = row_test_record(b"converted", 0, reference_bases.len() as u64);
    let mut observed = reference_bases.to_vec();
    observed[4] = b'T';
    observed[6] = b'T';
    observed[3] = b'A';
    record.sequence = SequenceData::Present {
        bases: observed,
        qualities: None,
    };
    let mut query = row_test_query(&[record]);
    query.reference = Some(ReferenceSlice {
        interval: GenomicInterval::new(ContigId::new(0), 0, reference_bases.len() as u64).unwrap(),
        bases: reference_bases.to_vec(),
    });
    let options = AlignmentLayoutOptions {
        reference_top: 0.0,
        coverage_top: 0.0,
        reads_top: 0.0,
        show_reference: false,
        show_coverage: false,
        show_junctions: false,
        show_modification_frequency: false,
        show_modified_bases: false,
        color_mode: AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
        read_base_display_mode: ReadBaseDisplayMode::Mismatches,
        ..AlignmentLayoutOptions::default()
    };
    let rows = assign_alignment_rows(&query, options, None);
    let viewport =
        Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(), 80.0).unwrap();
    let layout = layout_alignment_query_with_rows(&query, viewport, options, &rows);

    let protected = bisulfite_color(BisulfiteObservation::Protected);
    let converted = bisulfite_color(BisulfiteObservation::Converted);
    let ordinary_t = base_color(b'T');
    let ordinary_a = base_color(b'A');
    let read_detail_colors = layout
        .display_list
        .rectangles
        .iter()
        .filter(|rectangle| rectangle.object_id & READ_DETAIL_OBJECT_BIT != 0)
        .map(|rectangle| rectangle.color)
        .collect::<Vec<_>>();

    assert_eq!(
        read_detail_colors
            .iter()
            .filter(|color| color.red == protected.red && color.green == protected.green)
            .count(),
        1
    );
    assert_eq!(
        read_detail_colors
            .iter()
            .filter(|color| color.red == converted.red && color.blue == converted.blue)
            .count(),
        1
    );
    assert_eq!(
        read_detail_colors
            .iter()
            .filter(|color| color.red == ordinary_t.red && color.green == ordinary_t.green)
            .count(),
        0
    );
    assert_eq!(
        read_detail_colors
            .iter()
            .filter(|color| color.red == ordinary_a.red && color.green == ordinary_a.green)
            .count(),
        1
    );
    assert_eq!(layout.display_list.texts.len(), 1);
}

#[test]
fn smrt_kinetics_marks_remain_visible_across_base_and_read_detail_levels() {
    let mut record = row_test_record(b"smrt", 0, 4);
    record.sequence = SequenceData::Present {
        bases: b"ACGT".to_vec(),
        qualities: None,
    };
    record.auxiliary_fields.push(AuxField {
        tag: *b"ip",
        value: AuxValue::UnsignedArray(vec![0, 100, 600, 6_000]),
    });
    let query = row_test_query(&[record]);
    let options = AlignmentLayoutOptions {
        reference_top: 0.0,
        coverage_top: 0.0,
        reads_top: 0.0,
        show_reference: false,
        show_coverage: false,
        show_junctions: false,
        show_modification_frequency: false,
        show_modified_bases: false,
        color_mode: AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::SubreadIpd),
        read_base_display_mode: ReadBaseDisplayMode::Hidden,
        ..AlignmentLayoutOptions::default()
    };
    let rows = assign_alignment_rows(&query, options, None);
    for viewport in [
        Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 4).unwrap(), 80.0).unwrap(),
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
            100.0,
        )
        .unwrap(),
    ] {
        let layout = layout_alignment_query_with_rows(&query, viewport, options, &rows);
        for expected in [0_u64, 100, 600, 6_000].map(smrt_kinetics_color) {
            assert!(layout.display_list.rectangles.iter().any(|rectangle| {
                (rectangle.color.red - expected.red).abs() < 1.0e-6
                    && (rectangle.color.green - expected.green).abs() < 1.0e-6
                    && (rectangle.color.blue - expected.blue).abs() < 1.0e-6
                    && (rectangle.color.alpha - expected.alpha).abs() < 1.0e-6
            }));
        }
        assert_eq!(layout.stats.smrt_kinetics_marks_omitted, 0);
        assert_eq!(layout.stats.smrt_kinetics_unavailable_records, 0);
        assert_eq!(layout.stats.smrt_kinetics_malformed_records, 0);
    }
}

#[test]
fn smrt_kinetics_missing_and_malformed_records_remain_visible_and_reported() {
    let mut missing = row_test_record(b"missing", 0, 4);
    missing.sequence = SequenceData::Present {
        bases: b"ACGT".to_vec(),
        qualities: None,
    };
    let mut malformed = row_test_record(b"malformed", 0, 4);
    malformed.sequence = SequenceData::Present {
        bases: b"ACGT".to_vec(),
        qualities: None,
    };
    malformed.auxiliary_fields.push(AuxField {
        tag: *b"ip",
        value: AuxValue::UnsignedArray(vec![1, 2, 3]),
    });
    let query = row_test_query(&[missing, malformed]);
    let options = AlignmentLayoutOptions {
        show_reference: false,
        show_coverage: false,
        show_junctions: false,
        show_modification_frequency: false,
        show_modified_bases: false,
        color_mode: AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::SubreadIpd),
        ..AlignmentLayoutOptions::default()
    };
    let viewport =
        Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 4).unwrap(), 80.0).unwrap();
    let layout = layout_alignment_query(&query, viewport, options);
    assert_eq!(layout.stats.rendered_alignments, 2);
    assert_eq!(layout.stats.smrt_kinetics_unavailable_records, 1);
    assert_eq!(layout.stats.smrt_kinetics_malformed_records, 1);
}

#[test]
fn smrt_kinetics_palette_and_render_budget_are_bounded() {
    assert_eq!(smrt_kinetics_color(6_000), smrt_kinetics_color(u64::MAX));
    assert!(smrt_kinetics_color(100).red > smrt_kinetics_color(100).green);
    assert!(smrt_kinetics_color(600).green > 0.9);
    assert!(smrt_kinetics_color(6_000).blue > 0.9);

    let mut budget = SmrtKineticsRenderBudget::new(1);
    assert!(budget.take());
    assert!(!budget.take());
    assert!(!budget.take());
    assert_eq!(budget.omitted, 2);
}

fn ultima_insertion_record() -> AlignmentRecord {
    let mut record = row_test_record(b"ultima-insertion", 0, 6);
    record.cigar = vec![
        CigarOp {
            kind: CigarOpKind::Match,
            len: 3,
        },
        CigarOp {
            kind: CigarOpKind::Insertion,
            len: 2,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 3,
        },
    ];
    record.sequence = SequenceData::Present {
        bases: b"ACGAAATC".to_vec(),
        qualities: Some(vec![30, 30, 30, 30, 40, 30, 30, 30]),
    };
    record.auxiliary_fields.push(AuxField {
        tag: *b"tp",
        value: AuxValue::SignedArray(vec![0, 0, 0, 0, -2, 0, 0, 0]),
    });
    record
}

fn ultima_layout_options(options: UltimaDisplayOptions) -> AlignmentLayoutOptions {
    AlignmentLayoutOptions {
        reference_top: 0.0,
        coverage_top: 0.0,
        reads_top: 0.0,
        show_reference: false,
        show_coverage: false,
        show_junctions: false,
        show_modification_frequency: false,
        show_modified_bases: false,
        color_mode: AlignmentColorMode::Ultima(options),
        read_base_display_mode: ReadBaseDisplayMode::Hidden,
        ..AlignmentLayoutOptions::default()
    }
}

#[test]
fn ultima_insertion_quality_is_scale_stable_at_read_and_base_detail() {
    let query = row_test_query(&[ultima_insertion_record()]);
    let options = ultima_layout_options(UltimaDisplayOptions::default());
    let rows = assign_alignment_rows(&query, options, None);
    let expected = ultima_quality_color(40.0);
    for viewport in [
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 0, 10).unwrap(),
            200.0,
        )
        .unwrap(),
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
            100.0,
        )
        .unwrap(),
    ] {
        let layout = layout_alignment_query_with_rows(&query, viewport, options, &rows);
        let mark = layout
            .display_list
            .rectangles
            .iter()
            .find(|rectangle| rectangle.color == expected)
            .expect("Ultima insertion quality mark");
        assert_eq!(
            mark.object_id & READ_DETAIL_OBJECT_BIT,
            READ_DETAIL_OBJECT_BIT
        );
        assert!(mark.width <= 6.0);
        assert!(mark.height <= 2.0);
        assert_eq!(layout.stats.ultima_quality_marks_omitted, 0);
        assert_eq!(layout.stats.ultima_unavailable_records, 0);
        assert_eq!(layout.stats.ultima_malformed_records, 0);
        assert_eq!(layout.stats.ultima_unavailable_events, 0);
    }
}

#[test]
fn ultima_deletion_uses_read_group_maximum_homopolymer_length() {
    let mut record = row_test_record(b"ultima-deletion", 0, 8);
    record.cigar = vec![
        CigarOp {
            kind: CigarOpKind::Match,
            len: 3,
        },
        CigarOp {
            kind: CigarOpKind::Deletion,
            len: 2,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 3,
        },
    ];
    record.sequence = SequenceData::Present {
        bases: b"ACGAAA".to_vec(),
        qualities: Some(vec![30, 30, 30, 30, 38, 30]),
    };
    record.auxiliary_fields.extend([
        AuxField {
            tag: *b"tp",
            value: AuxValue::SignedArray(vec![0, 0, 0, 0, 2, 0]),
        },
        AuxField {
            tag: *b"RG",
            value: AuxValue::String("ultima".to_owned()),
        },
    ]);
    let mut query = row_test_query(&[record]);
    query.reference = Some(ReferenceSlice {
        interval: GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(),
        bases: b"ACGAAAAA".to_vec(),
    });
    query.header = Arc::new(AlignmentHeaderMetadata {
        read_groups: vec![AlignmentReadGroup {
            id: b"ultima".to_vec(),
            platform: Some("ULTIMA".to_owned()),
            maximum_homopolymer_length: Some(3),
            ultima_tag_orientation: Some(rgv_hts::UltimaTagOrientation::Reference),
        }],
        read_groups_omitted: 0,
        ..AlignmentHeaderMetadata::default()
    });
    let viewport =
        Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(), 160.0).unwrap();
    let layout = layout_alignment_query(
        &query,
        viewport,
        ultima_layout_options(UltimaDisplayOptions::default()),
    );
    let mark = layout
        .display_list
        .rectangles
        .iter()
        .find(|rectangle| rectangle.color == ultima_quality_color(0.0))
        .expect("Q0 maximum-homopolymer deletion mark");
    assert!((mark.x - 60.0).abs() < 1.1);
    assert!((mark.height - AlignmentLayoutOptions::default().read_height).abs() < f32::EPSILON);
}

#[test]
fn ultima_missing_and_malformed_evidence_leave_reads_visible() {
    let mut missing = ultima_insertion_record();
    missing.read_name = b"missing".to_vec();
    missing.auxiliary_fields.retain(|field| field.tag != *b"tp");
    let mut malformed = ultima_insertion_record();
    malformed.read_name = b"malformed".to_vec();
    malformed.auxiliary_fields[0].value = AuxValue::String("not-an-array".to_owned());
    let query = row_test_query(&[missing, malformed]);
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 0, 10).unwrap(),
        200.0,
    )
    .unwrap();
    let layout = layout_alignment_query(
        &query,
        viewport,
        ultima_layout_options(UltimaDisplayOptions::default()),
    );
    assert_eq!(layout.stats.rendered_alignments, 2);
    assert_eq!(layout.stats.ultima_unavailable_records, 1);
    assert_eq!(layout.stats.ultima_malformed_records, 1);
    assert_eq!(layout.stats.ultima_quality_marks_omitted, 0);
}

#[test]
fn ultima_palette_and_render_budget_match_the_bounded_jet_scale() {
    let rgb = |quality| {
        let color = ultima_quality_color(quality);
        [
            (color.red * 255.0).round() as u8,
            (color.green * 255.0).round() as u8,
            (color.blue * 255.0).round() as u8,
        ]
    };
    assert_eq!(rgb(0.0), [0, 0, 139]);
    assert_eq!(rgb(5.0), [0, 0, 255]);
    assert_eq!(rgb(16.0), [0, 255, 255]);
    assert_eq!(rgb(27.0), [255, 255, 0]);
    assert_eq!(rgb(38.0), [255, 0, 0]);
    assert_eq!(rgb(43.0), [140, 0, 0]);
    assert_eq!(ultima_quality_color(500.0), ultima_quality_color(43.0));

    let mut budget = UltimaRenderBudget::new(1);
    assert!(budget.take());
    assert!(!budget.take());
    assert!(!budget.take());
    assert_eq!(budget.omitted, 2);
}

fn sbx_insertion_record(name: &[u8], qualities: Vec<u8>) -> AlignmentRecord {
    let mut record = row_test_record(name, 0, 2);
    record.cigar = vec![
        CigarOp {
            kind: CigarOpKind::Match,
            len: 1,
        },
        CigarOp {
            kind: CigarOpKind::Insertion,
            len: 2,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 1,
        },
    ];
    record.sequence = SequenceData::Present {
        bases: b"ACGT".to_vec(),
        qualities: Some(qualities),
    };
    record
}

#[test]
fn sbx_insertion_colors_reach_the_display_list() {
    let query = row_test_query(&[
        sbx_insertion_record(b"full", vec![39, 5, 10, 39]),
        sbx_insertion_record(b"partial", vec![39, 39, 5, 39]),
        sbx_insertion_record(b"concordant", vec![39; 4]),
    ]);
    let viewport =
        Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 4).unwrap(), 80.0).unwrap();
    let layout = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            reference_top: 0.0,
            coverage_top: 0.0,
            reads_top: 0.0,
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modification_frequency: false,
            show_modified_bases: false,
            color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions {
                simplex_tail_mode: SbxSimplexTailMode::Off,
                ..SbxDisplayOptions::default()
            }),
            read_base_display_mode: ReadBaseDisplayMode::Hidden,
            ..AlignmentLayoutOptions::default()
        },
    );
    for expected in [
        Rgba::new(0.71, 0.71, 0.71, 0.98),
        Rgba::new(0.55, 0.55, 0.55, 0.98),
        Rgba::new(0.86, 0.32, 0.82, 0.96),
    ] {
        assert!(
            layout
                .display_list
                .rectangles
                .iter()
                .any(|rectangle| rectangle.color == expected)
        );
    }
    assert_eq!(layout.stats.sbx_unavailable_records, 0);
    assert_eq!(layout.stats.sbx_unavailable_insertions, 0);
}

#[test]
fn sbx_simplex_tails_highlight_at_read_and_base_detail() {
    let mut record = row_test_record(b"tails", 0, 8);
    record.sequence = SequenceData::Present {
        bases: b"ACGTACGT".to_vec(),
        qualities: Some(vec![20, 25, 39, 39, 39, 39, 18, 5]),
    };
    let query = row_test_query(&[record]);
    let options = AlignmentLayoutOptions {
        reference_top: 0.0,
        coverage_top: 0.0,
        reads_top: 0.0,
        show_reference: false,
        show_coverage: false,
        show_junctions: false,
        show_modification_frequency: false,
        show_modified_bases: false,
        color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions::default()),
        read_base_display_mode: ReadBaseDisplayMode::Hidden,
        ..AlignmentLayoutOptions::default()
    };
    for (viewport, expected_marks) in [
        (
            Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(), 80.0).unwrap(),
            4,
        ),
        (
            Viewport::new(
                GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
                100.0,
            )
            .unwrap(),
            2,
        ),
    ] {
        let layout = layout_alignment_query(&query, viewport, options);
        assert_eq!(
            layout
                .display_list
                .rectangles
                .iter()
                .filter(|rectangle| rectangle.color == sbx_simplex_tail_color())
                .count(),
            expected_marks
        );
        assert_eq!(layout.stats.sbx_tail_marks_omitted, 0);
    }
}

#[test]
fn sbx_hide_clips_tails_and_all_tail_reads_without_losing_record_status() {
    let mut partly_retained = row_test_record(b"partly-retained", 0, 8);
    partly_retained.sequence = SequenceData::Present {
        bases: b"ACGTACGT".to_vec(),
        qualities: Some(vec![20, 25, 39, 39, 39, 39, 18, 5]),
    };
    let viewport =
        Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(), 80.0).unwrap();
    let options = AlignmentLayoutOptions {
        reference_top: 0.0,
        coverage_top: 0.0,
        reads_top: 0.0,
        show_reference: false,
        show_coverage: false,
        show_junctions: false,
        show_modification_frequency: false,
        show_modified_bases: false,
        color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions {
            simplex_tail_mode: SbxSimplexTailMode::Hide,
            ..SbxDisplayOptions::default()
        }),
        read_base_display_mode: ReadBaseDisplayMode::Hidden,
        ..AlignmentLayoutOptions::default()
    };
    let layout = layout_alignment_query(&row_test_query(&[partly_retained]), viewport, options);
    let outer_body = layout
        .display_list
        .rectangles
        .iter()
        .find(|rectangle| rectangle.object_id == 1)
        .unwrap();
    assert!((outer_body.x - 20.0).abs() < 1.0e-6);
    assert!((outer_body.width - 40.0).abs() < 1.0e-6);

    let mut all_tail = row_test_record(b"all-tail", 0, 8);
    all_tail.sequence = SequenceData::Present {
        bases: b"ACGTACGT".to_vec(),
        qualities: Some(vec![5; 8]),
    };
    let layout = layout_alignment_query(&row_test_query(&[all_tail]), viewport, options);
    assert_eq!(layout.stats.rendered_alignments, 1);
    assert!(layout.display_list.rectangles.is_empty());
}

#[test]
fn sbx_hide_removes_reference_only_events_at_the_trimmed_left_boundary() {
    let mut record = row_test_record(b"left-tail-deletion", 0, 9);
    record.cigar = vec![
        CigarOp {
            kind: CigarOpKind::Match,
            len: 2,
        },
        CigarOp {
            kind: CigarOpKind::Deletion,
            len: 3,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 4,
        },
    ];
    record.sequence = SequenceData::Present {
        bases: b"ACGTAC".to_vec(),
        qualities: Some(vec![20, 20, 39, 39, 39, 39]),
    };
    let query = row_test_query(&[record]);
    let viewport =
        Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 9).unwrap(), 90.0).unwrap();
    let layout = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            reference_top: 0.0,
            coverage_top: 0.0,
            reads_top: 0.0,
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modification_frequency: false,
            show_modified_bases: false,
            color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions {
                simplex_tail_mode: SbxSimplexTailMode::Hide,
                ..SbxDisplayOptions::default()
            }),
            read_base_display_mode: ReadBaseDisplayMode::Hidden,
            ..AlignmentLayoutOptions::default()
        },
    );
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .all(|rectangle| { rectangle.color != Rgba::new(0.12, 0.12, 0.14, 0.96) })
    );
    let outer_body = layout
        .display_list
        .rectangles
        .iter()
        .find(|rectangle| rectangle.object_id == 1)
        .unwrap();
    assert!((outer_body.x - 50.0).abs() < 1.0e-6);
    assert!((outer_body.width - 40.0).abs() < 1.0e-6);
}

#[test]
fn sbx_missing_qualities_leave_reads_visible_and_report_unclassified_insertions() {
    let mut record = sbx_insertion_record(b"missing-quality", vec![39; 4]);
    record.sequence = SequenceData::Present {
        bases: b"ACGT".to_vec(),
        qualities: None,
    };
    let query = row_test_query(&[record]);
    let viewport =
        Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 4).unwrap(), 80.0).unwrap();
    let layout = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            reference_top: 0.0,
            coverage_top: 0.0,
            reads_top: 0.0,
            show_reference: false,
            show_coverage: false,
            show_junctions: false,
            show_modification_frequency: false,
            show_modified_bases: false,
            color_mode: AlignmentColorMode::Sbx(SbxDisplayOptions::default()),
            ..AlignmentLayoutOptions::default()
        },
    );
    assert_eq!(layout.stats.rendered_alignments, 1);
    assert!(!layout.display_list.rectangles.is_empty());
    assert_eq!(layout.stats.sbx_unavailable_records, 1);
    assert_eq!(layout.stats.sbx_unavailable_insertions, 1);
}

#[test]
fn sbx_tail_render_budget_is_bounded_and_reports_omissions() {
    let mut budget = SbxRenderBudget::new(1);
    assert!(budget.take());
    assert!(!budget.take());
    assert!(!budget.take());
    assert_eq!(budget.omitted, 2);
}

#[test]
fn stable_rows_survive_viewport_zoom_and_detail_transition() {
    let query = row_test_query(&[
        row_test_record(b"leading", 0, 70),
        row_test_record(b"retained", 50, 110),
    ]);
    let options = AlignmentLayoutOptions::default();
    let rows = assign_alignment_rows(&query, options, None);
    assert_eq!(rows.rows, vec![Some(0), Some(1)]);

    let broad = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 0, 120).unwrap(),
        500.0,
    )
    .unwrap();
    let narrow = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 71, 120).unwrap(),
        500.0,
    )
    .unwrap();
    assert_eq!(broad.detail_level(), DetailLevel::Reads);
    assert_eq!(narrow.detail_level(), DetailLevel::Bases);
    let broad_layout = layout_alignment_query_with_rows(&query, broad, options, &rows);
    let narrow_layout = layout_alignment_query_with_rows(&query, narrow, options, &rows);
    let read_y = |layout: &AlignmentLayout| {
        layout
            .display_list
            .rectangles
            .iter()
            .find(|primitive| primitive.object_id == 2)
            .map(|primitive| primitive.y)
            .unwrap()
    };
    assert!((read_y(&broad_layout) - read_y(&narrow_layout)).abs() < f32::EPSILON);
    assert!((read_y(&narrow_layout) - (options.reads_top + options.row_height)).abs() < 1.0e-6);
}

#[test]
fn chimeric_grouping_and_clip_sorting_use_normalized_evidence() {
    let mut left = row_test_record(b"left", 10, 100);
    left.cigar = vec![
        CigarOp {
            kind: CigarOpKind::SoftClip,
            len: 10,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 90,
        },
    ];
    left.auxiliary_fields.push(AuxField {
        tag: *b"SA",
        value: AuxValue::String("chr2,101,+,100M,60,0;".to_owned()),
    });
    let mut more_clipped = row_test_record(b"more-clipped", 10, 100);
    more_clipped.cigar = vec![
        CigarOp {
            kind: CigarOpKind::SoftClip,
            len: 20,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 80,
        },
    ];
    more_clipped.flags = AlignmentFlags::from_bits(0x800);
    let query = row_test_query(&[left, more_clipped]);

    assert_eq!(
        grouped_indices(
            &query,
            AlignmentGroupMode::Chimeric,
            AlignmentSortMode::Start,
            AlignmentOrderDirection::Natural,
            None,
            TemplateLengthBounds::default(),
            None,
        )
        .len(),
        2
    );
    assert_eq!(
        grouped_indices(
            &query,
            AlignmentGroupMode::Supplementary,
            AlignmentSortMode::Start,
            AlignmentOrderDirection::Natural,
            None,
            TemplateLengthBounds::default(),
            None,
        )
        .len(),
        2
    );
    let rows = assign_alignment_rows(
        &query,
        AlignmentLayoutOptions {
            sort_mode: AlignmentSortMode::LeftClip,
            ..AlignmentLayoutOptions::default()
        },
        None,
    );
    assert_eq!(rows.rows, vec![Some(1), Some(0)]);
}

#[test]
fn group_and_sort_directions_are_independent_and_deterministic() {
    let forward = row_test_record(b"forward", 10, 30);
    let mut reverse = row_test_record(b"reverse", 10, 30);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    let query = row_test_query(&[forward, reverse]);

    let natural = assign_alignment_rows(
        &query,
        AlignmentLayoutOptions {
            group_mode: AlignmentGroupMode::Strand,
            max_rows: 4,
            ..AlignmentLayoutOptions::default()
        },
        None,
    );
    let reversed = assign_alignment_rows(
        &query,
        AlignmentLayoutOptions {
            group_mode: AlignmentGroupMode::Strand,
            group_order: AlignmentOrderDirection::Reverse,
            max_rows: 4,
            ..AlignmentLayoutOptions::default()
        },
        Some(&natural),
    );

    assert!(natural.rows[0] < natural.rows[1]);
    assert!(reversed.rows[1] < reversed.rows[0]);

    let naturally_sorted = grouped_indices(
        &query,
        AlignmentGroupMode::None,
        AlignmentSortMode::ReadName,
        AlignmentOrderDirection::Natural,
        None,
        TemplateLengthBounds::default(),
        None,
    );
    let reverse_sorted = grouped_indices(
        &query,
        AlignmentGroupMode::None,
        AlignmentSortMode::ReadName,
        AlignmentOrderDirection::Reverse,
        None,
        TemplateLengthBounds::default(),
        None,
    );
    assert_eq!(naturally_sorted.values().next().unwrap(), &vec![0, 1]);
    assert_eq!(reverse_sorted.values().next().unwrap(), &vec![1, 0]);
}

#[test]
fn sparse_alignment_groups_are_packed_without_reserved_empty_rows() {
    let forward = row_test_record(b"forward", 10, 30);
    let mut reverse = row_test_record(b"reverse", 10, 30);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    let query = row_test_query(&[forward, reverse]);
    let options = AlignmentLayoutOptions {
        group_mode: AlignmentGroupMode::Strand,
        max_rows: 4_096,
        ..AlignmentLayoutOptions::default()
    };

    let rows = assign_alignment_rows(&query, options, None);

    assert_eq!(rows.rows, vec![Some(0), Some(1)]);
}

#[test]
fn grouping_does_not_hide_a_dense_group_when_capacity_covers_the_batch() {
    let mut records = (0..6)
        .map(|index| row_test_record(format!("forward-{index}").as_bytes(), 10, 30))
        .collect::<Vec<_>>();
    let mut reverse = row_test_record(b"reverse", 10, 30);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    records.push(reverse);
    let query = row_test_query(&records);
    let rows = assign_alignment_rows(
        &query,
        AlignmentLayoutOptions {
            group_mode: AlignmentGroupMode::Strand,
            max_rows: records.len(),
            ..AlignmentLayoutOptions::default()
        },
        None,
    );

    assert_eq!(
        rows.rows.iter().filter(|row| row.is_some()).count(),
        records.len()
    );
}

#[test]
fn history_preserves_local_rows_while_compacting_following_groups() {
    let leading = row_test_record(b"leading", 10, 40);
    let retained = row_test_record(b"retained", 20, 50);
    let mut reverse = row_test_record(b"reverse", 20, 50);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    let broad_query = row_test_query(&[leading, retained, reverse.clone()]);
    let options = AlignmentLayoutOptions {
        group_mode: AlignmentGroupMode::Strand,
        max_rows: 4_096,
        ..AlignmentLayoutOptions::default()
    };
    let broad_rows = assign_alignment_rows(&broad_query, options, None);
    assert_eq!(broad_rows.rows, vec![Some(0), Some(1), Some(2)]);

    let refined_query = row_test_query(&[row_test_record(b"retained", 20, 50), reverse]);
    let refined_rows = assign_alignment_rows(&refined_query, options, Some(&broad_rows));

    assert_eq!(refined_rows.rows, vec![Some(1), Some(2)]);
}

#[test]
fn selected_read_promotion_precedes_configured_sort_without_changing_identity() {
    let records = [
        row_test_record(b"alpha", 10, 30),
        row_test_record(b"selected", 10, 30),
        row_test_record(b"zeta", 10, 30),
    ];
    let query = row_test_query(&records);
    let promoted = alignment_read_name_hash(b"selected");
    let groups = grouped_indices(
        &query,
        AlignmentGroupMode::None,
        AlignmentSortMode::ReadName,
        AlignmentOrderDirection::Reverse,
        None,
        TemplateLengthBounds::default(),
        Some(promoted),
    );
    assert_eq!(groups.values().next().unwrap(), &vec![1, 2, 0]);

    let ordinary = assign_alignment_rows(
        &query,
        AlignmentLayoutOptions {
            sort_mode: AlignmentSortMode::ReadName,
            max_rows: 3,
            ..AlignmentLayoutOptions::default()
        },
        None,
    );
    let promoted_rows = assign_alignment_rows(
        &query,
        AlignmentLayoutOptions {
            sort_mode: AlignmentSortMode::ReadName,
            promoted_read_name_hash: Some(promoted),
            max_rows: 3,
            ..AlignmentLayoutOptions::default()
        },
        Some(&ordinary),
    );
    assert_eq!(promoted_rows.rows[1], Some(0));
    assert_ne!(
        ordinary.promoted_read_name_hash,
        promoted_rows.promoted_read_name_hash
    );
    assert_eq!(ordinary.identities, promoted_rows.identities);
}

#[test]
fn base_sort_uses_sam_alignment_order_and_explicit_gap_states() {
    let mut adenine = row_test_record(b"adenine", 10, 20);
    adenine.sequence = SequenceData::Present {
        bases: b"AAAAAAAAAA".to_vec(),
        qualities: None,
    };
    let mut reverse_cytosine = row_test_record(b"reverse-cytosine", 10, 20);
    reverse_cytosine.flags = AlignmentFlags::from_bits(0x10);
    reverse_cytosine.sequence = SequenceData::Present {
        // SAM/BAM SEQ is already reference-facing when flag 0x10 is set.
        bases: b"CCCCCCCCCC".to_vec(),
        qualities: None,
    };
    let mut deletion = row_test_record(b"deletion", 10, 20);
    deletion.cigar = vec![
        CigarOp {
            kind: CigarOpKind::Match,
            len: 5,
        },
        CigarOp {
            kind: CigarOpKind::Deletion,
            len: 1,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 4,
        },
    ];
    deletion.sequence = SequenceData::Present {
        bases: b"AAAAAAAAA".to_vec(),
        qualities: None,
    };
    let unavailable = row_test_record(b"unavailable", 10, 20);
    let query = row_test_query(&[adenine, reverse_cytosine, deletion, unavailable]);

    let natural = grouped_indices(
        &query,
        AlignmentGroupMode::None,
        AlignmentSortMode::Base,
        AlignmentOrderDirection::Natural,
        Some(15),
        TemplateLengthBounds::default(),
        None,
    );
    assert_eq!(natural.values().next().unwrap(), &vec![0, 1, 2, 3]);

    let reverse = grouped_indices(
        &query,
        AlignmentGroupMode::None,
        AlignmentSortMode::Base,
        AlignmentOrderDirection::Reverse,
        Some(15),
        TemplateLengthBounds::default(),
        None,
    );
    assert_eq!(reverse.values().next().unwrap(), &vec![3, 2, 1, 0]);
}

#[test]
fn strand_and_tag_sort_modes_use_normalized_record_fields() {
    let mut reverse_zeta = row_test_record(b"reverse-zeta", 10, 20);
    reverse_zeta.flags = AlignmentFlags::from_bits(0x10);
    reverse_zeta.auxiliary_fields.push(AuxField {
        tag: *b"RG",
        value: AuxValue::String("zeta".to_owned()),
    });
    let mut forward_alpha = row_test_record(b"forward-alpha", 10, 20);
    forward_alpha.auxiliary_fields.push(AuxField {
        tag: *b"RG",
        value: AuxValue::String("alpha".to_owned()),
    });
    let query = row_test_query(&[reverse_zeta, forward_alpha]);

    let strand = grouped_indices(
        &query,
        AlignmentGroupMode::None,
        AlignmentSortMode::Strand,
        AlignmentOrderDirection::Natural,
        None,
        TemplateLengthBounds::default(),
        None,
    );
    assert_eq!(strand.values().next().unwrap(), &vec![1, 0]);

    for mode in [AlignmentSortMode::ReadGroup, AlignmentSortMode::Tag(*b"RG")] {
        let groups = grouped_indices(
            &query,
            AlignmentGroupMode::None,
            mode,
            AlignmentOrderDirection::Natural,
            None,
            TemplateLengthBounds::default(),
            None,
        );
        assert_eq!(groups.values().next().unwrap(), &vec![1, 0]);
    }
}

#[test]
fn selected_chain_hash_adds_non_pickable_outlines_to_every_loaded_segment() {
    let records = [
        row_test_record(b"split", 10, 30),
        row_test_record(b"split", 40, 60),
        row_test_record(b"other", 70, 90),
    ];
    let query = row_test_query(&records);
    let options = AlignmentLayoutOptions {
        selected_chain_read_name_hash: Some(alignment_read_name_hash(b"split")),
        ..AlignmentLayoutOptions::default()
    };
    for (pixel_width, expected_level) in
        [(800.0, DetailLevel::Reads), (10.0, DetailLevel::Regional)]
    {
        let viewport = Viewport::new(query.interval, pixel_width).unwrap();
        assert_eq!(viewport.detail_level(), expected_level);
        let layout = layout_alignment_query(&query, viewport, options);
        let outlines = layout
            .display_list
            .rectangles
            .iter()
            .filter(|primitive| {
                primitive.object_id == 0 && primitive.color == Rgba::new(0.05, 0.66, 0.78, 1.0)
            })
            .count();
        assert_eq!(outlines, 8);
    }
    assert_eq!(
        alignment_read_name_hash(b"split"),
        alignment_read_name_hash(b"split")
    );
    assert_ne!(
        alignment_read_name_hash(b"split"),
        alignment_read_name_hash(b"other")
    );
}

#[test]
fn pair_view_packs_both_ends_as_one_row_and_one_pick_target() {
    let mut left = row_test_record(b"pair", 10, 20);
    left.flags = AlignmentFlags::from_bits(0x63);
    left.mate_contig = Some(left.contig);
    left.mate_start = Some(50);
    left.template_length = 50;
    let mut middle = row_test_record(b"other", 25, 35);
    middle.flags = AlignmentFlags::from_bits(0);
    let mut right = row_test_record(b"pair", 50, 60);
    right.flags = AlignmentFlags::from_bits(0x93);
    right.mate_contig = Some(right.contig);
    right.mate_start = Some(10);
    right.template_length = -50;
    let query = row_test_query(&[left, middle, right]);
    let options = AlignmentLayoutOptions {
        view_as_pairs: true,
        ..AlignmentLayoutOptions::default()
    };
    let rows = assign_alignment_rows(&query, options, None);
    assert_eq!(rows.rows[0], rows.rows[2]);
    assert_ne!(rows.rows[0], rows.rows[1]);

    let viewport = Viewport::new(query.interval, 800.0).unwrap();
    let layout = layout_alignment_query_with_rows(&query, viewport, options, &rows);
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .all(|primitive| { crate::record_object_id(primitive.object_id) != 3 })
    );
    let connectors = layout
        .display_list
        .rectangles
        .iter()
        .filter(|primitive| {
            primitive.color == Rgba::new(0.58, 0.64, 0.74, 0.62) && primitive.object_id == 1
        })
        .count();
    assert_eq!(connectors, 1);
    let terminal_shapes = layout
        .display_list
        .rectangles
        .iter()
        .filter(|primitive| record_object_id(primitive.object_id) == 1)
        .map(|primitive| primitive.shape)
        .collect::<Vec<_>>();
    assert!(terminal_shapes.contains(&RectShape::DirectionalTerminalRight));
    assert!(terminal_shapes.contains(&RectShape::DirectionalTerminalLeft));
}

#[test]
fn reads_keep_direction_when_pair_view_is_disabled() {
    let mut record = row_test_record(b"pair", 10, 20);
    record.flags = AlignmentFlags::from_bits(0x41);
    record.mate_contig = Some(record.contig);
    record.mate_start = Some(50);
    let query = row_test_query(&[record]);
    let viewport = Viewport::new(query.interval, 800.0).unwrap();
    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .any(|primitive| primitive.shape == RectShape::DirectionalTerminalRight)
    );
}

#[test]
fn paired_read_direction_survives_the_regional_detail_boundary() {
    let mut record = row_test_record(b"regional-pair", 40, 160);
    record.flags = AlignmentFlags::from_bits(0x41);
    record.mate_contig = Some(record.contig);
    record.mate_start = Some(170);
    let query = row_test_query(&[record]);
    let viewport = Viewport::new(query.interval, 10.0).unwrap();
    assert_eq!(viewport.detail_level(), DetailLevel::Regional);
    let layout = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            view_as_pairs: true,
            ..AlignmentLayoutOptions::default()
        },
    );
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .any(|primitive| { primitive.shape == RectShape::DirectionalTerminalRight })
    );
}

#[test]
fn unpaired_reverse_read_keeps_direction_at_regional_detail() {
    let mut record = row_test_record(b"regional-reverse", 40, 160);
    record.flags = AlignmentFlags::from_bits(0x10);
    let query = row_test_query(&[record]);
    let viewport = Viewport::new(query.interval, 10.0).unwrap();
    assert_eq!(viewport.detail_level(), DetailLevel::Regional);
    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .any(|primitive| primitive.shape == RectShape::DirectionalTerminalLeft)
    );
}

#[test]
fn visible_spliced_blocks_keep_direction_when_the_outer_terminal_is_clipped() {
    let mut reverse = row_test_record(b"spliced-reverse", 40, 180);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    reverse.cigar = vec![
        CigarOp {
            kind: CigarOpKind::Match,
            len: 20,
        },
        CigarOp {
            kind: CigarOpKind::ReferenceSkip,
            len: 100,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 20,
        },
    ];
    let reverse_query = row_test_query(&[reverse]);
    let reverse_viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 130, 190).unwrap(),
        300.0,
    )
    .unwrap();
    let reverse_layout = layout_alignment_query(
        &reverse_query,
        reverse_viewport,
        AlignmentLayoutOptions::default(),
    );
    let visible_block_left = reverse_viewport.pixel_at_genomic(160) as f32;
    assert!(
        reverse_layout
            .display_list
            .rectangles
            .iter()
            .any(|primitive| {
                primitive.shape == RectShape::DirectionalTerminalLeft
                    && (primitive.x - visible_block_left).abs() <= 1.0
            })
    );

    let mut forward = reverse_query.materialize_record(0).unwrap();
    forward.read_name = b"spliced-forward".to_vec();
    forward.flags = AlignmentFlags::from_bits(0);
    let forward_query = row_test_query(&[forward]);
    let forward_viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 30, 100).unwrap(),
        350.0,
    )
    .unwrap();
    let forward_layout = layout_alignment_query(
        &forward_query,
        forward_viewport,
        AlignmentLayoutOptions::default(),
    );
    let visible_block_right = forward_viewport.pixel_at_genomic(60) as f32;
    assert!(
        forward_layout
            .display_list
            .rectangles
            .iter()
            .any(|primitive| {
                primitive.shape == RectShape::DirectionalTerminalRight
                    && (primitive.x + primitive.width - visible_block_right).abs() <= 1.0
            })
    );
}

#[test]
fn spliced_block_direction_survives_regional_detail() {
    let mut reverse = row_test_record(b"regional-spliced-reverse", 40, 180);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    reverse.cigar = vec![
        CigarOp {
            kind: CigarOpKind::Match,
            len: 20,
        },
        CigarOp {
            kind: CigarOpKind::ReferenceSkip,
            len: 100,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 20,
        },
    ];
    let query = row_test_query(&[reverse]);
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 130, 190).unwrap(),
        5.0,
    )
    .unwrap();
    assert_eq!(viewport.detail_level(), DetailLevel::Regional);
    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .any(|primitive| primitive.shape == RectShape::DirectionalTerminalLeft)
    );
}

#[test]
fn clipped_alignment_edges_are_not_presented_as_biological_terminals() {
    let forward = row_test_record(b"clipped-forward", 40, 160);
    let mut reverse = row_test_record(b"clipped-reverse", 40, 160);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    let query = row_test_query(&[forward, reverse]);
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 50, 150).unwrap(),
        500.0,
    )
    .unwrap();
    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .all(|primitive| primitive.shape == RectShape::Rectangle)
    );
}

#[test]
fn every_alignment_class_keeps_its_biological_terminal() {
    let primary = row_test_record(b"primary", 10, 30);

    let mut secondary = row_test_record(b"secondary", 35, 55);
    secondary.flags = AlignmentFlags::from_bits(0x110);

    let mut supplementary = row_test_record(b"supplementary", 60, 80);
    supplementary.flags = AlignmentFlags::from_bits(0x800);

    let mut paired = row_test_record(b"paired", 85, 105);
    paired.flags = AlignmentFlags::from_bits(0x51);
    paired.mate_contig = Some(paired.contig);
    paired.mate_start = Some(150);

    let long = row_test_record(b"long", 10, 190);

    let mut spliced = row_test_record(b"spliced", 100, 180);
    spliced.cigar = vec![
        CigarOp {
            kind: CigarOpKind::Match,
            len: 20,
        },
        CigarOp {
            kind: CigarOpKind::ReferenceSkip,
            len: 40,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 20,
        },
    ];

    let records = [primary, secondary, supplementary, paired, long, spliced];
    let query = row_test_query(&records);
    let viewport = Viewport::new(query.interval, 800.0).unwrap();
    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());

    let expected = [
        RectShape::DirectionalTerminalRight,
        RectShape::DirectionalTerminalLeft,
        RectShape::DirectionalTerminalRight,
        RectShape::DirectionalTerminalLeft,
        RectShape::DirectionalTerminalRight,
        RectShape::DirectionalTerminalRight,
    ];
    for (record_index, expected_shape) in expected.into_iter().enumerate() {
        let object_id = u32::try_from(record_index + 1).unwrap();
        assert!(layout.display_list.rectangles.iter().any(|primitive| {
            record_object_id(primitive.object_id) == object_id && primitive.shape == expected_shape
        }));
    }

    let spliced_object_id = u32::try_from(records.len()).unwrap();
    let spliced_terminals = layout
        .display_list
        .rectangles
        .iter()
        .filter(|primitive| {
            record_object_id(primitive.object_id) == spliced_object_id
                && primitive.shape == RectShape::DirectionalTerminalRight
        })
        .collect::<Vec<_>>();
    for block_end in [120, 180] {
        let expected_x = viewport.pixel_at_genomic(block_end) as f32;
        assert!(
            spliced_terminals
                .iter()
                .any(|primitive| { (primitive.x + primitive.width - expected_x).abs() <= 1.0 })
        );
    }
}

#[test]
fn alignment_direction_is_continuous_across_bases_reads_and_regional_thresholds() {
    let forward = row_test_record(b"forward", 40, 160);
    let mut reverse = row_test_record(b"reverse", 40, 160);
    reverse.flags = AlignmentFlags::from_bits(0x10);
    let query = row_test_query(&[forward, reverse]);

    for (bases_per_pixel, expected_detail) in [
        (0.124, DetailLevel::Bases),
        (0.126, DetailLevel::Reads),
        (9.99, DetailLevel::Reads),
        (10.01, DetailLevel::Regional),
    ] {
        let viewport = Viewport::new(
            query.interval,
            query.interval.len() as f64 / bases_per_pixel,
        )
        .unwrap();
        assert_eq!(viewport.detail_level(), expected_detail);
        let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
        assert!(layout.display_list.rectangles.iter().any(|primitive| {
            record_object_id(primitive.object_id) == 1
                && primitive.shape == RectShape::DirectionalTerminalRight
        }));
        assert!(layout.display_list.rectangles.iter().any(|primitive| {
            record_object_id(primitive.object_id) == 2
                && primitive.shape == RectShape::DirectionalTerminalLeft
        }));
    }
}

#[test]
fn pair_style_modes_use_explicit_classification_and_template_length() {
    let mut expected = row_test_record(b"expected", 10, 20);
    expected.flags = AlignmentFlags::from_bits(0x63);
    expected.mate_contig = Some(expected.contig);
    expected.mate_start = Some(50);
    expected.template_length = 50;
    let unpaired = row_test_record(b"unpaired", 70, 80);
    let query = row_test_query(&[expected, unpaired]);
    let bounds = TemplateLengthBounds {
        minimum: Some(10),
        maximum: Some(40),
    };

    let groups = grouped_indices(
        &query,
        AlignmentGroupMode::UnexpectedPair,
        AlignmentSortMode::Start,
        AlignmentOrderDirection::Natural,
        None,
        bounds,
        None,
    );
    assert_eq!(groups.len(), 2);
    assert!(
        groups
            .keys()
            .any(|key| { key == &AlignmentGroupKey::Tagged("Unexpected".to_owned()) })
    );
    assert!(
        groups
            .keys()
            .any(|key| { key == &AlignmentGroupKey::Tagged("Unpaired".to_owned()) })
    );

    let sorted = grouped_indices(
        &query,
        AlignmentGroupMode::None,
        AlignmentSortMode::TemplateLength,
        AlignmentOrderDirection::Natural,
        None,
        bounds,
        None,
    );
    assert_eq!(sorted.values().next().unwrap(), &vec![0, 1]);
    assert_eq!(
        alignment_color(&query, 0, AlignmentColorMode::UnexpectedPair, bounds, 1.0,),
        Rgba::new(0.91, 0.29, 0.24, 1.0)
    );
}

#[test]
fn chromosome_scale_start_establishes_rows_before_reads_appear() {
    let records = [
        row_test_record(b"leading", 900_000, 900_370),
        row_test_record(b"retained", 900_300, 900_500),
    ];
    let mut batch = AlignmentBatch::default();
    for record in &records {
        batch.push(record).unwrap();
    }
    let query = AlignmentQuery {
        interval: GenomicInterval::new(ContigId::new(0), 0, 3_000_000).unwrap(),
        header: Arc::default(),
        batch,
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats {
            records_seen: records.len(),
            records_eligible: records.len(),
            records_emitted: records.len(),
            ..QueryStats::default()
        },
    };
    let options = AlignmentLayoutOptions::default();
    let rows = assign_alignment_rows(&query, options, None);
    assert_eq!(rows.rows, vec![Some(0), Some(1)]);

    let chromosome = Viewport::new(query.interval, 1_000.0).unwrap();
    assert_eq!(chromosome.detail_level(), DetailLevel::Overview);
    let overview = layout_alignment_query_with_rows(&query, chromosome, options, &rows);
    assert!(
        overview
            .display_list
            .rectangles
            .iter()
            .all(|primitive| primitive.object_id == 0)
    );
    assert!(overview.display_list.texts.iter().any(|primitive| {
        primitive.text == "2 alignments present — zoom in to view individual reads"
    }));
    assert_eq!(overview.stats.rendered_alignments, 0);
    assert_eq!(overview.stats.hidden_alignments, 2);

    let reads = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 900_250, 900_650).unwrap(),
        1_000.0,
    )
    .unwrap();
    let bases = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 900_320, 900_370).unwrap(),
        1_000.0,
    )
    .unwrap();
    assert_eq!(reads.detail_level(), DetailLevel::Reads);
    assert_eq!(bases.detail_level(), DetailLevel::Bases);
    let reads_layout = layout_alignment_query_with_rows(&query, reads, options, &rows);
    let bases_layout = layout_alignment_query_with_rows(&query, bases, options, &rows);
    let retained_y = |layout: &AlignmentLayout| {
        layout
            .display_list
            .rectangles
            .iter()
            .find(|primitive| primitive.object_id == 2)
            .map(|primitive| primitive.y)
            .unwrap()
    };
    assert!((retained_y(&reads_layout) - retained_y(&bases_layout)).abs() < f32::EPSILON);
}

#[test]
fn refined_query_preserves_rows_for_shared_read_identities() {
    let broad_query = row_test_query(&[
        row_test_record(b"leading", 0, 70),
        row_test_record(b"retained", 50, 110),
    ]);
    let options = AlignmentLayoutOptions::default();
    let broad_rows = assign_alignment_rows(&broad_query, options, None);

    let refined_query = row_test_query(&[row_test_record(b"retained", 50, 110)]);
    let scratch_rows = assign_alignment_rows(&refined_query, options, None);
    let carried_rows = assign_alignment_rows(&refined_query, options, Some(&broad_rows));
    assert_eq!(scratch_rows.rows, vec![Some(0)]);
    assert_eq!(carried_rows.rows, vec![Some(1)]);
}

#[test]
fn uncapped_query_refresh_preserves_rows_when_loaded_read_count_changes() {
    let broad_query = row_test_query(&[
        row_test_record(b"leading", 0, 70),
        row_test_record(b"selected", 50, 110),
    ]);
    let broad_rows = assign_alignment_rows(
        &broad_query,
        AlignmentLayoutOptions {
            max_rows: broad_query.batch.len(),
            ..AlignmentLayoutOptions::default()
        },
        None,
    );
    assert_eq!(broad_rows.rows, vec![Some(0), Some(1)]);

    let refined_query = row_test_query(&[
        row_test_record(b"selected", 50, 110),
        row_test_record(b"new", 120, 150),
        row_test_record(b"new-overlap", 120, 160),
    ]);
    let refined_rows = assign_alignment_rows(
        &refined_query,
        AlignmentLayoutOptions {
            max_rows: refined_query.batch.len(),
            ..AlignmentLayoutOptions::default()
        },
        Some(&broad_rows),
    );

    assert_eq!(refined_rows.rows[0], Some(1));

    let narrowed_query = row_test_query(&[row_test_record(b"new-overlap", 120, 160)]);
    let narrowed_rows = assign_alignment_rows(
        &narrowed_query,
        AlignmentLayoutOptions {
            // The desktop retains the largest uncapped capacity observed for
            // the row-history chain, even when the next query has fewer reads.
            max_rows: refined_query.batch.len(),
            ..AlignmentLayoutOptions::default()
        },
        Some(&refined_rows),
    );
    assert_eq!(narrowed_rows.rows[0], refined_rows.rows[2]);
}

#[test]
fn persistent_history_restores_broad_narrow_broad_rows() {
    let broad = row_test_query(&[
        row_test_record(b"leading", 0, 70),
        row_test_record(b"retained", 50, 110),
        row_test_record(b"trailing", 105, 150),
    ]);
    let narrow = row_test_query(&[row_test_record(b"retained", 50, 110)]);
    let options = AlignmentLayoutOptions {
        max_rows: broad.batch.len(),
        ..AlignmentLayoutOptions::default()
    };
    let mut history = AlignmentRowHistory::default();

    let (first, initial) = reconcile_alignment_rows(
        &broad,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let (middle, narrowed) = reconcile_alignment_rows(
        &narrow,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let (restored, returned) = reconcile_alignment_rows(
        &broad,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(middle.rows[0], first.rows[1]);
    assert_eq!(restored.rows, first.rows);
    assert_eq!(initial.added, 3);
    assert_eq!(narrowed.retained, 1);
    assert_eq!(narrowed.reservations, 2);
    assert_eq!(returned.restored, 2);
    assert_eq!(returned.moved, 0);
}

#[test]
fn canonical_horizon_makes_narrow_and_broad_entry_order_identical() {
    let horizon = row_test_query(&[
        row_test_record(b"long", 0, 100),
        row_test_record(b"short-a", 10, 20),
        row_test_record(b"short-b", 30, 40),
        row_test_record(b"middle", 60, 130),
        row_test_record(b"after", 110, 150),
    ]);
    let mut narrow = row_test_query(&[
        row_test_record(b"short-b", 30, 40),
        row_test_record(b"middle", 60, 130),
        row_test_record(b"after", 110, 150),
    ]);
    narrow.interval = GenomicInterval::new(ContigId::new(0), 25, 155).unwrap();
    let options = AlignmentLayoutOptions {
        max_rows: horizon.batch.len(),
        ..AlignmentLayoutOptions::default()
    };

    let mut narrow_first_history = AlignmentRowHistory::default();
    let (narrow_first, first_stats) = reconcile_alignment_rows_with_horizon(
        &narrow,
        &horizon,
        options,
        &mut narrow_first_history,
        ReadStackingMode::Stable,
        false,
    );

    let mut broad_first_history = AlignmentRowHistory::default();
    let (_, broad_stats) = reconcile_alignment_rows_with_horizon(
        &horizon,
        &horizon,
        options,
        &mut broad_first_history,
        ReadStackingMode::Stable,
        false,
    );
    let (narrow_after_broad, revisit_stats) = reconcile_alignment_rows_with_horizon(
        &narrow,
        &horizon,
        options,
        &mut broad_first_history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(narrow_first.rows, narrow_after_broad.rows);
    assert_eq!(
        first_stats.transition_reason,
        ReadStackingTransitionReason::InitialCanonical
    );
    assert_eq!(
        broad_stats.transition_reason,
        ReadStackingTransitionReason::InitialCanonical
    );
    assert_eq!(
        revisit_stats.transition_reason,
        ReadStackingTransitionReason::Navigation
    );
    assert_eq!(first_stats.horizon_generation, 1);
    assert_eq!(first_stats.horizon_extensions, 0);
    assert_eq!(first_stats.horizon_records, horizon.batch.len());
    assert_eq!(revisit_stats.moved, 0);
}

#[test]
fn complete_foreground_rows_survive_a_truncated_empty_horizon() {
    let mut visible = row_test_query(&[
        row_test_record(b"visible-a", 50, 90),
        row_test_record(b"visible-b", 60, 100),
    ]);
    visible.interval = GenomicInterval::new(ContigId::new(0), 40, 110).unwrap();
    let mut dense_horizon = row_test_query(&[]);
    dense_horizon.stats.records_eligible = 70_000;
    dense_horizon.stats.individual_reads =
        rgv_hts::IndividualReadStatus::TooDense { limit: 50_000 };
    let mut history = AlignmentRowHistory::default();

    let (layout, stats) = reconcile_alignment_rows_with_horizon(
        &visible,
        &dense_horizon,
        AlignmentLayoutOptions {
            max_rows: visible.batch.len(),
            ..AlignmentLayoutOptions::default()
        },
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(layout.rows, vec![Some(0), Some(1)]);
    assert_eq!(stats.horizon_records, 0);
    assert!(stats.horizon_truncated);
    assert_eq!(stats.assignments, visible.batch.len());
}

#[test]
fn dense_complete_foreground_seeds_empty_horizon_without_compatibility_scans() {
    const RECORDS: usize = 10_000;
    let records = (0..RECORDS)
        .map(|index| row_test_record(format!("dense-foreground-{index:05}").as_bytes(), 50, 100))
        .collect::<Vec<_>>();
    let visible = row_test_query(&records);
    let mut dense_horizon = row_test_query(&[]);
    dense_horizon.stats.records_eligible = 700_000;
    dense_horizon.stats.individual_reads =
        rgv_hts::IndividualReadStatus::TooDense { limit: 50_000 };
    let mut history = AlignmentRowHistory::default();

    let (layout, stats) = reconcile_alignment_rows_with_horizon(
        &visible,
        &dense_horizon,
        AlignmentLayoutOptions {
            max_rows: RECORDS,
            ..AlignmentLayoutOptions::default()
        },
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(layout.used_row_count(), RECORDS);
    assert_eq!(stats.assignments, RECORDS);
    assert_eq!(stats.added, RECORDS);
    assert_eq!(stats.rows_appended, RECORDS);
    assert_eq!(stats.rows_reused, 0);
    assert_eq!(stats.compatibility_probes, 0);
    assert!(stats.horizon_truncated);
}

#[test]
fn cancelled_dense_reconciliation_drops_the_candidate_history() {
    const RECORDS: usize = 10_000;
    let records = (0..RECORDS)
        .map(|index| row_test_record(format!("cancel-{index:05}").as_bytes(), 50, 100))
        .collect::<Vec<_>>();
    let query = row_test_query(&records);
    let committed = AlignmentRowHistory::default();
    let checks = std::sync::atomic::AtomicUsize::new(0);

    let result = reconcile_alignment_rows_with_horizon_cancellable(
        &query,
        &query,
        AlignmentLayoutOptions {
            max_rows: RECORDS,
            ..AlignmentLayoutOptions::default()
        },
        committed.clone(),
        ReadStackingMode::Stable,
        false,
        || checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 4,
    );

    assert!(result.is_none());
    assert!(checks.load(std::sync::atomic::Ordering::Relaxed) >= 5);
    assert_eq!(committed.revision(), 0);
    assert_eq!(committed.assignment_count(), 0);
}

#[test]
fn monotonic_history_extension_reuses_an_indexed_historical_gap() {
    let initial = row_test_query(&[
        row_test_record(b"left", 0, 10),
        row_test_record(b"right", 20, 30),
    ]);
    let extended = row_test_query(&[
        row_test_record(b"left", 0, 10),
        row_test_record(b"gap", 10, 20),
        row_test_record(b"right", 20, 30),
    ]);
    let mut history = AlignmentRowHistory::default();
    let options = AlignmentLayoutOptions {
        max_rows: extended.batch.len(),
        ..AlignmentLayoutOptions::default()
    };
    let (initial_layout, _) = reconcile_alignment_rows(
        &initial,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let (extended_layout, stats) = reconcile_alignment_rows(
        &extended,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(initial_layout.rows, vec![Some(0), Some(0)]);
    assert_eq!(extended_layout.rows, vec![Some(0), Some(0), Some(0)]);
    assert_eq!(stats.rows_reused, 1);
    assert_eq!(stats.rows_appended, 0);
    assert!(stats.compatibility_probes > 0);
}

#[test]
fn dense_history_extension_has_logarithmic_probe_growth() {
    const INCUMBENTS: usize = 2_048;
    const ADDED: usize = 4_096;
    let initial_records = (0..INCUMBENTS)
        .map(|index| row_test_record(format!("incumbent-{index:05}").as_bytes(), 20, 180))
        .collect::<Vec<_>>();
    let initial = row_test_query(&initial_records);
    let mut extended_records = initial_records;
    extended_records.extend(
        (0..ADDED).map(|index| row_test_record(format!("addition-{index:05}").as_bytes(), 20, 180)),
    );
    let extended = row_test_query(&extended_records);
    let mut history = AlignmentRowHistory::default();
    let options = AlignmentLayoutOptions {
        max_rows: extended.batch.len(),
        ..AlignmentLayoutOptions::default()
    };
    reconcile_alignment_rows(
        &initial,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let (layout, stats) = reconcile_alignment_rows(
        &extended,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(layout.used_row_count(), INCUMBENTS + ADDED);
    assert_eq!(stats.rows_appended, ADDED);
    assert_eq!(stats.rows_reused, 0);
    assert!(
        stats.compatibility_probes <= ADDED * 20,
        "{} probes for {ADDED} new dense reads",
        stats.compatibility_probes
    );
}

#[test]
fn rich_visible_payload_maps_to_layout_only_horizon_identities() {
    let layout_query = row_test_query(&[
        row_test_record(b"alpha", 10, 80),
        row_test_record(b"beta", 20, 90),
    ]);
    let mut alpha = row_test_record(b"alpha", 10, 80);
    alpha.sequence = SequenceData::Present {
        bases: vec![b'A'; 70],
        qualities: Some(vec![30; 70]),
    };
    alpha.auxiliary_fields.push(AuxField {
        tag: *b"ZZ",
        value: AuxValue::String("inspector-only".to_owned()),
    });
    let mut beta = row_test_record(b"beta", 20, 90);
    beta.auxiliary_fields.push(AuxField {
        tag: *b"XY",
        value: AuxValue::String("unrelated".to_owned()),
    });
    let visible_query = row_test_query(&[alpha, beta]);
    let mut history = AlignmentRowHistory::default();
    let (layout, stats) = reconcile_alignment_rows_with_horizon(
        &visible_query,
        &layout_query,
        AlignmentLayoutOptions::default(),
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(layout.rows, vec![Some(0), Some(1)]);
    assert_eq!(stats.added, layout_query.batch.len());
    assert_eq!(stats.assignments, layout_query.batch.len());
}

#[test]
fn horizon_extension_is_additive_and_typed() {
    let mut initial = row_test_query(&[
        row_test_record(b"incumbent-a", 10, 90),
        row_test_record(b"incumbent-b", 20, 100),
    ]);
    initial.interval = GenomicInterval::new(ContigId::new(0), 0, 200).unwrap();
    let mut extended = row_test_query(&[
        row_test_record(b"incumbent-a", 10, 90),
        row_test_record(b"incumbent-b", 20, 100),
        row_test_record(b"new-top-fit", 220, 260),
    ]);
    extended.interval = GenomicInterval::new(ContigId::new(0), 0, 400).unwrap();
    let options = AlignmentLayoutOptions {
        max_rows: extended.batch.len(),
        ..AlignmentLayoutOptions::default()
    };
    let mut history = AlignmentRowHistory::default();
    let (before, _) = reconcile_alignment_rows_with_horizon(
        &initial,
        &initial,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let (after, stats) = reconcile_alignment_rows_with_horizon(
        &extended,
        &extended,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(&after.rows[..2], &before.rows);
    assert_eq!(after.rows[2], Some(0));
    assert_eq!(
        stats.transition_reason,
        ReadStackingTransitionReason::HorizonExtension
    );
    assert_eq!(stats.horizon_generation, 2);
    assert_eq!(stats.horizon_extensions, 1);
    assert_eq!(stats.moved, 0);
}

#[test]
fn persistent_reservation_cannot_be_stolen_by_a_conflicting_new_read() {
    let broad = row_test_query(&[
        row_test_record(b"reserved", 10, 90),
        row_test_record(b"retained", 10, 90),
    ]);
    let changed = row_test_query(&[
        row_test_record(b"retained", 10, 90),
        row_test_record(b"new", 10, 90),
    ]);
    let options = AlignmentLayoutOptions {
        max_rows: 3,
        ..AlignmentLayoutOptions::default()
    };
    let mut history = AlignmentRowHistory::default();
    let (first, _) = reconcile_alignment_rows(
        &broad,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let (next, stats) = reconcile_alignment_rows(
        &changed,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(next.rows[0], first.rows[1]);
    assert_eq!(next.rows[1], Some(2));
    assert_eq!(stats.reservations, 1);
    assert_eq!(stats.added, 1);
}

#[test]
fn compact_projection_does_not_destroy_stable_history() {
    let broad = row_test_query(&[
        row_test_record(b"leading", 0, 70),
        row_test_record(b"retained", 50, 110),
    ]);
    let narrow = row_test_query(&[row_test_record(b"retained", 50, 110)]);
    let options = AlignmentLayoutOptions::default();
    let mut history = AlignmentRowHistory::default();
    let (stable, _) = reconcile_alignment_rows(
        &broad,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let revision = history.revision();
    let (compact, compact_stats) = reconcile_alignment_rows(
        &narrow,
        options,
        &mut history,
        ReadStackingMode::Compact,
        false,
    );
    let (restored, restored_stats) = reconcile_alignment_rows(
        &narrow,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(compact.rows, vec![Some(0)]);
    assert_eq!(history.revision(), revision + 1);
    assert_eq!(restored.rows[0], stable.rows[1]);
    assert!(compact_stats.compact);
    assert_eq!(
        restored_stats.transition_reason,
        ReadStackingTransitionReason::Navigation
    );
}

#[test]
fn one_shot_repack_establishes_a_new_stable_baseline() {
    let broad = row_test_query(&[
        row_test_record(b"leading", 0, 70),
        row_test_record(b"retained", 50, 110),
    ]);
    let narrow = row_test_query(&[row_test_record(b"retained", 50, 110)]);
    let options = AlignmentLayoutOptions::default();
    let mut history = AlignmentRowHistory::default();
    reconcile_alignment_rows(
        &broad,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let (repacked, stats) = reconcile_alignment_rows(
        &narrow,
        options,
        &mut history,
        ReadStackingMode::Stable,
        true,
    );
    let (again, _) = reconcile_alignment_rows(
        &narrow,
        options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(repacked.rows, vec![Some(0)]);
    assert_eq!(again.rows, repacked.rows);
    assert!(stats.repacked);
    assert_eq!(
        stats.transition_reason,
        ReadStackingTransitionReason::ExplicitRebuild
    );
}

#[test]
fn query_local_pair_bounds_do_not_change_ordinary_row_policy() {
    let query = row_test_query(&[
        row_test_record(b"first", 10, 80),
        row_test_record(b"second", 20, 90),
    ]);
    let broad_options = AlignmentLayoutOptions {
        pair_length_bounds: TemplateLengthBounds {
            minimum: Some(0),
            maximum: Some(526),
        },
        ..AlignmentLayoutOptions::default()
    };
    let narrow_options = AlignmentLayoutOptions {
        pair_length_bounds: TemplateLengthBounds {
            minimum: Some(0),
            maximum: Some(398),
        },
        ..broad_options
    };
    let mut history = AlignmentRowHistory::default();
    let (first, _) = reconcile_alignment_rows(
        &query,
        broad_options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let (second, stats) = reconcile_alignment_rows(
        &query,
        narrow_options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(history.context_count(), 1);
    assert_eq!(second.rows, first.rows);
    assert_eq!(stats.retained, 2);
    assert_eq!(
        stats.transition_reason,
        ReadStackingTransitionReason::Navigation
    );
}

#[test]
fn large_rna_zoom_reproducer_keeps_all_1262_shared_rows() {
    const BROAD_COUNT: usize = 3_655;
    const SHARED_COUNT: usize = 1_262;
    let leading_count = BROAD_COUNT - SHARED_COUNT;
    let mut broad_records = Vec::with_capacity(BROAD_COUNT);
    for index in 0..leading_count {
        broad_records.push(row_test_record(
            format!("leading-{index:04}").as_bytes(),
            10,
            190,
        ));
    }
    let mut narrow_records = Vec::with_capacity(SHARED_COUNT);
    for index in 0..SHARED_COUNT {
        let record = row_test_record(format!("shared-{index:04}").as_bytes(), 10, 190);
        broad_records.push(record.clone());
        narrow_records.push(record);
    }
    let broad = row_test_query(&broad_records);
    let narrow = row_test_query(&narrow_records);
    let broad_options = AlignmentLayoutOptions {
        max_rows: BROAD_COUNT,
        pair_length_bounds: TemplateLengthBounds {
            minimum: Some(0),
            maximum: Some(526),
        },
        ..AlignmentLayoutOptions::default()
    };
    let narrow_options = AlignmentLayoutOptions {
        max_rows: SHARED_COUNT,
        pair_length_bounds: TemplateLengthBounds {
            minimum: Some(0),
            maximum: Some(398),
        },
        ..broad_options
    };

    let scratch_broad = assign_alignment_rows(&broad, broad_options, None);
    let scratch_narrow = assign_alignment_rows(&narrow, narrow_options, None);
    let scratch_moved = (0..SHARED_COUNT)
        .filter(|index| scratch_broad.rows[leading_count + index] != scratch_narrow.rows[*index])
        .count();
    assert!(scratch_moved >= 1_250);

    let mut history = AlignmentRowHistory::default();
    let (stable_broad, _) = reconcile_alignment_rows(
        &broad,
        broad_options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let (stable_narrow, stats) = reconcile_alignment_rows(
        &narrow,
        narrow_options,
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    let stable_moved = (0..SHARED_COUNT)
        .filter(|index| stable_broad.rows[leading_count + index] != stable_narrow.rows[*index])
        .count();

    assert_eq!(stable_moved, 0);
    assert_eq!(stats.retained, SHARED_COUNT);
    assert_eq!(stats.moved, 0);
    assert_eq!(history.context_count(), 1);
}

#[test]
fn disjoint_loci_create_independent_stable_islands() {
    let mut left = row_test_query(&[row_test_record(b"left", 10, 30)]);
    left.interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
    let mut right = row_test_query(&[row_test_record(b"right", 410, 430)]);
    right.interval = GenomicInterval::new(ContigId::new(0), 400, 500).unwrap();
    let mut history = AlignmentRowHistory::default();
    reconcile_alignment_rows(
        &left,
        AlignmentLayoutOptions::default(),
        &mut history,
        ReadStackingMode::Stable,
        false,
    );
    reconcile_alignment_rows(
        &right,
        AlignmentLayoutOptions::default(),
        &mut history,
        ReadStackingMode::Stable,
        false,
    );

    assert_eq!(history.context_count(), 1);
    assert_eq!(history.island_count(), 2);
}

#[test]
fn overlapping_navigation_sequences_never_move_known_identities_or_collide() {
    let mut seed = 0x5eed_cafe_u64;
    let records = (0..256)
        .map(|index| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let start = 20 + seed % 1_760;
            let length = 20 + (seed >> 16) % 180;
            row_test_record(
                format!("property-{index:04}").as_bytes(),
                start,
                (start + length).min(1_990),
            )
        })
        .collect::<Vec<_>>();
    let intervals = [
        (0, 700),
        (250, 950),
        (500, 1_200),
        (750, 1_450),
        (500, 1_200),
        (250, 950),
        (0, 700),
        (600, 1_600),
        (900, 1_900),
        (600, 1_600),
    ];
    let mut history = AlignmentRowHistory::default();
    let mut known = HashMap::<AlignmentRowIdentity, usize>::new();

    for (step, (start, end)) in intervals.into_iter().enumerate() {
        let mut visible = records
            .iter()
            .filter(|record| record.start < end && start < record.end)
            .cloned()
            .collect::<Vec<_>>();
        if step % 2 == 1 {
            visible.reverse();
        }
        let mut query = row_test_query(&visible);
        query.interval = GenomicInterval::new(ContigId::new(0), start, end).unwrap();
        let (layout, stats) = reconcile_alignment_rows(
            &query,
            AlignmentLayoutOptions {
                max_rows: query.batch.len().max(1),
                ..AlignmentLayoutOptions::default()
            },
            &mut history,
            ReadStackingMode::Stable,
            false,
        );
        assert_eq!(stats.moved, 0);
        for index in 0..query.batch.len() {
            let identity = layout.identities[index];
            let row = layout.rows[index].unwrap();
            if let Some(previous) = known.insert(identity, row) {
                assert_eq!(row, previous, "identity moved at navigation step {step}");
            }
        }
        for left in 0..query.batch.len() {
            for right in left + 1..query.batch.len() {
                if layout.rows[left] == layout.rows[right]
                    && query.batch.starts[left] < query.batch.ends[right]
                    && query.batch.starts[right] < query.batch.ends[left]
                {
                    panic!("overlapping records shared a row at navigation step {step}");
                }
            }
        }
    }
}

#[test]
fn initial_stable_baseline_is_independent_of_batch_iteration_order() {
    let mut supplementary = row_test_record(b"repeated-name", 10, 90);
    supplementary.flags = AlignmentFlags::from_bits(0x800);
    let records = vec![
        row_test_record(b"zeta", 10, 90),
        supplementary,
        row_test_record(b"alpha", 10, 90),
    ];
    let forward = row_test_query(&records);
    let mut reversed_records = records;
    reversed_records.reverse();
    let reversed = row_test_query(&reversed_records);
    let mut forward_history = AlignmentRowHistory::default();
    let mut reversed_history = AlignmentRowHistory::default();
    let (forward_layout, _) = reconcile_alignment_rows(
        &forward,
        AlignmentLayoutOptions::default(),
        &mut forward_history,
        ReadStackingMode::Stable,
        false,
    );
    let (reversed_layout, _) = reconcile_alignment_rows(
        &reversed,
        AlignmentLayoutOptions::default(),
        &mut reversed_history,
        ReadStackingMode::Stable,
        false,
    );
    let forward_rows = forward_layout
        .identities
        .iter()
        .copied()
        .zip(forward_layout.rows.iter().copied())
        .collect::<BTreeMap<_, _>>();
    let reversed_rows = reversed_layout
        .identities
        .iter()
        .copied()
        .zip(reversed_layout.rows.iter().copied())
        .collect::<BTreeMap<_, _>>();

    assert_eq!(forward_rows, reversed_rows);
    assert_eq!(
        forward_layout.assignment_fingerprint(),
        reversed_layout.assignment_fingerprint()
    );
}

#[allow(clippy::too_many_lines)]
#[test]
fn layout_distinguishes_cigar_operations() {
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 125,
        flags: AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"complex".to_vec(),
        cigar: vec![
            CigarOp {
                kind: CigarOpKind::Match,
                len: 10,
            },
            CigarOp {
                kind: CigarOpKind::Insertion,
                len: 3,
            },
            CigarOp {
                kind: CigarOpKind::ReferenceSkip,
                len: 5,
            },
            CigarOp {
                kind: CigarOpKind::SequenceMismatch,
                len: 10,
            },
        ],
        sequence: SequenceData::NotLoaded,
        auxiliary_fields: Vec::new(),
    };
    let mut batch = AlignmentBatch::default();
    batch.push(&record).unwrap();
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 95, 130).unwrap(),
        700.0,
    )
    .unwrap();
    let query = AlignmentQuery {
        interval: viewport.interval(),
        header: Arc::default(),
        batch,
        pileup: PileupSummary::from_depths(vec![1; 100]),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats {
            records_seen: 1,
            records_eligible: 1,
            records_emitted: 1,
            ..QueryStats::default()
        },
    };
    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
    assert_eq!(layout.stats.rendered_alignments, 1);
    assert!(layout.display_list.rectangles.len() > query.pileup.len());
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .any(|primitive| primitive.object_id == 1)
    );
    assert!(layout.display_list.rectangles.iter().any(|primitive| {
        primitive.object_id & READ_DETAIL_OBJECT_BIT != 0
            && record_object_id(primitive.object_id) == 1
    }));
    assert!(layout.display_list.rectangles.iter().any(|primitive| {
        primitive.object_id == 1
            && (primitive.height - AlignmentLayoutOptions::default().read_height).abs()
                < f32::EPSILON
    }));
    assert!(
        layout
            .display_list
            .texts
            .iter()
            .any(|text| text.text == "I3" && text.color == insertion_event_color(1.0))
    );
    assert!(
        layout
            .display_list
            .texts
            .iter()
            .any(|text| text.text == "N5")
    );

    let filtered = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            minimum_indel_size: 4,
            minimum_event_label_size: 4,
            ..AlignmentLayoutOptions::default()
        },
    );
    assert!(
        !filtered
            .display_list
            .texts
            .iter()
            .any(|text| text.text == "I3")
    );
    assert!(
        filtered
            .display_list
            .texts
            .iter()
            .any(|text| text.text == "N5")
    );
    assert!(
        !filtered
            .display_list
            .rectangles
            .iter()
            .any(|primitive| { primitive.color == insertion_event_color(0.96) })
    );
}

#[test]
fn base_layout_compares_m_operations_with_reference() {
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 104,
        flags: AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"mismatch".to_vec(),
        cigar: vec![CigarOp {
            kind: CigarOpKind::Match,
            len: 4,
        }],
        sequence: SequenceData::Present {
            bases: b"ACGT".to_vec(),
            qualities: Some(vec![0, 40, 40, 40]),
        },
        auxiliary_fields: Vec::new(),
    };
    let mut batch = AlignmentBatch::default();
    batch.push(&record).unwrap();
    let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
    let viewport = Viewport::new(interval, 800.0).unwrap();
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch,
        pileup: PileupSummary::from_depths(vec![1; 4]),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: Some(ReferenceSlice {
            interval,
            bases: b"ACAT".to_vec(),
        }),
        stats: QueryStats {
            records_seen: 1,
            records_eligible: 1,
            records_emitted: 1,
            ..QueryStats::default()
        },
    };

    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
    assert_eq!(layout.stats.rendered_alignments, 1);
    assert!(layout.pinned_rectangle_count > 0);
    assert!(
        layout.display_list.rectangles[..layout.pinned_rectangle_count]
            .iter()
            .all(|primitive| primitive.object_id == 0)
    );
    assert!(
        layout.display_list.rectangles[layout.pinned_rectangle_count..]
            .iter()
            .any(|primitive| record_object_id(primitive.object_id) == 1)
    );
    assert_eq!(layout.pinned_text_count, 4);
    assert!(layout.display_list.texts.len() > layout.pinned_text_count);
    assert!(layout.display_list.rectangles.iter().any(|primitive| {
        record_object_id(primitive.object_id) == 1
            && primitive.object_id & READ_DETAIL_OBJECT_BIT != 0
            && primitive.color.red > 0.9
            && primitive.color.green > 0.5
    }));
    assert!(layout.display_list.rectangles.iter().any(|primitive| {
        primitive.object_id == 0 && (primitive.y - 64.0).abs() < f32::EPSILON
    }));
    let reference_label = layout
        .display_list
        .texts
        .iter()
        .min_by(|left, right| left.y.total_cmp(&right.y))
        .unwrap();
    assert!(reference_label.color.red < 0.9);
    assert!(reference_label.color.green < 0.9);
    assert!(reference_label.color.blue < 0.9);
    let mismatch_label = layout
        .display_list
        .texts
        .iter()
        .max_by(|left, right| left.y.total_cmp(&right.y))
        .unwrap();
    assert!(mismatch_label.color.red < 0.1);
    assert!(mismatch_label.color.green < 0.1);
    assert!(mismatch_label.color.blue < 0.15);
    assert!((mismatch_label.color.alpha - 1.0).abs() < f32::EPSILON);
}

#[test]
fn mismatch_marks_precede_base_letters_at_read_detail() {
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 104,
        flags: AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"mismatch-thresholds".to_vec(),
        cigar: vec![CigarOp {
            kind: CigarOpKind::Match,
            len: 4,
        }],
        sequence: SequenceData::Present {
            bases: b"ACGT".to_vec(),
            qualities: Some(vec![40; 4]),
        },
        auxiliary_fields: Vec::new(),
    };
    let mut batch = AlignmentBatch::default();
    batch.push(&record).unwrap();
    let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch,
        pileup: PileupSummary::from_depths(vec![1; 4]),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: Some(ReferenceSlice {
            interval,
            bases: b"ACAT".to_vec(),
        }),
        stats: QueryStats {
            records_seen: 1,
            records_eligible: 1,
            records_emitted: 1,
            ..QueryStats::default()
        },
    };
    let options = AlignmentLayoutOptions {
        show_reference: false,
        ..AlignmentLayoutOptions::default()
    };

    let wider = layout_alignment_query(&query, Viewport::new(interval, 3.0).unwrap(), options);
    assert!(!wider.display_list.rectangles.iter().any(|primitive| {
        primitive.object_id & READ_DETAIL_OBJECT_BIT != 0
            && record_object_id(primitive.object_id) == 1
    }));

    let marks = layout_alignment_query(&query, Viewport::new(interval, 4.0).unwrap(), options);
    assert!(marks.display_list.rectangles.iter().any(|primitive| {
        primitive.object_id & READ_DETAIL_OBJECT_BIT != 0
            && record_object_id(primitive.object_id) == 1
    }));
    assert!(marks.display_list.texts.is_empty());

    let letter_viewport = Viewport::new(interval, 28.0).unwrap();
    assert_eq!(letter_viewport.detail_level(), DetailLevel::Reads);
    let letters = layout_alignment_query(&query, letter_viewport, options);
    assert_eq!(letters.display_list.texts.len(), 1);
    assert_eq!(letters.display_list.texts[0].text, "G");
}

#[test]
fn contradictory_explicit_match_uses_the_observed_nucleotide_rendering() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 100, 101).unwrap();
    let record = AlignmentRecord {
        contig,
        start: 100,
        end: 101,
        flags: AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"contradictory-equal".to_vec(),
        cigar: vec![CigarOp {
            kind: CigarOpKind::SequenceMatch,
            len: 1,
        }],
        sequence: SequenceData::Present {
            bases: vec![b'C'],
            qualities: Some(vec![40]),
        },
        auxiliary_fields: Vec::new(),
    };
    let mut batch = AlignmentBatch::default();
    batch.push(&record).unwrap();
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch,
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: Some(ReferenceSlice {
            interval,
            bases: vec![b'T'],
        }),
        stats: QueryStats::default(),
    };
    let viewport = Viewport::new(interval, 28.0).unwrap();
    let rendered = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            show_reference: false,
            show_coverage: false,
            ..AlignmentLayoutOptions::default()
        },
    );

    assert!(
        rendered
            .display_list
            .texts
            .iter()
            .any(|text| text.text == "C")
    );
    assert!(
        rendered
            .display_list
            .texts
            .iter()
            .all(|text| text.text != "!"),
        "an encoding conflict must not replace the observed base"
    );
    let nucleotide = base_color(b'C');
    assert!(rendered.display_list.rectangles.iter().any(|rectangle| {
        rectangle.object_id & READ_DETAIL_OBJECT_BIT != 0
            && (rectangle.color.red - nucleotide.red).abs() < f32::EPSILON
            && (rectangle.color.green - nucleotide.green).abs() < f32::EPSILON
            && (rectangle.color.blue - nucleotide.blue).abs() < f32::EPSILON
    }));
}

#[test]
fn read_base_labels_contrast_with_each_nucleotide_fill() {
    for base in [b'A', b'G'] {
        let color = read_base_label_color(base);
        assert!(color.red < 0.1, "{base:?} should use dark text");
        assert!((color.alpha - 1.0).abs() < f32::EPSILON);
    }
    for base in [b'C', b'T'] {
        let color = read_base_label_color(base);
        assert!(color.red > 0.95, "{base:?} should use light text");
        assert!((color.alpha - 1.0).abs() < f32::EPSILON);
    }
}

#[allow(clippy::too_many_lines)]
#[test]
fn exact_match_fill_is_stable_across_the_base_detail_boundary() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 100, 104).unwrap();
    let viewport = Viewport::new(interval, 800.0).unwrap();
    let record = AlignmentRecord {
        contig,
        start: 100,
        end: 104,
        flags: AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"exact-match".to_vec(),
        cigar: vec![CigarOp {
            kind: CigarOpKind::SequenceMatch,
            len: 4,
        }],
        sequence: SequenceData::Present {
            bases: b"ACGT".to_vec(),
            qualities: Some(vec![0, 40, 40, 40]),
        },
        auxiliary_fields: Vec::new(),
    };
    let mut batch = AlignmentBatch::default();
    batch.push(&record).unwrap();
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch,
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: Some(ReferenceSlice {
            interval,
            bases: b"ACGT".to_vec(),
        }),
        stats: QueryStats::default(),
    };

    let mut reads = DisplayList::default();
    add_match_segment(
        &mut reads,
        &query,
        0,
        CigarOpKind::SequenceMatch,
        100,
        104,
        viewport,
        20.0,
        5.0,
        1,
        AlignmentColorMode::None,
        TemplateLengthBounds::default(),
    );
    let mut bases = DisplayList::default();
    let mut bisulfite_budget = BisulfiteRenderBudget::new(MAX_RENDERED_BISULFITE_MARKS);
    let mut smrt_kinetics_budget = SmrtKineticsRenderBudget::new(MAX_RENDERED_SMRT_KINETICS_MARKS);
    let mut sbx_budget = SbxRenderBudget::new(MAX_RENDERED_SBX_TAIL_MARKS);
    add_aligned_bases(
        &mut bases,
        &query,
        0,
        CigarOpKind::SequenceMatch,
        100,
        0,
        4,
        viewport,
        20.0,
        5.0,
        1,
        AlignmentColorMode::None,
        TemplateLengthBounds::default(),
        ReadBaseDisplayMode::Mismatches,
        true,
        &mut bisulfite_budget,
        None,
        &mut smrt_kinetics_budget,
        None,
        &mut sbx_budget,
    );

    assert_eq!(reads.rectangles.len(), 2);
    assert_eq!(bases.rectangles.len(), 2);
    assert_eq!(reads.rectangles[1].color, bases.rectangles[1].color);

    let mut all_bases = DisplayList::default();
    add_aligned_bases(
        &mut all_bases,
        &query,
        0,
        CigarOpKind::SequenceMatch,
        100,
        0,
        4,
        viewport,
        20.0,
        5.0,
        1,
        AlignmentColorMode::None,
        TemplateLengthBounds::default(),
        ReadBaseDisplayMode::All,
        true,
        &mut bisulfite_budget,
        None,
        &mut smrt_kinetics_budget,
        None,
        &mut sbx_budget,
    );
    assert_eq!(all_bases.rectangles.len(), 6);
    assert_eq!(all_bases.texts.len(), 4);
    assert!((all_bases.rectangles[2].color.alpha - 0.40).abs() < 0.01);

    let mut unshaded = DisplayList::default();
    add_aligned_bases(
        &mut unshaded,
        &query,
        0,
        CigarOpKind::SequenceMatch,
        100,
        0,
        4,
        viewport,
        20.0,
        5.0,
        1,
        AlignmentColorMode::None,
        TemplateLengthBounds::default(),
        ReadBaseDisplayMode::All,
        false,
        &mut bisulfite_budget,
        None,
        &mut smrt_kinetics_budget,
        None,
        &mut sbx_budget,
    );
    assert!((unshaded.rectangles[2].color.alpha - 0.98).abs() < 0.01);

    let mut hidden = DisplayList::default();
    add_aligned_bases(
        &mut hidden,
        &query,
        0,
        CigarOpKind::SequenceMatch,
        100,
        0,
        4,
        viewport,
        20.0,
        5.0,
        1,
        AlignmentColorMode::None,
        TemplateLengthBounds::default(),
        ReadBaseDisplayMode::Hidden,
        true,
        &mut bisulfite_budget,
        None,
        &mut smrt_kinetics_budget,
        None,
        &mut sbx_budget,
    );
    assert_eq!(hidden.rectangles.len(), 2);
    assert!(hidden.texts.is_empty());
}

#[test]
fn reverse_strand_bases_are_already_in_sam_alignment_order() {
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 0,
        end: 4,
        flags: AlignmentFlags::from_bits(0x10),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: Vec::new(),
        cigar: vec![CigarOp {
            kind: CigarOpKind::Match,
            len: 4,
        }],
        sequence: SequenceData::Present {
            bases: b"AACG".to_vec(),
            qualities: Some(vec![10, 20, 30, 40]),
        },
        auxiliary_fields: Vec::new(),
    };
    let SequenceData::Present { bases, qualities } = &record.sequence else {
        unreachable!();
    };
    assert_eq!(
        aligned_base(bases, qualities.as_deref(), 0),
        Some((b'A', Some(10)))
    );
    assert_eq!(
        aligned_base(bases, qualities.as_deref(), 3),
        Some((b'G', Some(40)))
    );

    let interval = GenomicInterval::new(ContigId::new(0), 0, 4).unwrap();
    let viewport = Viewport::new(interval, 40.0).unwrap();
    let mut batch = AlignmentBatch::default();
    batch.push(&record).unwrap();
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch,
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: Some(ReferenceSlice {
            interval,
            bases: b"AACG".to_vec(),
        }),
        stats: QueryStats::default(),
    };
    let mut display = DisplayList::default();
    let mut bisulfite_budget = BisulfiteRenderBudget::new(MAX_RENDERED_BISULFITE_MARKS);
    let mut smrt_kinetics_budget = SmrtKineticsRenderBudget::new(MAX_RENDERED_SMRT_KINETICS_MARKS);
    let mut sbx_budget = SbxRenderBudget::new(MAX_RENDERED_SBX_TAIL_MARKS);
    add_aligned_bases(
        &mut display,
        &query,
        0,
        CigarOpKind::Match,
        0,
        0,
        4,
        viewport,
        20.0,
        5.0,
        1,
        AlignmentColorMode::None,
        TemplateLengthBounds::default(),
        ReadBaseDisplayMode::Mismatches,
        true,
        &mut bisulfite_budget,
        None,
        &mut smrt_kinetics_budget,
        None,
        &mut sbx_budget,
    );
    assert_eq!(display.rectangles.len(), 2);
    assert!(display.texts.is_empty());
}

#[test]
fn prefetched_coverage_is_clipped_and_scaled_to_visible_bins() {
    let contig = ContigId::new(0);
    let query_interval = GenomicInterval::new(contig, 0, 1_000).unwrap();
    let viewport = Viewport::new(GenomicInterval::new(contig, 400, 600).unwrap(), 1_000.0).unwrap();
    let mut display_list = DisplayList::default();
    let maximum = add_coverage(
        &mut display_list,
        &PileupSummary::from_depths([1; 10]),
        None,
        query_interval,
        viewport,
        1_000.0,
        &AlignmentLayoutOptions::default(),
    );
    assert_eq!(maximum, 1);
    assert_eq!(display_list.rectangles.len(), 2);
    assert!(display_list.rectangles[0].x.abs() < f32::EPSILON);
    assert!((display_list.rectangles[1].x - 500.0).abs() < f32::EPSILON);
}

#[test]
fn fixed_coverage_scale_is_applied_and_clipped_without_changing_reported_depth() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 0, 1).unwrap();
    let viewport = Viewport::new(interval, 100.0).unwrap();
    let pileup = PileupSummary::from_depths([10]);
    let options = AlignmentLayoutOptions {
        coverage_height: 60.0,
        coverage_scale_maximum: Some(40),
        ..AlignmentLayoutOptions::default()
    };
    let mut display = DisplayList::default();
    let maximum = add_coverage(
        &mut display,
        &pileup,
        None,
        interval,
        viewport,
        100.0,
        &options,
    );
    assert_eq!(maximum, 10);
    assert_eq!(display.rectangles.len(), 1);
    assert!((display.rectangles[0].height - 15.0).abs() < 0.01);

    display.rectangles.clear();
    let square_root_options = AlignmentLayoutOptions {
        coverage_scale_transform: CoverageScaleTransform::SquareRoot,
        ..options
    };
    let maximum = add_coverage(
        &mut display,
        &pileup,
        None,
        interval,
        viewport,
        100.0,
        &square_root_options,
    );
    assert_eq!(maximum, 10);
    assert!((display.rectangles[0].height - 30.0).abs() < 0.01);

    display.rectangles.clear();
    let maximum = add_coverage(
        &mut display,
        &PileupSummary::from_depths([80]),
        None,
        interval,
        viewport,
        100.0,
        &options,
    );
    assert_eq!(maximum, 80);
    assert!((display.rectangles[0].height - 60.0).abs() < 0.01);
}

#[test]
fn hidden_alignment_subtracks_emit_no_primitives_but_keep_coverage_provenance() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 0, 200).unwrap();
    let viewport = Viewport::new(interval, 1_000.0).unwrap();
    let mut query = row_test_query(&[row_test_record(b"read", 10, 40)]);
    query.pileup = PileupSummary::from_depths([7]);
    query.reference = Some(ReferenceSlice {
        interval,
        bases: vec![b'A'; 200],
    });
    let options = AlignmentLayoutOptions {
        show_reference: false,
        show_coverage: false,
        show_reads: false,
        show_modification_frequency: false,
        ..AlignmentLayoutOptions::default()
    };
    let layout = layout_alignment_query(&query, viewport, options);
    assert!(layout.display_list.rectangles.is_empty());
    assert!(layout.display_list.texts.is_empty());
    assert_eq!(layout.stats.maximum_coverage, 7);
    assert_eq!(layout.stats.rendered_alignments, 0);
    assert_eq!(layout.stats.hidden_alignments, 1);
}

#[test]
fn junction_lane_renders_bounded_strand_aware_full_evidence() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 0, 200).unwrap();
    let viewport = Viewport::new(interval, 1_000.0).unwrap();
    let mut query = row_test_query(&[]);
    query.junctions = JunctionSummary {
        junctions: vec![
            JunctionEvidence {
                start: 20,
                end: 80,
                depth: 4,
                positive_depth: 4,
                ..JunctionEvidence::default()
            },
            JunctionEvidence {
                start: 100,
                end: 180,
                depth: 9,
                negative_depth: 9,
                ..JunctionEvidence::default()
            },
        ],
        provenance: rgv_hts::JunctionProvenance {
            eligible_records: 13,
            spliced_records: 13,
            all_eligible_records: true,
            ..rgv_hts::JunctionProvenance::default()
        },
    };
    let options = AlignmentLayoutOptions {
        show_reference: false,
        show_coverage: false,
        show_junctions: true,
        show_reads: false,
        show_modification_frequency: false,
        junction_top: 10.0,
        junction_height: 30.0,
        ..AlignmentLayoutOptions::default()
    };
    let layout = layout_alignment_query(&query, viewport, options);
    assert_eq!(layout.display_list.rectangles.len(), 5);
    assert_eq!(layout.pinned_rectangle_count, 5);
    assert_eq!(layout.stats.rendered_junctions, 2);
    assert_eq!(layout.stats.hidden_junctions, 0);
    assert_eq!(layout.stats.maximum_junction_depth, 9);
    assert_eq!(layout.junction_arc_targets.len(), 2);
    for target in &layout.junction_arc_targets {
        assert_eq!(
            layout.display_list.rectangles[target.draw_order as usize], target.primitive,
            "interaction geometry must be the exact emitted arc primitive"
        );
    }
    assert_eq!(
        layout
            .display_list
            .rectangles
            .iter()
            .filter(|rectangle| rectangle.shape == RectShape::Rectangle)
            .count(),
        1,
        "count labels must not add opaque backing rectangles"
    );
    let positive_arc = layout
        .display_list
        .rectangles
        .iter()
        .find(|rectangle| rectangle.shape == RectShape::JunctionArcUp)
        .unwrap();
    let positive_label = layout
        .display_list
        .texts
        .iter()
        .find(|text| text.text == "4")
        .unwrap();
    assert!(positive_label.y + positive_label.font_size * 0.5 <= positive_arc.y + 0.01);
    let negative_arc = layout
        .display_list
        .rectangles
        .iter()
        .find(|rectangle| rectangle.shape == RectShape::JunctionArcDown)
        .unwrap();
    let negative_label = layout
        .display_list
        .texts
        .iter()
        .find(|text| text.text == "9")
        .unwrap();
    assert!(
        negative_label.y - negative_label.font_size * 0.5
            >= negative_arc.y + negative_arc.height - 0.01
    );
    assert_eq!(
        layout
            .display_list
            .rectangles
            .iter()
            .find(|rectangle| rectangle.shape == RectShape::JunctionArcUp)
            .unwrap()
            .color,
        Rgba::new(0.84, 0.30, 0.38, 0.82)
    );
    assert_eq!(
        layout
            .display_list
            .rectangles
            .iter()
            .find(|rectangle| rectangle.shape == RectShape::JunctionArcDown)
            .unwrap()
            .color,
        Rgba::new(0.25, 0.45, 0.82, 0.82)
    );
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::ChevronRight)
    );
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::ChevronLeft)
    );
    assert_eq!(query.junctions.provenance.eligible_records, 13);

    let filtered = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            minimum_junction_depth: 5,
            ..options
        },
    );
    assert_eq!(filtered.display_list.rectangles.len(), 3);
    assert_eq!(filtered.stats.rendered_junctions, 1);
    assert_eq!(filtered.stats.hidden_junctions, 1);
    assert_eq!(filtered.junction_arc_targets.len(), 1);
    assert_eq!(
        filtered.junction_arc_targets[0].component,
        JunctionArcComponent::Negative
    );
    assert_eq!(
        filtered.junction_arc_targets[0].primitive.color,
        Rgba::new(0.25, 0.45, 0.82, 0.82),
        "selection must preserve the component's biological strand colour"
    );
}

#[test]
fn sashimi_targets_match_only_rendered_components_and_are_deterministic() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 200).unwrap();
    let viewport = Viewport::new(interval, 800.0).unwrap();
    let mut query = row_test_query(&[]);
    query.junctions = JunctionSummary {
        junctions: vec![
            JunctionEvidence {
                start: 10,
                end: 110,
                depth: 7,
                positive_depth: 4,
                negative_depth: 2,
                unknown_depth: 1,
                ..JunctionEvidence::default()
            },
            JunctionEvidence {
                start: 30,
                end: 130,
                depth: 1,
                positive_depth: 1,
                ..JunctionEvidence::default()
            },
        ],
        ..JunctionSummary::default()
    };
    let options = AlignmentLayoutOptions {
        show_reference: false,
        show_coverage: false,
        show_reads: false,
        show_modification_frequency: false,
        minimum_junction_depth: 2,
        junction_top: 10.0,
        junction_height: 40.0,
        ..AlignmentLayoutOptions::default()
    };

    let first = layout_alignment_query(&query, viewport, options);
    let second = layout_alignment_query(&query, viewport, options);
    assert_eq!(first.junction_arc_targets, second.junction_arc_targets);
    assert_eq!(first.junction_arc_targets.len(), 3);
    assert_eq!(
        first
            .junction_arc_targets
            .iter()
            .map(|target| target.component)
            .collect::<Vec<_>>(),
        vec![
            JunctionArcComponent::Positive,
            JunctionArcComponent::Negative,
            JunctionArcComponent::Unknown,
        ]
    );
    assert!(first.junction_arc_targets.iter().all(|target| {
        target.junction_index == 0
            && target.start == 10
            && target.end == 110
            && first.display_list.rectangles[target.draw_order as usize] == target.primitive
    }));

    let expanded = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            junction_display_mode: JunctionDisplayMode::Expanded,
            ..options
        },
    );
    assert_eq!(
        first
            .junction_arc_targets
            .iter()
            .map(|target| (target.start, target.end, target.component))
            .collect::<Vec<_>>(),
        expanded
            .junction_arc_targets
            .iter()
            .map(|target| (target.start, target.end, target.component))
            .collect::<Vec<_>>()
    );
    assert!(expanded.junction_arc_targets.iter().all(|target| {
        expanded.display_list.rectangles[target.draw_order as usize] == target.primitive
    }));
}

#[test]
fn sashimi_targets_obey_the_rendered_junction_cap() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 200).unwrap();
    let viewport = Viewport::new(interval, 800.0).unwrap();
    let mut query = row_test_query(&[]);
    query.junctions.junctions = (0..=MAX_RENDERED_JUNCTIONS)
        .map(|index| JunctionEvidence {
            start: 10,
            end: 190,
            depth: u32::try_from(index + 1).unwrap(),
            positive_depth: u32::try_from(index + 1).unwrap(),
            ..JunctionEvidence::default()
        })
        .collect();
    let layout = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            show_reference: false,
            show_coverage: false,
            show_reads: false,
            show_modification_frequency: false,
            show_junction_labels: false,
            ..AlignmentLayoutOptions::default()
        },
    );

    assert_eq!(layout.stats.rendered_junctions, MAX_RENDERED_JUNCTIONS);
    assert_eq!(layout.stats.hidden_junctions, 1);
    assert_eq!(layout.junction_arc_targets.len(), MAX_RENDERED_JUNCTIONS);
    assert!(
        layout
            .junction_arc_targets
            .iter()
            .all(|target| target.junction_index != 0),
        "the capped-out junction must not acquire an interaction target"
    );
}

#[test]
fn sashimi_layout_applies_exact_cpm_fixed_scale_and_expanded_rows() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 0, 200).unwrap();
    let viewport = Viewport::new(interval, 1_000.0).unwrap();
    let mut query = row_test_query(&[]);
    query.junctions = JunctionSummary {
        junctions: vec![
            JunctionEvidence {
                start: 20,
                end: 120,
                depth: 4,
                positive_depth: 4,
                ..JunctionEvidence::default()
            },
            JunctionEvidence {
                start: 40,
                end: 160,
                depth: 16,
                negative_depth: 16,
                ..JunctionEvidence::default()
            },
        ],
        provenance: rgv_hts::JunctionProvenance {
            normalization_mapped_records: Some(2_000_000),
            all_eligible_records: true,
            ..rgv_hts::JunctionProvenance::default()
        },
    };
    assert!(
        (junction_count_value(
            &query.junctions,
            &query.junctions.junctions[0],
            SashimiCountMode::CountsPerMillion
        ) - 2.0)
            .abs()
            < f64::EPSILON
    );

    let options = AlignmentLayoutOptions {
        show_reference: false,
        show_coverage: false,
        show_reads: false,
        show_modification_frequency: false,
        junction_top: 10.0,
        junction_height: 80.0,
        junction_display_mode: JunctionDisplayMode::Expanded,
        junction_count_mode: SashimiCountMode::CountsPerMillion,
        junction_scale_maximum: Some(8.0),
        ..AlignmentLayoutOptions::default()
    };
    let layout = layout_alignment_query(&query, viewport, options);
    assert_eq!(layout.stats.rendered_junctions, 2);
    assert!((layout.stats.maximum_junction_value - 8.0).abs() < f64::EPSILON);
    assert!(layout.stats.junction_normalization_available);
    let arc_tops = layout
        .display_list
        .rectangles
        .iter()
        .filter(|rectangle| rectangle.shape.is_junction_arc())
        .map(|rectangle| rectangle.y)
        .collect::<Vec<_>>();
    assert_eq!(arc_tops.len(), 2);
    assert!((arc_tops[0] - arc_tops[1]).abs() > f32::EPSILON);
    assert_eq!(layout.display_list.texts.len(), 2);
    assert_eq!(layout.display_list.texts[0].text, "2.0 CPM");
    assert_eq!(layout.display_list.texts[1].text, "8.0 CPM");

    let annotations = JunctionAnnotationLookup::new([(20, 120)], 1, true);
    assert_eq!(
        annotations.status(20, 120),
        JunctionAnnotationStatus::Annotated
    );
    assert_eq!(annotations.status(40, 160), JunctionAnnotationStatus::Novel);
    let rows = assign_alignment_rows(&query, options, None);
    let annotated_layout = layout_alignment_query_with_rows_and_annotations(
        &query,
        viewport,
        options,
        &rows,
        Some(&annotations),
    );
    assert_eq!(annotated_layout.display_list.rectangles.len(), 8);
    assert!(
        annotated_layout
            .display_list
            .rectangles
            .iter()
            .any(|rectangle| rectangle.color == Rgba::new(0.16, 0.58, 0.28, 1.0))
    );
    assert!(
        annotated_layout
            .display_list
            .rectangles
            .iter()
            .any(|rectangle| rectangle.color == Rgba::new(0.92, 0.48, 0.10, 1.0))
    );

    let raw_fallback = JunctionSummary {
        provenance: rgv_hts::JunctionProvenance::default(),
        ..query.junctions.clone()
    };
    assert!(
        (junction_count_value(
            &raw_fallback,
            &raw_fallback.junctions[0],
            SashimiCountMode::CountsPerMillion,
        ) - 4.0)
            .abs()
            < f64::EPSILON
    );
}

#[test]
fn high_density_sashimi_output_has_hard_arc_and_label_caps() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 0, 20_000).unwrap();
    let viewport = Viewport::new(interval, 1_000.0).unwrap();
    let summary = JunctionSummary {
        junctions: (0..5_000_u64)
            .map(|index| JunctionEvidence {
                start: index,
                end: 10_000 + index,
                depth: u32::try_from(index + 1).unwrap(),
                positive_depth: 1,
                ..JunctionEvidence::default()
            })
            .collect(),
        ..JunctionSummary::default()
    };
    let layout = layout_sashimi(
        &summary,
        viewport,
        AlignmentLayoutOptions {
            show_junctions: true,
            junction_top: 0.0,
            junction_height: 100.0,
            ..AlignmentLayoutOptions::default()
        },
        None,
    );
    assert_eq!(layout.stats.rendered_junctions, MAX_RENDERED_JUNCTIONS);
    assert_eq!(
        layout.stats.hidden_junctions,
        5_000 - MAX_RENDERED_JUNCTIONS
    );
    assert_eq!(
        layout.display_list.rectangles.len(),
        1 + MAX_RENDERED_JUNCTIONS * 2
    );
    assert!(!layout.display_list.texts.is_empty());
    assert!(layout.display_list.texts.len() <= MAX_RENDERED_JUNCTION_LABELS);
}

#[test]
fn collapsed_sashimi_suppresses_colliding_count_labels() {
    let summary = JunctionSummary {
        junctions: vec![
            JunctionEvidence {
                start: 20,
                end: 80,
                depth: 4,
                positive_depth: 4,
                ..JunctionEvidence::default()
            },
            JunctionEvidence {
                start: 20,
                end: 81,
                depth: 1,
                positive_depth: 1,
                ..JunctionEvidence::default()
            },
        ],
        ..JunctionSummary::default()
    };
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
        500.0,
    )
    .unwrap();
    let layout = layout_sashimi(
        &summary,
        viewport,
        AlignmentLayoutOptions {
            show_junctions: true,
            junction_height: 52.0,
            ..AlignmentLayoutOptions::default()
        },
        None,
    );
    assert_eq!(layout.stats.rendered_junctions, 2);
    assert_eq!(layout.display_list.texts.len(), 1);
}

#[test]
fn mixed_junctions_render_both_directions_and_unknown_junctions_remain_neutral() {
    let summary = JunctionSummary {
        junctions: vec![
            JunctionEvidence {
                start: 20,
                end: 80,
                depth: 7,
                positive_depth: 4,
                negative_depth: 3,
                ..JunctionEvidence::default()
            },
            JunctionEvidence {
                start: 100,
                end: 180,
                depth: 5,
                unknown_depth: 5,
                ..JunctionEvidence::default()
            },
        ],
        ..JunctionSummary::default()
    };
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 0, 200).unwrap(),
        1_000.0,
    )
    .unwrap();
    let layout = layout_sashimi(
        &summary,
        viewport,
        AlignmentLayoutOptions {
            show_junctions: true,
            junction_top: 0.0,
            junction_height: 100.0,
            show_junction_labels: false,
            ..AlignmentLayoutOptions::default()
        },
        None,
    );
    assert_eq!(
        layout
            .display_list
            .rectangles
            .iter()
            .filter(|rectangle| rectangle.shape == RectShape::ChevronLeft)
            .count(),
        1
    );
    assert_eq!(
        layout
            .display_list
            .rectangles
            .iter()
            .filter(|rectangle| rectangle.shape == RectShape::ChevronRight)
            .count(),
        1
    );
}

#[test]
fn coverage_colours_only_reference_discordant_support_above_threshold() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 100, 101).unwrap();
    let viewport = Viewport::new(interval, 100.0).unwrap();
    let pileup = PileupSummary {
        bins: vec![PileupBin {
            depth: 10,
            alleles: AlleleSupport {
                a: 8,
                c: 2,
                ..AlleleSupport::default()
            },
            ..PileupBin::default()
        }],
        ..PileupSummary::default()
    };
    let reference = ReferenceSlice {
        interval,
        bases: b"A".to_vec(),
    };
    let mut display = DisplayList::default();
    let options = AlignmentLayoutOptions {
        minimum_allele_fraction: 0.20,
        quality_weighted_alleles: false,
        ..AlignmentLayoutOptions::default()
    };
    add_coverage(
        &mut display,
        &pileup,
        Some(&reference),
        interval,
        viewport,
        100.0,
        &options,
    );
    assert_eq!(display.rectangles.len(), 2);
    assert_eq!(display.rectangles[1].color, base_color(b'C'));
    assert!((display.rectangles[1].height - options.coverage_height * 0.2).abs() < 0.01);

    display.rectangles.clear();
    add_coverage(
        &mut display,
        &pileup,
        Some(&reference),
        interval,
        viewport,
        100.0,
        &AlignmentLayoutOptions {
            minimum_allele_fraction: 0.21,
            ..options
        },
    );
    assert_eq!(display.rectangles.len(), 1);
}

#[test]
fn bisulfite_coverage_uses_context_and_template_strand_instead_of_base_palette() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 100, 102).unwrap();
    let viewport = Viewport::new(interval, 200.0).unwrap();
    let pileup = PileupSummary {
        bins: vec![
            PileupBin {
                depth: 4,
                alleles: AlleleSupport {
                    a: 1,
                    c: 2,
                    t: 1,
                    ..AlleleSupport::default()
                },
                bisulfite_forward_alleles: AlleleSupport {
                    c: 2,
                    t: 1,
                    ..AlleleSupport::default()
                },
                ..PileupBin::default()
            },
            PileupBin {
                depth: 4,
                alleles: AlleleSupport {
                    a: 2,
                    c: 1,
                    g: 1,
                    ..AlleleSupport::default()
                },
                bisulfite_reverse_alleles: AlleleSupport {
                    a: 2,
                    g: 1,
                    ..AlleleSupport::default()
                },
                ..PileupBin::default()
            },
        ],
        ..PileupSummary::default()
    };
    let reference = ReferenceSlice {
        interval,
        bases: b"CG".to_vec(),
    };
    let options = AlignmentLayoutOptions {
        coverage_height: 40.0,
        color_mode: AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
        quality_weighted_alleles: false,
        ..AlignmentLayoutOptions::default()
    };
    let mut display = DisplayList::default();

    add_coverage(
        &mut display,
        &pileup,
        Some(&reference),
        interval,
        viewport,
        200.0,
        &options,
    );

    assert_eq!(display.rectangles.len(), 6);
    assert_eq!(
        display
            .rectangles
            .iter()
            .filter(|rectangle| rectangle.color == bisulfite_color(BisulfiteObservation::Protected))
            .count(),
        2
    );
    assert_eq!(
        display
            .rectangles
            .iter()
            .filter(|rectangle| rectangle.color == bisulfite_color(BisulfiteObservation::Converted))
            .count(),
        2
    );
    assert!(!display.rectangles.iter().any(|rectangle| {
        [b'A', b'C', b'G', b'T']
            .into_iter()
            .any(|base| rectangle.color == base_color(base))
    }));
}

#[test]
fn standalone_reference_layout_uses_the_alignment_base_lane_semantics() {
    let interval = GenomicInterval::new(ContigId::new(0), 10, 14).unwrap();
    let viewport = Viewport::new(interval, 40.0).unwrap();
    let reference = ReferenceSlice {
        interval,
        bases: b"ACGT".to_vec(),
    };

    let display = layout_reference_slice(&reference, viewport, 25.0);

    assert_eq!(display.rectangles.len(), 4);
    assert_eq!(display.texts.len(), 4);
    assert!(
        display
            .rectangles
            .iter()
            .all(|base| (base.y - 25.0).abs() < f32::EPSILON)
    );
    assert_eq!(
        display
            .texts
            .iter()
            .map(|label| label.text.as_str())
            .collect::<Vec<_>>(),
        ["A", "C", "G", "T"]
    );
    let reverse = layout_reference_slice_oriented(&reference, viewport, 25.0, true);
    assert_eq!(
        reverse
            .texts
            .iter()
            .map(|label| label.text.as_str())
            .collect::<Vec<_>>(),
        ["T", "G", "C", "A"]
    );
}

#[test]
fn reference_translation_supports_both_strands_and_explicit_genetic_codes() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 3).unwrap();
    let viewport = Viewport::new(interval, 30.0).unwrap();
    let forward = ReferenceSlice {
        interval,
        bases: b"ATG".to_vec(),
    };
    let reverse = ReferenceSlice {
        interval,
        bases: b"CAT".to_vec(),
    };
    assert_eq!(
        layout_reference_translation(
            &forward,
            viewport,
            0.0,
            ReferenceTranslationFrame::Forward1,
            GeneticCode::Standard,
        )
        .texts[0]
            .text,
        "M"
    );
    assert_eq!(
        layout_reference_translation(
            &reverse,
            viewport,
            0.0,
            ReferenceTranslationFrame::Reverse1,
            GeneticCode::Standard,
        )
        .texts[0]
            .text,
        "M"
    );
    let mitochondrial = ReferenceSlice {
        interval,
        bases: b"TGA".to_vec(),
    };
    assert_eq!(
        layout_reference_translation(
            &mitochondrial,
            viewport,
            0.0,
            ReferenceTranslationFrame::Forward1,
            GeneticCode::VertebrateMitochondrial,
        )
        .texts[0]
            .text,
        "W"
    );
}

#[test]
fn deep_overlapping_reads_extend_beyond_the_old_ninety_row_cap() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 0, 1_000).unwrap();
    let viewport = Viewport::new(interval, 1_000.0).unwrap();
    let mut batch = AlignmentBatch::default();
    for index in 0..120 {
        batch
            .push(&AlignmentRecord {
                contig,
                start: 100,
                end: 200,
                flags: AlignmentFlags::from_bits(0),
                mapping_quality: Some(60),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: format!("read-{index}").into_bytes(),
                cigar: vec![CigarOp {
                    kind: CigarOpKind::Match,
                    len: 100,
                }],
                sequence: SequenceData::NotLoaded,
                auxiliary_fields: Vec::new(),
            })
            .unwrap();
    }
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch,
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats {
            records_seen: 120,
            records_eligible: 120,
            records_emitted: 120,
            ..QueryStats::default()
        },
    };
    let options = AlignmentLayoutOptions {
        max_rows: 128,
        ..AlignmentLayoutOptions::default()
    };

    let layout = layout_alignment_query(&query, viewport, options);

    assert_eq!(layout.stats.rendered_alignments, 120);
    assert_eq!(layout.stats.hidden_alignments, 0);
    let bottom = layout
        .display_list
        .rectangles
        .iter()
        .map(|rectangle| rectangle.y + rectangle.height)
        .fold(0.0_f32, f32::max);
    assert!(bottom > options.reads_top + 90.0 * options.row_height);

    let virtualized = layout_alignment_query(
        &query,
        viewport,
        AlignmentLayoutOptions {
            materialized_row_range: Some((40, 50)),
            ..options
        },
    );
    assert_eq!(virtualized.stats.rendered_alignments, 10);
    assert_eq!(virtualized.stats.virtualized_alignments, 110);
    assert_eq!(virtualized.stats.hidden_alignments, 110);
    assert_eq!(virtualized.stats.packed_row_count, 120);
    assert_eq!(virtualized.stats.materialized_row_start, 40);
    assert_eq!(virtualized.stats.materialized_row_end, 50);
    assert!(virtualized.display_list.rectangles.len() < layout.display_list.rectangles.len() / 4);
}

#[test]
fn regional_spans_reuse_rows_after_preceding_alignments_end() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 0, 1_000).unwrap();
    let viewport = Viewport::new(interval, 50.0).unwrap();
    assert_eq!(viewport.detail_level(), DetailLevel::Regional);
    let mut batch = AlignmentBatch::default();
    for (index, (start, end)) in [(100, 200), (110, 210), (220, 320), (230, 330)]
        .into_iter()
        .enumerate()
    {
        batch
            .push(&AlignmentRecord {
                contig,
                start,
                end,
                flags: AlignmentFlags::from_bits(0),
                mapping_quality: Some(60),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: format!("regional-{index}").into_bytes(),
                cigar: vec![CigarOp {
                    kind: CigarOpKind::Match,
                    len: u32::try_from(end - start).unwrap(),
                }],
                sequence: SequenceData::NotLoaded,
                auxiliary_fields: Vec::new(),
            })
            .unwrap();
    }
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch,
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats {
            records_seen: 4,
            records_eligible: 4,
            records_emitted: 4,
            ..QueryStats::default()
        },
    };

    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());

    assert_eq!(layout.stats.rendered_alignments, 4);
    assert_eq!(layout.stats.hidden_alignments, 0);
    let rows = layout
        .display_list
        .rectangles
        .iter()
        .filter(|rectangle| {
            rectangle.object_id != 0
                && (rectangle.height - AlignmentLayoutOptions::default().read_height).abs()
                    < f32::EPSILON
        })
        .map(|rectangle| rectangle.y.to_bits())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(rows.len(), 2);
    assert!(
        layout
            .display_list
            .rectangles
            .iter()
            .filter(|rectangle| rectangle.object_id != 0)
            .all(|rectangle| rectangle.width >= 1.0)
    );
}

#[test]
fn canonical_start_packing_reuses_the_topmost_free_row() {
    let query = row_test_query(&[
        row_test_record(b"long", 0, 100),
        row_test_record(b"short-one", 10, 20),
        row_test_record(b"short-two", 30, 40),
        row_test_record(b"after-all", 110, 120),
    ]);
    let rows = assign_alignment_rows(
        &query,
        AlignmentLayoutOptions {
            max_rows: query.batch.len(),
            ..AlignmentLayoutOptions::default()
        },
        None,
    );

    assert_eq!(rows.row(0), Some(0));
    assert_eq!(rows.row(1), Some(1));
    assert_eq!(rows.row(2), Some(1));
    assert_eq!(
        rows.row(3),
        Some(0),
        "when every lane is free, canonical packing must choose the topmost lane"
    );
    assert_eq!(rows.used_row_count(), 2);
    let metrics = alignment_row_organization_metrics(
        &query,
        AlignmentLayoutOptions {
            max_rows: query.batch.len(),
            ..AlignmentLayoutOptions::default()
        },
        &rows,
    );
    assert_eq!(metrics.architecture_version, 3);
    assert_eq!(metrics.canonical_rows, 2);
    assert_eq!(metrics.stable_rows, 2);
    assert_eq!(metrics.row_inflation, 0);
    assert_eq!(metrics.canonical_displacement, 0);
    assert_eq!(metrics.top_first_violations, 0);
}

#[test]
fn canonical_start_height_equals_maximum_concurrent_span_depth() {
    let mut seed = 0x7f4a_7c15_d1ce_ba5e_u64;
    let records = (0..512)
        .map(|index| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let start = seed % 8_000;
            let length = 1 + (seed >> 17) % 600;
            row_test_record(
                format!("depth-{index:04}").as_bytes(),
                start,
                start + length,
            )
        })
        .collect::<Vec<_>>();
    let query = row_test_query(&records);
    let rows = assign_alignment_rows(
        &query,
        AlignmentLayoutOptions {
            max_rows: query.batch.len(),
            ..AlignmentLayoutOptions::default()
        },
        None,
    );
    let mut events = records
        .iter()
        .flat_map(|record| [(record.start, 1_i32), (record.end, -1_i32)])
        .collect::<Vec<_>>();
    events.sort_unstable();
    let mut depth = 0_i32;
    let maximum_depth = events
        .into_iter()
        .map(|(_, delta)| {
            depth += delta;
            depth
        })
        .max()
        .unwrap_or_default();

    assert_eq!(
        rows.used_row_count(),
        usize::try_from(maximum_depth).unwrap()
    );
}

#[test]
fn regional_insertions_are_prominent_pickable_and_collision_bounded() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 0, 1_000).unwrap();
    let viewport = Viewport::new(interval, 50.0).unwrap();
    assert_eq!(viewport.detail_level(), DetailLevel::Regional);
    let mut record = row_test_record(b"regional-insertions", 100, 200);
    record.cigar = vec![
        CigarOp {
            kind: CigarOpKind::Match,
            len: 10,
        },
        CigarOp {
            kind: CigarOpKind::Insertion,
            len: 5,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 1,
        },
        CigarOp {
            kind: CigarOpKind::Insertion,
            len: 7,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 89,
        },
    ];
    let query = row_test_query(&[record]);

    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());

    assert_eq!(layout.stats.regional_insertions_rendered, 1);
    assert_eq!(layout.stats.regional_insertions_collapsed, 1);
    assert_eq!(layout.stats.regional_insertions_omitted, 0);
    assert!(
        layout
            .display_list
            .texts
            .iter()
            .any(|text| text.text == "I" && text.color == insertion_event_color(1.0))
    );
    assert!(layout.display_list.rectangles.iter().any(|rectangle| {
        rectangle.object_id & READ_DETAIL_OBJECT_BIT != 0 && rectangle.width >= 3.0
    }));
}

#[test]
fn default_reads_have_a_neutral_outline_and_pale_fill_at_every_read_stack_lod() {
    let contig = ContigId::new(0);
    let interval = GenomicInterval::new(contig, 0, 1_000).unwrap();
    let mut batch = AlignmentBatch::default();
    batch
        .push(&AlignmentRecord {
            contig,
            start: 100,
            end: 200,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"outlined".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 100,
            }],
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        })
        .unwrap();
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch,
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats::default(),
    };
    let options = AlignmentLayoutOptions {
        color_mode: AlignmentColorMode::None,
        ..AlignmentLayoutOptions::default()
    };
    for (pixel_width, expected_detail) in
        [(1_000.0, DetailLevel::Reads), (50.0, DetailLevel::Regional)]
    {
        let viewport = Viewport::new(interval, pixel_width).unwrap();
        assert_eq!(viewport.detail_level(), expected_detail);
        let layout = layout_alignment_query(&query, viewport, options);
        let body = layout
            .display_list
            .rectangles
            .iter()
            .filter(|primitive| primitive.object_id == 1)
            .collect::<Vec<_>>();
        assert_eq!(body.len(), 2);
        assert!(body[0].color.red < 0.7);
        assert!(body[1].color.red > 0.8);
        assert!((body[1].color.red - body[1].color.green).abs() < f32::EPSILON);
        assert!((body[1].color.green - body[1].color.blue).abs() < f32::EPSILON);
        assert!(body[1].width < body[0].width);
        assert!(body[1].height < body[0].height);
    }
}

#[test]
fn event_label_generation_obeys_a_hard_budget() {
    let mut display = DisplayList::default();
    let mut budget = 3;
    for index in 0..10 {
        add_event_label(
            &mut display,
            "I12",
            index as f32 * 20.0,
            20.0,
            Rgba::new(0.5, 0.2, 0.8, 1.0),
            1,
            &mut budget,
        );
    }
    assert_eq!(budget, 0);
    assert_eq!(display.texts.len(), 3);
    assert_eq!(
        display
            .rectangles
            .iter()
            .filter(|primitive| primitive.object_id == 0)
            .count(),
        3
    );
}

#[test]
fn maps_read_positions_through_insertions_and_deletions() {
    let cigar = [
        CigarOp {
            kind: CigarOpKind::Match,
            len: 2,
        },
        CigarOp {
            kind: CigarOpKind::Insertion,
            len: 2,
        },
        CigarOp {
            kind: CigarOpKind::Deletion,
            len: 3,
        },
        CigarOp {
            kind: CigarOpKind::Match,
            len: 2,
        },
    ];
    assert_eq!(
        reference_position_for_read_position(100, &cigar, 0),
        Some(100)
    );
    assert_eq!(
        reference_position_for_read_position(100, &cigar, 2),
        Some(102)
    );
    assert_eq!(
        reference_position_for_read_position(100, &cigar, 4),
        Some(105)
    );
}

fn modified_base_query(mm: &str, probabilities: Option<Vec<u64>>) -> (AlignmentQuery, Viewport) {
    modified_base_query_with_alignment(mm, probabilities, AlignmentFlags::from_bits(0), b"ACCC")
}

fn modified_base_query_with_alignment(
    mm: &str,
    probabilities: Option<Vec<u64>>,
    flags: AlignmentFlags,
    bases: &[u8],
) -> (AlignmentQuery, Viewport) {
    let mut auxiliary_fields = vec![AuxField {
        tag: *b"MM",
        value: AuxValue::String(mm.to_owned()),
    }];
    if let Some(probabilities) = probabilities {
        auxiliary_fields.push(AuxField {
            tag: *b"ML",
            value: AuxValue::UnsignedArray(probabilities),
        });
    }
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 104,
        flags,
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"modified".to_vec(),
        cigar: vec![CigarOp {
            kind: CigarOpKind::Match,
            len: 4,
        }],
        sequence: SequenceData::Present {
            bases: bases.to_vec(),
            qualities: Some(vec![40; bases.len()]),
        },
        auxiliary_fields,
    };
    let mut batch = AlignmentBatch::default();
    batch.push(&record).unwrap();
    let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
    let viewport = Viewport::new(interval, 800.0).unwrap();
    (
        AlignmentQuery {
            interval,
            header: Arc::default(),
            batch,
            pileup: PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats::default(),
        },
        viewport,
    )
}

fn modification_overlays(layout: &AlignmentLayout) -> Vec<&RectPrimitive> {
    layout
        .display_list
        .rectangles
        .iter()
        .filter(|primitive| {
            record_object_id(primitive.object_id) == 1
                && primitive.object_id & READ_DETAIL_OBJECT_BIT != 0
                && primitive.height <= 4.0
        })
        .collect()
}

#[test]
fn modified_base_calls_use_stable_type_colour_and_probability_alpha() {
    let (query, viewport) = modified_base_query("C+m,1;", Some(vec![255]));
    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
    let overlays = modification_overlays(&layout);
    assert_eq!(overlays.len(), 1);
    assert_eq!(
        overlays[0].color,
        modification_code_color(ModificationCode::Letter(b'm'))
    );

    assert_ne!(
        modification_code_color(ModificationCode::Letter(b'm')),
        modification_code_color(ModificationCode::Letter(b'h'))
    );
    assert_eq!(
        modification_code_color(ModificationCode::Chebi(76792)),
        modification_code_color(ModificationCode::Chebi(76792))
    );
}

#[test]
fn reverse_modified_base_calls_decode_in_sequencer_orientation_before_cigar_mapping() {
    let (query, viewport) = modified_base_query_with_alignment(
        "C+m,0;",
        Some(vec![255]),
        AlignmentFlags::from_bits(0x10),
        // Original ACCG, stored as reference-facing reverse complement CGGT.
        b"CGGT",
    );
    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
    let overlays = modification_overlays(&layout);
    assert_eq!(overlays.len(), 1);
    // Original position 1 becomes aligned position 2 on a reverse alignment.
    assert!((400.0..600.0).contains(&overlays[0].x));
}

#[test]
fn modified_base_probability_and_visibility_filters_are_enforced() {
    let (query, viewport) = modified_base_query("C+m,1;", Some(vec![127]));
    let mut options = AlignmentLayoutOptions {
        minimum_modification_probability: 128,
        ..AlignmentLayoutOptions::default()
    };
    let filtered = layout_alignment_query(&query, viewport, options);
    assert!(modification_overlays(&filtered).is_empty());

    options.minimum_modification_probability = 0;
    options.hidden_modification_codes[0] = Some(ModificationCode::Letter(b'm'));
    let hidden = layout_alignment_query(&query, viewport, options);
    assert!(modification_overlays(&hidden).is_empty());

    options.hidden_modification_codes[0] = None;
    options.show_modified_bases = false;
    let disabled = layout_alignment_query(&query, viewport, options);
    assert!(modification_overlays(&disabled).is_empty());
}

#[test]
fn missing_probability_is_visible_only_without_a_probability_threshold() {
    let (query, viewport) = modified_base_query("C+m,1;", None);
    let layout = layout_alignment_query(&query, viewport, AlignmentLayoutOptions::default());
    let overlays = modification_overlays(&layout);
    assert_eq!(overlays.len(), 1);
    assert!((overlays[0].color.alpha - 0.78).abs() < f32::EPSILON);

    let options = AlignmentLayoutOptions {
        minimum_modification_probability: 1,
        ..AlignmentLayoutOptions::default()
    };
    let filtered = layout_alignment_query(&query, viewport, options);
    assert!(modification_overlays(&filtered).is_empty());
}

#[test]
fn modified_vs_unmodified_mode_marks_only_implicit_canonical_sites() {
    let (query, viewport) = modified_base_query("C+m,1;", Some(vec![255]));
    let options = AlignmentLayoutOptions {
        modified_base_color_mode: ModifiedBaseColorMode::ModifiedVsUnmodified,
        ..AlignmentLayoutOptions::default()
    };
    let layout = layout_alignment_query(&query, viewport, options);
    let overlays = modification_overlays(&layout);
    assert_eq!(overlays.len(), 3);
    assert_eq!(
        overlays
            .iter()
            .filter(|primitive| primitive.color.red > 0.8)
            .count(),
        1
    );
    assert_eq!(
        overlays
            .iter()
            .filter(|primitive| primitive.color.blue > 0.8 && primitive.color.red < 0.3)
            .count(),
        2
    );

    let (unknown_query, unknown_viewport) = modified_base_query("C+m?,1;", Some(vec![255]));
    let unknown = layout_alignment_query(&unknown_query, unknown_viewport, options);
    assert_eq!(modification_overlays(&unknown).len(), 1);
}

#[test]
fn modified_base_overlays_obey_the_shared_hard_budget() {
    let (query, viewport) = modified_base_query("C+m,0,0,0;", Some(vec![255, 255, 255]));
    let mut display = DisplayList::default();
    let mut budget = 2;
    add_modified_base_calls(
        &mut display,
        &query,
        0,
        viewport,
        20.0,
        5.0,
        1,
        &AlignmentLayoutOptions::default(),
        &mut budget,
        None,
    );
    assert_eq!(display.rectangles.len(), 2);
    assert_eq!(budget, 0);
}

#[test]
fn aggregate_modification_lane_renders_fraction_color_and_selection() {
    let (mut query, viewport) = modified_base_query("C+m,1;", Some(vec![255]));
    query.pileup.modifications = ModificationSummary {
        bin_count: 4,
        series: vec![ModificationSeries {
            key: ModificationSeriesKey {
                canonical_base: b'C',
                code: ModificationCode::Letter(b'm'),
                modification_strand: BaseModificationStrand::Forward,
                implicit_mode: ImplicitModificationMode::Canonical,
                haplotype: Some(1),
            },
            bins: vec![
                ModificationBin::default(),
                ModificationBin::default(),
                ModificationBin {
                    modified_calls: 1,
                    canonical_observations: 2,
                    forward_modified_calls: 1,
                    probability_sum: 255,
                    probability_observations: 1,
                    ..ModificationBin::default()
                },
                ModificationBin::default(),
            ],
        }],
        provenance: ModificationProvenance {
            eligible_records: 2,
            records_with_modification_tags: 1,
            decoded_calls: 1,
            all_eligible_records: true,
            ..ModificationProvenance::default()
        },
    };
    let options = AlignmentLayoutOptions {
        selected_modification_position: Some(102),
        ..AlignmentLayoutOptions::default()
    };
    let layout = layout_alignment_query(&query, viewport, options);
    let lane = layout
        .display_list
        .rectangles
        .iter()
        .filter(|primitive| {
            primitive.y >= options.modification_top
                && primitive.y < options.modification_top + options.modification_height
        })
        .collect::<Vec<_>>();
    assert_eq!(lane.len(), 2);
    assert_eq!(
        lane[0].color,
        modification_code_color(ModificationCode::Letter(b'm'))
    );
    assert!((lane[0].height - options.modification_height * 0.5).abs() < f32::EPSILON);
    assert!((lane[1].height - options.modification_height).abs() < f32::EPSILON);
}

#[test]
fn aggregate_modification_lane_respects_visibility_and_two_color_mode() {
    let (mut query, viewport) = modified_base_query("C+m,1;", Some(vec![255]));
    query.pileup.modifications = ModificationSummary {
        bin_count: 1,
        series: vec![ModificationSeries {
            key: ModificationSeriesKey {
                canonical_base: b'C',
                code: ModificationCode::Letter(b'm'),
                modification_strand: BaseModificationStrand::Forward,
                implicit_mode: ImplicitModificationMode::Canonical,
                haplotype: None,
            },
            bins: vec![ModificationBin {
                modified_calls: 1,
                canonical_observations: 2,
                probability_sum: 255,
                probability_observations: 1,
                ..ModificationBin::default()
            }],
        }],
        provenance: ModificationProvenance::default(),
    };
    let options = AlignmentLayoutOptions {
        modified_base_color_mode: ModifiedBaseColorMode::ModifiedVsUnmodified,
        ..AlignmentLayoutOptions::default()
    };
    let mut display = DisplayList::default();
    add_modification_frequency(
        &mut display,
        &query.pileup.modifications,
        query.interval,
        viewport,
        &options,
    );
    assert_eq!(display.rectangles.len(), 2);
    assert!(display.rectangles[0].color.blue > display.rectangles[0].color.red);
    assert_eq!(
        display.rectangles[1].color,
        Rgba::new(0.90, 0.24, 0.92, 0.92)
    );

    let mut hidden = options;
    hidden.hidden_modification_codes[0] = Some(ModificationCode::Letter(b'm'));
    let mut display = DisplayList::default();
    add_modification_frequency(
        &mut display,
        &query.pileup.modifications,
        query.interval,
        viewport,
        &hidden,
    );
    assert!(display.rectangles.is_empty());

    hidden.hidden_modification_codes[0] = None;
    hidden.show_modification_frequency = false;
    add_modification_frequency(
        &mut display,
        &query.pileup.modifications,
        query.interval,
        viewport,
        &hidden,
    );
    assert!(display.rectangles.is_empty());
}
