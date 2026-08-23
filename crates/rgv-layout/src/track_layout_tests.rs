use rgv_core::{ContigId, GenomicInterval};

use super::*;

fn viewport() -> Viewport {
    Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        1_000.0,
    )
    .unwrap()
}

#[test]
fn bed_blocks_are_rendered_as_connected_exons() {
    let feature = FeatureRecord {
        contig: "chr1".to_owned(),
        start: 110,
        end: 190,
        name: Some("tx".to_owned()),
        feature_type: None,
        score: None,
        strand: Strand::Forward,
        blocks: vec![(110, 130), (170, 190)],
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    };
    let (display, stats) = layout_features(&[feature], viewport(), TrackLayoutOptions::default());
    assert_eq!(stats.rendered_records, 1);
    assert_eq!(display.rectangles.len(), 3);
}

#[test]
fn gene_models_render_utrs_thinner_than_coding_exons() {
    let feature = FeatureRecord {
        contig: "chr1".to_owned(),
        start: 110,
        end: 190,
        name: Some("GENE1".to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: vec![(110, 140), (160, 190)],
        coding_interval: Some((125, 175)),
        peak: None,
        attributes: Vec::new(),
    };
    let (display, _) = layout_features(&[feature], viewport(), TrackLayoutOptions::default());
    assert_eq!(display.rectangles.len(), 5);
    let exon_height = display.rectangles[1].height;
    let coding_height = display.rectangles[2].height;
    assert!(coding_height > exon_height);
}

#[test]
fn narrow_peaks_render_the_called_summit() {
    let feature = FeatureRecord {
        contig: "chr1".to_owned(),
        start: 110,
        end: 190,
        name: Some("peak".to_owned()),
        feature_type: Some("narrowPeak".to_owned()),
        score: Some(500.0),
        strand: Strand::Unstranded,
        blocks: Vec::new(),
        coding_interval: None,
        peak: Some(rgv_hts::PeakMetrics {
            signal_value: 12.0,
            p_value: Some(5.0),
            q_value: None,
            summit: Some(150),
        }),
        attributes: Vec::new(),
    };
    let (display, _) = layout_features(&[feature], viewport(), TrackLayoutOptions::default());
    assert_eq!(display.rectangles.len(), 2);
    assert!((display.rectangles[1].width - 2.0).abs() < f32::EPSILON);
    assert!((display.rectangles[1].x - 499.0).abs() < f32::EPSILON);
}

#[test]
fn feature_display_modes_are_collapsed_squished_and_expanded() {
    let features = (0..3)
        .map(|index| FeatureRecord {
            contig: "chr1".to_owned(),
            start: 110 + index,
            end: 180 + index,
            name: Some(format!("feature-{index}")),
            feature_type: None,
            score: None,
            strand: Strand::Unstranded,
            blocks: Vec::new(),
            coding_interval: None,
            peak: None,
            attributes: Vec::new(),
        })
        .collect::<Vec<_>>();
    let expanded = layout_features(
        &features,
        viewport(),
        TrackLayoutOptions {
            maximum_rows: 1,
            feature_display_mode: FeatureDisplayMode::Expanded,
            ..TrackLayoutOptions::default()
        },
    );
    assert_eq!(expanded.1.rendered_records, 1);
    assert_eq!(expanded.1.hidden_records, 2);

    let squished = layout_features(
        &features,
        viewport(),
        TrackLayoutOptions {
            maximum_rows: 1,
            feature_display_mode: FeatureDisplayMode::Squished,
            ..TrackLayoutOptions::default()
        },
    );
    assert_eq!(squished.1.rendered_records, 2);
    assert_eq!(squished.1.hidden_records, 1);

    let collapsed = layout_features(
        &features,
        viewport(),
        TrackLayoutOptions {
            maximum_rows: 1,
            feature_display_mode: FeatureDisplayMode::Collapsed,
            ..TrackLayoutOptions::default()
        },
    );
    assert_eq!(collapsed.1.rendered_records, 3);
    assert_eq!(collapsed.1.hidden_records, 0);
    let block_rows = collapsed
        .0
        .rectangles
        .iter()
        .skip(1)
        .step_by(2)
        .map(|rectangle| rectangle.y)
        .collect::<Vec<_>>();
    assert_eq!(block_rows.len(), 3);
    assert!(
        block_rows
            .windows(2)
            .all(|pair| (pair[0] - pair[1]).abs() < f32::EPSILON)
    );
}

#[test]
fn quantitative_layout_supports_signed_values() {
    let records = [
        QuantitativeRecord {
            start: 100,
            end: 150,
            value: 2.0,
        },
        QuantitativeRecord {
            start: 150,
            end: 200,
            value: -1.0,
        },
    ];
    let settings = QuantitativeTrackSettings {
        window_function: QuantitativeWindowFunction::Raw,
        show_range_labels: false,
        ..QuantitativeTrackSettings::default()
    };
    let (display, stats) = layout_quantitative(
        &records,
        viewport(),
        TrackLayoutOptions::default(),
        &settings,
        None,
    );
    assert_eq!(stats.rendered_records, 2);
    assert_eq!(stats.quantitative_minimum, Some(-1.0));
    assert_eq!(stats.quantitative_maximum, Some(2.0));
    assert_eq!(display.rectangles.len(), 3); // baseline and two bars
    assert!(display.rectangles[1].y < display.rectangles[2].y);
}

#[test]
fn quantitative_mean_is_weighted_by_genomic_overlap() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 104).unwrap(),
        1.0,
    )
    .unwrap();
    let records = [
        QuantitativeRecord {
            start: 100,
            end: 101,
            value: 2.0,
        },
        QuantitativeRecord {
            start: 101,
            end: 104,
            value: 4.0,
        },
    ];
    let settings = QuantitativeTrackSettings::default();
    assert_eq!(
        quantitative_data_range(&records, viewport, &settings),
        Some(QuantitativeRange {
            minimum: 0.0,
            maximum: 3.5,
        })
    );
}

#[test]
fn quantitative_missing_windows_are_explicit_and_bounded() {
    let records = [QuantitativeRecord {
        start: 100,
        end: 150,
        value: 2.0,
    }];
    let settings = QuantitativeTrackSettings {
        missing_mode: QuantitativeMissingMode::Marker,
        show_range_labels: false,
        ..QuantitativeTrackSettings::default()
    };
    let (display, stats) = layout_quantitative(
        &records,
        viewport(),
        TrackLayoutOptions::default(),
        &settings,
        None,
    );
    assert_eq!(stats.rendered_records, 50);
    assert_eq!(stats.missing_windows, 50);
    assert!(stats.aggregated);
    assert_eq!(display.rectangles.len(), 101); // baseline, values, and missing markers
}

#[test]
fn quantitative_fixed_and_shared_ranges_override_automatic_range() {
    let records = [QuantitativeRecord {
        start: 100,
        end: 200,
        value: 2.0,
    }];
    let fixed = QuantitativeTrackSettings {
        scale_mode: QuantitativeScaleMode::Fixed,
        fixed_minimum: -10.0,
        fixed_maximum: 20.0,
        ..QuantitativeTrackSettings::default()
    };
    let (_, fixed_stats) = layout_quantitative(
        &records,
        viewport(),
        TrackLayoutOptions::default(),
        &fixed,
        None,
    );
    assert_eq!(fixed_stats.quantitative_minimum, Some(-10.0));
    assert_eq!(fixed_stats.quantitative_maximum, Some(20.0));

    let shared = QuantitativeTrackSettings {
        scale_mode: QuantitativeScaleMode::Shared,
        ..QuantitativeTrackSettings::default()
    };
    let (_, shared_stats) = layout_quantitative(
        &records,
        viewport(),
        TrackLayoutOptions::default(),
        &shared,
        Some(QuantitativeRange {
            minimum: -5.0,
            maximum: 8.0,
        }),
    );
    assert_eq!(shared_stats.quantitative_minimum, Some(-5.0));
    assert_eq!(shared_stats.quantitative_maximum, Some(8.0));
}

#[test]
fn raw_quantitative_layout_has_a_deterministic_record_cap() {
    let records = (0..MAX_RAW_QUANTITATIVE_RECORDS + 10)
        .map(|index| QuantitativeRecord {
            start: 100 + u64::try_from(index % 100).unwrap(),
            end: 101 + u64::try_from(index % 100).unwrap(),
            value: 1.0,
        })
        .collect::<Vec<_>>();
    let settings = QuantitativeTrackSettings {
        window_function: QuantitativeWindowFunction::Raw,
        show_range_labels: false,
        ..QuantitativeTrackSettings::default()
    };
    let (_, stats) = layout_quantitative(
        &records,
        viewport(),
        TrackLayoutOptions::default(),
        &settings,
        None,
    );
    assert_eq!(stats.rendered_records, MAX_RAW_QUANTITATIVE_RECORDS);
    assert_eq!(stats.hidden_records, 10);
}

#[test]
fn quantitative_arithmetic_segments_exact_overlaps_and_reports_missing_values() {
    let left = [QuantitativeRecord {
        start: 0,
        end: 10,
        value: 4.0,
    }];
    let right = [QuantitativeRecord {
        start: 2,
        end: 8,
        value: 2.0,
    }];
    let composition =
        compose_quantitative_records(&left, &right, QuantitativeArithmetic::Subtract, 0, 10, 100);
    assert_eq!(
        composition.records,
        vec![QuantitativeRecord {
            start: 2,
            end: 8,
            value: 2.0,
        }]
    );
    assert_eq!(composition.provenance.missing_operand_segments, 2);
    assert!(!composition.provenance.records_truncated);
}

#[test]
fn quantitative_arithmetic_uses_narrowest_overlap_and_marks_division_by_zero() {
    let left = [
        QuantitativeRecord {
            start: 0,
            end: 10,
            value: 1.0,
        },
        QuantitativeRecord {
            start: 2,
            end: 4,
            value: 5.0,
        },
    ];
    let right = [
        QuantitativeRecord {
            start: 0,
            end: 5,
            value: 1.0,
        },
        QuantitativeRecord {
            start: 5,
            end: 10,
            value: 0.0,
        },
    ];
    let added =
        compose_quantitative_records(&left, &right, QuantitativeArithmetic::Add, 0, 10, 100);
    assert_eq!(
        added.records,
        vec![
            QuantitativeRecord {
                start: 0,
                end: 2,
                value: 2.0,
            },
            QuantitativeRecord {
                start: 2,
                end: 4,
                value: 6.0,
            },
            QuantitativeRecord {
                start: 4,
                end: 5,
                value: 2.0,
            },
            QuantitativeRecord {
                start: 5,
                end: 10,
                value: 1.0,
            },
        ]
    );
    let divided =
        compose_quantitative_records(&left, &right, QuantitativeArithmetic::Divide, 0, 10, 100);
    assert_eq!(divided.provenance.divide_by_zero_segments, 1);
    assert_eq!(divided.records.last().unwrap().end, 5);
}

#[test]
fn copy_number_order_is_deterministic_and_places_missing_samples_last() {
    let values = vec![Some(1.0), Some(3.0), None, Some(3.0)];
    assert_eq!(
        copy_number_sample_order(4, &values, CopyNumberSortMode::Amplification),
        vec![1, 3, 0, 2]
    );
    assert_eq!(
        copy_number_sample_order(4, &values, CopyNumberSortMode::Deletion),
        vec![0, 1, 3, 2]
    );
    assert_eq!(
        copy_number_sample_order(4, &values, CopyNumberSortMode::FileOrder),
        vec![0, 1, 2, 3]
    );
}

#[test]
fn copy_number_layout_only_materializes_visible_sample_rows() {
    let query = CopyNumberQuery {
        samples: (0..5).map(|index| format!("sample-{index}")).collect(),
        segments: (0..5)
            .map(|sample_index| rgv_hts::CopyNumberSegment {
                sample_index,
                start: 100,
                end: 200,
                marker_count: Some(10),
                value: 1.0 + sample_index as f32,
            })
            .collect(),
        sample_sort_values: vec![None; 5],
        inferred_value_semantics: CopyNumberValueSemantics::Absolute,
        provenance: rgv_hts::CopyNumberQueryProvenance::default(),
    };
    let settings = CopyNumberTrackSettings {
        row_height: 10.0,
        ..CopyNumberTrackSettings::default()
    };
    let layout = layout_copy_number(
        &query,
        viewport(),
        TrackLayoutOptions {
            top: 0.0,
            height: copy_number_track_height(5, 10.0),
            ..TrackLayoutOptions::default()
        },
        &settings,
        &QuantitativeTrackSettings::default(),
        None,
        44.0,
        64.0,
    );
    assert_eq!(
        layout
            .visible_rows
            .iter()
            .map(|row| row.sample_index)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert_eq!(layout.stats.rendered_records, 2);
    assert_eq!(layout.stats.hidden_records, 3);
    assert!((layout.baseline - 2.0).abs() < f32::EPSILON);
    assert_eq!(layout.value_semantics, CopyNumberValueSemantics::Absolute);
    assert!((copy_number_track_height(5, 10.0) - 74.0).abs() < f32::EPSILON);
}

#[test]
fn mutation_layout_virtualizes_samples_and_marks_repeated_calls() {
    let mutation = |sample_index, gene: &str| rgv_hts::MutationRecord {
        sample_index,
        start: 120,
        end: 121,
        source_start: 121,
        source_end: 121,
        insertion_boundary: false,
        gene: gene.to_owned(),
        classification: "Missense_Mutation".to_owned(),
        variant_type: "SNP".to_owned(),
        reference: "C".to_owned(),
        tumor_allele_one: "C".to_owned(),
        tumor_allele_two: "T".to_owned(),
        annotations: Vec::new(),
        annotation_values_omitted: 0,
    };
    let query = rgv_hts::MutationQuery {
        headers: Vec::new(),
        samples: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
        records: vec![
            mutation(0, "TP53"),
            mutation(1, "KRAS"),
            mutation(1, "KRAS"),
        ],
        provenance: rgv_hts::MutationQueryProvenance::default(),
    };
    let layout = layout_mutations(
        &query,
        viewport(),
        TrackLayoutOptions {
            top: 0.0,
            height: mutation_track_height(3, 16.0),
            ..TrackLayoutOptions::default()
        },
        &MutationTrackSettings::default(),
        40.0,
        56.0,
    );

    assert_eq!(layout.visible_rows.len(), 1);
    assert_eq!(layout.visible_rows[0].sample_index, 1);
    assert_eq!(layout.stats.rendered_records, 2);
    assert!(layout.display_list.rectangles.len() >= 6);
    assert_eq!(layout.display_list.texts.len(), 2);
    assert!((mutation_track_height(3, 16.0) - 72.0).abs() < f32::EPSILON);
}

#[test]
fn interaction_layout_draws_intrachromosomal_arcs_and_cross_contig_markers() {
    let endpoint = |contig: &str, start, end, name: &str| rgv_hts::InteractionEndpoint {
        contig: contig.to_owned(),
        start,
        end,
        name: Some(name.to_owned()),
        strand: Strand::Unstranded,
    };
    let records = [
        InteractionRecord {
            source: endpoint("chr1", 110, 120, "a"),
            target: endpoint("chr1", 180, 190, "b"),
            name: Some("intra".to_owned()),
            score: Some(100.0),
            value: None,
            experiment: None,
            color: Some([10, 20, 30, 255]),
            directional: false,
        },
        InteractionRecord {
            source: endpoint("chr1", 130, 140, "local"),
            target: endpoint("chr2", 500, 510, "remote"),
            name: Some("cross".to_owned()),
            score: Some(50.0),
            value: None,
            experiment: None,
            color: None,
            directional: true,
        },
    ];
    let (display, stats) = layout_interactions(
        &records,
        "chr1",
        viewport(),
        TrackLayoutOptions::default(),
        &InteractionTrackSettings::default(),
    );
    assert_eq!(stats.rendered_records, 2);
    assert!(display.rectangles.len() >= INTERACTION_ARC_SEGMENTS + 4);
    assert_eq!(display.texts.len(), 2);
    assert!(display.texts[1].text.contains("chr2:501"));
}

#[test]
fn interaction_layout_obeys_filter_fixed_scale_and_hard_record_cap() {
    let records = (0..MAX_RENDERED_INTERACTIONS + 10)
        .map(|index| InteractionRecord {
            source: rgv_hts::InteractionEndpoint {
                contig: "chr1".to_owned(),
                start: 100 + u64::try_from(index % 20).unwrap(),
                end: 101 + u64::try_from(index % 20).unwrap(),
                name: None,
                strand: Strand::Unstranded,
            },
            target: rgv_hts::InteractionEndpoint {
                contig: "chr1".to_owned(),
                start: 180,
                end: 181,
                name: None,
                strand: Strand::Unstranded,
            },
            name: None,
            score: Some(if index == 0 { 1.0 } else { 10.0 }),
            value: None,
            experiment: None,
            color: None,
            directional: false,
        })
        .collect::<Vec<_>>();
    let settings = InteractionTrackSettings {
        minimum_score: Some(5.0),
        fixed_maximum_score: Some(20.0),
        show_labels: false,
        ..InteractionTrackSettings::default()
    };
    let (_, stats) = layout_interactions(
        &records,
        "chr1",
        viewport(),
        TrackLayoutOptions::default(),
        &settings,
    );
    assert_eq!(stats.rendered_records, MAX_RENDERED_INTERACTIONS);
    assert_eq!(stats.hidden_records, 10);
}

#[test]
fn genotype_colour_uses_selected_cohort_calls_instead_of_variant_type() {
    let record = VariantRecord {
        contig: "chr1".to_owned(),
        start: 120,
        end: 121,
        id: None,
        reference: b"A".to_vec(),
        alternates: vec![b"T".to_vec()],
        quality: Some(50.0),
        filters: Vec::new(),
        info: Vec::new(),
        genotypes: vec![rgv_hts::VariantGenotype {
            sample_index: 0,
            alleles: vec![
                rgv_hts::VariantGenotypeAllele {
                    index: Some(0),
                    phased: false,
                },
                rgv_hts::VariantGenotypeAllele {
                    index: Some(1),
                    phased: false,
                },
            ],
            fields: Vec::new(),
        }],
        variant_types: vec![rgv_hts::VariantType::Snv],
        breakends: Vec::new(),
    };
    assert_eq!(genotype_color(&record), Rgba::new(0.94, 0.54, 0.12, 0.96));
    let settings = VariantTrackSettings {
        color_mode: VariantColorMode::Genotype,
        ..VariantTrackSettings::default()
    };
    let (display, stats) = layout_variants(
        &[record],
        viewport(),
        TrackLayoutOptions::default(),
        &settings,
    );
    assert_eq!(stats.rendered_records, 1);
    assert_eq!(
        display.rectangles[1].color,
        Rgba::new(0.94, 0.54, 0.12, 0.96)
    );
}

#[test]
fn gwas_layout_transforms_p_values_and_preserves_selectable_point_identity() {
    let records = vec![
        rgv_hts::GwasRecord {
            contig: "chr1".to_owned(),
            position: 125,
            identifier: "rs-low".to_owned(),
            p_value: 0.1,
            minus_log10_p: 1.0,
            fields: Vec::new(),
        },
        rgv_hts::GwasRecord {
            contig: "chr1".to_owned(),
            position: 175,
            identifier: "rs-high".to_owned(),
            p_value: 1e-30,
            minus_log10_p: 30.0,
            fields: Vec::new(),
        },
    ];
    let settings = GwasTrackSettings {
        color_mode: GwasColorMode::Uniform,
        uniform_color: [10, 20, 30, 255],
        maximum_score: 25.0,
        point_size: 5.0,
        ..GwasTrackSettings::default()
    };
    let options = TrackLayoutOptions {
        selected_record: Some(1),
        ..TrackLayoutOptions::default()
    };
    let (display, stats) = layout_gwas(&records, viewport(), options, &settings);
    assert_eq!(stats.rendered_records, 2);
    assert_eq!(stats.quantitative_maximum, Some(25.0));
    // Axis, first point, selection halo, selected point.
    assert_eq!(display.rectangles.len(), 4);
    assert!((display.rectangles[1].width - 5.0).abs() < f32::EPSILON);
    assert_eq!(
        display.rectangles[3].color,
        rgba_from_bytes([10, 20, 30, 255])
    );
    assert!(display.rectangles[3].y < display.rectangles[1].y);
    assert_eq!(display.rectangles[3].object_id, options.object_id_base + 1);
}

#[test]
fn gwas_chromosome_and_alternating_colours_are_stable() {
    let chromosome = GwasTrackSettings::default();
    assert_ne!(
        gwas_point_color("chr1", &chromosome),
        gwas_point_color("chr2", &chromosome)
    );
    let alternating = GwasTrackSettings {
        color_mode: GwasColorMode::Alternating,
        alternating_color_one: [1, 2, 3, 255],
        alternating_color_two: [4, 5, 6, 255],
        ..GwasTrackSettings::default()
    };
    assert_eq!(
        gwas_point_color("1", &alternating),
        rgba_from_bytes([1, 2, 3, 255])
    );
    assert_eq!(
        gwas_point_color("chr2", &alternating),
        rgba_from_bytes([4, 5, 6, 255])
    );
    assert!(
        (gwas_point_y(f32::INFINITY, 10.0, 100.0, 25.0) - gwas_point_y(25.0, 10.0, 100.0, 25.0))
            .abs()
            < f32::EPSILON
    );
}
