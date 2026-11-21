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
    assert!(display.rectangles.len() > 3);
    assert!(
        display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::ChevronRight)
    );
    assert!(
        display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::DirectionalTerminalRight)
    );
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
    assert!(display.rectangles.len() > 5);
    let exon_height = display.rectangles[1].height;
    let coding_height = display.rectangles[2].height;
    assert!(coding_height > exon_height);
}

#[test]
fn reverse_transcripts_use_left_chevrons_and_unstranded_features_remain_neutral() {
    let transcript = FeatureRecord {
        contig: "chr1".to_owned(),
        start: 110,
        end: 190,
        name: Some("reverse".to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: Strand::Reverse,
        blocks: vec![(110, 130), (170, 190)],
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    };
    let neutral = FeatureRecord {
        strand: Strand::Unstranded,
        name: Some("neutral".to_owned()),
        start: 111,
        end: 189,
        blocks: vec![(111, 131), (169, 189)],
        ..transcript.clone()
    };
    let (reverse, _) = layout_features(&[transcript], viewport(), TrackLayoutOptions::default());
    assert!(
        reverse
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::ChevronLeft)
    );
    assert!(
        reverse
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::DirectionalTerminalLeft)
    );
    let (neutral, _) = layout_features(&[neutral], viewport(), TrackLayoutOptions::default());
    assert!(
        neutral
            .rectangles
            .iter()
            .all(|rectangle| rectangle.shape == RectShape::Rectangle)
    );
}

#[test]
fn component_features_use_compact_chevrons_instead_of_full_terminal_wedges() {
    let exon = FeatureRecord {
        contig: "chr1".to_owned(),
        start: 120,
        end: 160,
        name: Some("component-exon".to_owned()),
        feature_type: Some("exon".to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    };
    let (display, _) = layout_features(&[exon], viewport(), TrackLayoutOptions::default());
    assert!(
        display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::ChevronRight)
    );
    assert!(display.rectangles.iter().all(|rectangle| {
        !matches!(
            rectangle.shape,
            RectShape::DirectionalTerminalLeft | RectShape::DirectionalTerminalRight
        )
    }));
}

#[test]
fn gene_models_render_bounded_gene_labels_when_rows_have_room() {
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
    let (display, _) = layout_features(
        &[feature],
        viewport(),
        TrackLayoutOptions {
            height: 48.0,
            maximum_rows: 2,
            ..TrackLayoutOptions::default()
        },
    );
    assert_eq!(display.texts.len(), 1);
    assert_eq!(display.texts[0].text, "GENE1");
}

#[test]
fn gene_model_labels_do_not_overlap_at_overview_scale() {
    let gene = |start, end, name: &str| FeatureRecord {
        contig: "chr1".to_owned(),
        start,
        end,
        name: Some(name.to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: vec![(start, end)],
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    };
    let features = [
        gene(110, 111, "GENE_ONE"),
        gene(112, 113, "GENE_TWO"),
        gene(180, 181, "GENE_THREE"),
    ];
    let (display, _) = layout_features(
        &features,
        viewport(),
        TrackLayoutOptions {
            height: 24.0,
            maximum_rows: 1,
            ..TrackLayoutOptions::default()
        },
    );

    assert_eq!(
        display
            .texts
            .iter()
            .map(|text| text.text.as_str())
            .collect::<Vec<_>>(),
        ["GENE_ONE", "GENE_THREE"]
    );
}

#[test]
fn gene_model_label_prefers_symbol_to_transcript_accession() {
    let transcript = FeatureRecord {
        contig: "chr1".to_owned(),
        start: 110,
        end: 190,
        name: Some("ENST000001".to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: vec![(110, 190)],
        coding_interval: None,
        peak: None,
        attributes: vec![
            ("gene_name".to_owned(), "ACTB".to_owned()),
            ("gene_id".to_owned(), "ENSG000001".to_owned()),
            ("transcript_id".to_owned(), "ENST000001".to_owned()),
        ],
    };
    let (display, _) = layout_features(
        &[transcript],
        viewport(),
        TrackLayoutOptions {
            height: 24.0,
            maximum_rows: 1,
            ..TrackLayoutOptions::default()
        },
    );
    assert_eq!(display.texts[0].text, "ACTB");
}

#[test]
fn gff_gene_records_receive_automatic_symbol_labels_but_exons_do_not() {
    let record = |feature_type: &str, start, end| FeatureRecord {
        contig: "chr1".to_owned(),
        start,
        end,
        name: Some("ENSG000001".to_owned()),
        feature_type: Some(feature_type.to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: vec![("gene_name".to_owned(), "ACTB".to_owned())],
    };
    let (display, _) = layout_features(
        &[record("gene", 110, 150), record("exon", 160, 190)],
        viewport(),
        TrackLayoutOptions {
            height: 48.0,
            maximum_rows: 2,
            ..TrackLayoutOptions::default()
        },
    );
    assert_eq!(
        display
            .texts
            .iter()
            .map(|text| text.text.as_str())
            .collect::<Vec<_>>(),
        ["ACTB"]
    );
}

#[test]
fn subpixel_gene_labels_are_suppressed_until_selected() {
    let transcript = FeatureRecord {
        contig: "chr1".to_owned(),
        start: 100_000,
        end: 100_100,
        name: Some("SMALL".to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: vec![(100_000, 100_100)],
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    };
    let wide = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 0, 1_000_000).unwrap(),
        1_000.0,
    )
    .unwrap();
    let options = TrackLayoutOptions {
        height: 24.0,
        maximum_rows: 1,
        ..TrackLayoutOptions::default()
    };
    let (ordinary, _) = layout_features(std::slice::from_ref(&transcript), wide, options);
    let (selected, _) = layout_features(
        &[transcript],
        wide,
        TrackLayoutOptions {
            selected_record: Some(0),
            ..options
        },
    );
    assert!(ordinary.texts.is_empty());
    assert_eq!(selected.texts[0].text, "SMALL");
}

#[test]
fn overview_gene_models_suppress_subpixel_exons_but_keep_a_landmark_and_direction() {
    let transcript = FeatureRecord {
        contig: "chr1".to_owned(),
        start: 1_000,
        end: 9_000,
        name: Some("GENE_OVERVIEW".to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: (0..100)
            .map(|index| {
                let start = 1_000 + index * 80;
                (start, start + 10)
            })
            .collect(),
        coding_interval: Some((1_010, 8_990)),
        peak: None,
        attributes: Vec::new(),
    };
    let wide = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 0, 1_000_000).unwrap(),
        1_000.0,
    )
    .unwrap();
    let (display, stats) = layout_features(
        &[transcript],
        wide,
        TrackLayoutOptions {
            height: 48.0,
            maximum_rows: 2,
            ..TrackLayoutOptions::default()
        },
    );
    assert_eq!(stats.rendered_records, 1);
    assert!(display.rectangles.len() <= 3);
    assert!(
        display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::ChevronRight)
    );
}

#[test]
fn selected_gene_label_survives_normal_collision_suppression() {
    let gene = |name: &str| FeatureRecord {
        contig: "chr1".to_owned(),
        start: 110,
        end: 190,
        name: Some(name.to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: None,
        strand: Strand::Forward,
        blocks: vec![(110, 140), (160, 190)],
        coding_interval: Some((120, 180)),
        peak: None,
        attributes: Vec::new(),
    };
    let (display, _) = layout_features(
        &[gene("FIRST"), gene("SELECTED")],
        viewport(),
        TrackLayoutOptions {
            height: 12.0,
            maximum_rows: 1,
            selected_record: Some(1),
            feature_display_mode: FeatureDisplayMode::Collapsed,
            ..TrackLayoutOptions::default()
        },
    );
    assert_eq!(
        display
            .texts
            .iter()
            .map(|text| text.text.as_str())
            .collect::<Vec<_>>(),
        ["SELECTED"]
    );
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
        peak: Some(rgv_model::PeakMetrics {
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
fn raw_quantitative_layout_covers_the_viewport_beyond_its_record_cap() {
    let raw = QuantitativeTrackSettings {
        window_function: QuantitativeWindowFunction::Raw,
        show_range_labels: false,
        ..QuantitativeTrackSettings::default()
    };
    let covered_pixels = |display: &DisplayList| {
        let mut covered = vec![false; 1_000];
        for rectangle in display.rectangles.iter().filter(|r| r.object_id != 0) {
            let first = rectangle.x.max(0.0) as usize;
            let last = ((rectangle.x + rectangle.width).min(1_000.0)) as usize;
            covered[first.min(1_000)..last].fill(true);
        }
        covered
    };

    // Within the cap: one window per record, identified by record index.
    let few = (0..100_u64)
        .map(|base| QuantitativeRecord {
            start: 100 + base,
            end: 101 + base,
            value: 1.0 + base as f32,
        })
        .collect::<Vec<_>>();
    let (display, stats) =
        layout_quantitative(&few, viewport(), TrackLayoutOptions::default(), &raw, None);
    assert!(!stats.aggregated);
    assert_eq!(stats.rendered_records, 100);
    assert_eq!(stats.hidden_records, 0);
    assert!(covered_pixels(&display).iter().all(|covered| *covered));

    // Beyond the cap, with the records in genomic order: the first
    // MAX_RAW_QUANTITATIVE_RECORDS all fall in the left half of the viewport,
    // which is all the previous prefix truncation would have drawn.
    let per_base = MAX_RAW_QUANTITATIVE_RECORDS / 50;
    let mut dense = Vec::new();
    for base in 0..100_u64 {
        for copy in 0..per_base {
            dense.push(QuantitativeRecord {
                start: 100 + base,
                end: 101 + base,
                // One trough per base among positive values.
                value: if copy == 3 { -9.0 } else { 2.0 },
            });
        }
    }
    assert!(dense.len() > MAX_RAW_QUANTITATIVE_RECORDS);
    assert!(dense[MAX_RAW_QUANTITATIVE_RECORDS - 1].start <= 150);
    let (display, stats) = layout_quantitative(
        &dense,
        viewport(),
        TrackLayoutOptions::default(),
        &raw,
        None,
    );
    assert!(stats.aggregated);
    assert_eq!(stats.rendered_records, 100);
    assert_eq!(stats.missing_windows, 0);
    assert!(covered_pixels(&display).iter().all(|covered| *covered));
    // The fallback keeps the extreme rather than averaging it away.
    assert_eq!(stats.quantitative_minimum, Some(-9.0));
    assert_eq!(
        quantitative_data_range(&dense, viewport(), &raw),
        Some(QuantitativeRange {
            minimum: -9.0,
            maximum: 0.0
        })
    );
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
            .map(|sample_index| rgv_model::CopyNumberSegment {
                sample_index,
                start: 100,
                end: 200,
                marker_count: Some(10),
                value: 1.0 + sample_index as f32,
            })
            .collect(),
        sample_sort_values: vec![None; 5],
        inferred_value_semantics: CopyNumberValueSemantics::Absolute,
        provenance: rgv_model::CopyNumberQueryProvenance::default(),
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
    let mutation = |sample_index, gene: &str| rgv_model::MutationRecord {
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
    let query = rgv_model::MutationQuery {
        headers: Vec::new(),
        samples: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
        records: vec![
            mutation(0, "TP53"),
            mutation(1, "KRAS"),
            mutation(1, "KRAS"),
        ],
        provenance: rgv_model::MutationQueryProvenance::default(),
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
    let endpoint = |contig: &str, start, end, name: &str| rgv_model::InteractionEndpoint {
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
    // One smooth arc primitive for the intrachromosomal record, not a polyline.
    let arcs = display
        .rectangles
        .iter()
        .filter(|rectangle| rectangle.shape == RectShape::JunctionArcUp)
        .collect::<Vec<_>>();
    assert_eq!(arcs.len(), 1);
    let baseline = TrackLayoutOptions::default().top + TrackLayoutOptions::default().height - 5.0;
    // Endpoint centres 115 and 185 at ten pixels per base, meeting the baseline.
    assert!((arcs[0].x - 150.0).abs() < 1e-3);
    assert!((arcs[0].width - 700.0).abs() < 1e-3);
    assert!((arcs[0].y + arcs[0].height - baseline).abs() < 1e-3);
    assert!(arcs[0].height > 3.0);
    assert_eq!(
        arcs[0].object_id,
        TrackLayoutOptions::default().object_id_base
    );
    assert!(display.rectangles.len() >= 5);
    assert_eq!(display.texts.len(), 2);
    assert!(display.texts[1].text.contains("chr2:501"));
    assert!(display.texts[1].text.contains("->"));
}

#[test]
fn cross_contig_undirected_interactions_use_a_neutral_join_label() {
    let endpoint = |contig: &str, start| rgv_model::InteractionEndpoint {
        contig: contig.to_owned(),
        start,
        end: start + 4,
        name: None,
        strand: Strand::Unstranded,
    };
    let record = InteractionRecord {
        source: endpoint("chr1", 120),
        target: endpoint("chr2", 500),
        name: None,
        score: Some(1.0),
        value: None,
        experiment: None,
        color: None,
        directional: false,
    };
    let (display, _) = layout_interactions(
        &[record],
        "chr1",
        viewport(),
        TrackLayoutOptions::default(),
        &InteractionTrackSettings::default(),
    );
    assert!(display.texts.iter().any(|text| text.text.contains("<->")));
}

#[test]
fn directed_interactions_point_to_the_semantic_target_in_either_genomic_order() {
    let endpoint = |start, strand| rgv_model::InteractionEndpoint {
        contig: "chr1".to_owned(),
        start,
        end: start + 4,
        name: None,
        strand,
    };
    let records = [
        InteractionRecord {
            source: endpoint(110, Strand::Forward),
            target: endpoint(180, Strand::Reverse),
            name: None,
            score: Some(1.0),
            value: None,
            experiment: None,
            color: None,
            directional: true,
        },
        InteractionRecord {
            source: endpoint(180, Strand::Reverse),
            target: endpoint(110, Strand::Forward),
            name: None,
            score: Some(1.0),
            value: None,
            experiment: None,
            color: None,
            directional: true,
        },
    ];
    for display_mode in [InteractionDisplayMode::Arcs, InteractionDisplayMode::Blocks] {
        let (display, _) = layout_interactions(
            &records,
            "chr1",
            viewport(),
            TrackLayoutOptions::default(),
            &InteractionTrackSettings {
                display_mode,
                show_labels: false,
                ..InteractionTrackSettings::default()
            },
        );
        let first_target = display.rectangles.iter().any(|rectangle| {
            rectangle.object_id == TrackLayoutOptions::default().object_id_base
                && rectangle.shape == RectShape::DirectionalTerminalRight
        });
        let second_target = display.rectangles.iter().any(|rectangle| {
            rectangle.object_id == TrackLayoutOptions::default().object_id_base + 1
                && rectangle.shape == RectShape::DirectionalTerminalLeft
        });
        assert!(first_target, "missing right target in {display_mode:?}");
        assert!(second_target, "missing left target in {display_mode:?}");
    }
}

#[test]
fn directed_interactions_keep_an_outward_target_marker_when_the_target_is_clipped() {
    let endpoint = |start, end| rgv_model::InteractionEndpoint {
        contig: "chr1".to_owned(),
        start,
        end,
        name: None,
        strand: Strand::Unstranded,
    };
    let records = [
        InteractionRecord {
            source: endpoint(130, 134),
            target: endpoint(80, 90),
            name: None,
            score: Some(1.0),
            value: None,
            experiment: None,
            color: None,
            directional: true,
        },
        InteractionRecord {
            source: endpoint(160, 164),
            target: endpoint(210, 220),
            name: None,
            score: Some(1.0),
            value: None,
            experiment: None,
            color: None,
            directional: true,
        },
    ];
    let (display, _) = layout_interactions(
        &records,
        "chr1",
        viewport(),
        TrackLayoutOptions::default(),
        &InteractionTrackSettings {
            show_labels: false,
            ..InteractionTrackSettings::default()
        },
    );
    let object_id_base = TrackLayoutOptions::default().object_id_base;
    assert!(display.rectangles.iter().any(|rectangle| {
        rectangle.object_id == object_id_base && rectangle.shape == RectShape::ChevronLeft
    }));
    assert!(display.rectangles.iter().any(|rectangle| {
        rectangle.object_id == object_id_base + 1 && rectangle.shape == RectShape::ChevronRight
    }));
}

#[test]
fn undirected_interactions_use_endpoint_strands_without_inventing_source_target_direction() {
    let record = InteractionRecord {
        source: rgv_model::InteractionEndpoint {
            contig: "chr1".to_owned(),
            start: 110,
            end: 114,
            name: None,
            strand: Strand::Forward,
        },
        target: rgv_model::InteractionEndpoint {
            contig: "chr1".to_owned(),
            start: 180,
            end: 184,
            name: None,
            strand: Strand::Reverse,
        },
        name: None,
        score: Some(1.0),
        value: None,
        experiment: None,
        color: None,
        directional: false,
    };
    let (display, _) = layout_interactions(
        &[record],
        "chr1",
        viewport(),
        TrackLayoutOptions::default(),
        &InteractionTrackSettings {
            show_labels: false,
            ..InteractionTrackSettings::default()
        },
    );
    assert!(
        display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::DirectionalTerminalRight)
    );
    assert!(
        display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::DirectionalTerminalLeft)
    );
}

#[test]
fn interaction_layout_obeys_filter_fixed_scale_and_hard_record_cap() {
    let records = (0..MAX_RENDERED_INTERACTIONS + 10)
        .map(|index| InteractionRecord {
            source: rgv_model::InteractionEndpoint {
                contig: "chr1".to_owned(),
                start: 100 + u64::try_from(index % 20).unwrap(),
                end: 101 + u64::try_from(index % 20).unwrap(),
                name: None,
                strand: Strand::Unstranded,
            },
            target: rgv_model::InteractionEndpoint {
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
fn interaction_cap_counts_only_records_drawable_in_the_viewport() {
    let record = |contig: &str, start: u64, target: u64| InteractionRecord {
        source: rgv_model::InteractionEndpoint {
            contig: contig.to_owned(),
            start,
            end: start + 1,
            name: None,
            strand: Strand::Unstranded,
        },
        target: rgv_model::InteractionEndpoint {
            contig: contig.to_owned(),
            start: target,
            end: target + 1,
            name: None,
            strand: Strand::Unstranded,
        },
        name: None,
        score: Some(10.0),
        value: None,
        experiment: None,
        color: None,
        directional: false,
    };
    // More than the cap of other-contig and off-screen records come first in
    // file order; the fifty visible records follow them.
    let mut records = (0..MAX_RENDERED_INTERACTIONS as u64)
        .map(|index| record("chr2", 100 + index % 50, 150))
        .collect::<Vec<_>>();
    records.extend(
        (0..MAX_RENDERED_INTERACTIONS as u64).map(|index| record("chr1", 5_000 + index, 6_000)),
    );
    let first_visible = records.len();
    records.extend((0..50_u64).map(|index| record("chr1", 110 + index, 190)));
    let settings = InteractionTrackSettings {
        show_labels: false,
        ..InteractionTrackSettings::default()
    };
    let options = TrackLayoutOptions::default();
    let (display, stats) = layout_interactions(&records, "chr1", viewport(), options, &settings);
    assert_eq!(stats.rendered_records, 50);
    assert_eq!(stats.hidden_records, records.len() - 50);
    let arcs = display
        .rectangles
        .iter()
        .filter(|rectangle| rectangle.shape == RectShape::JunctionArcUp)
        .collect::<Vec<_>>();
    assert_eq!(arcs.len(), 50);
    assert_eq!(
        arcs[0].object_id,
        options.object_id_base + u32::try_from(first_visible).unwrap()
    );
}

#[test]
fn interaction_arcs_with_distant_endpoints_stay_bounded_and_finite() {
    let endpoint = |start: u64| rgv_model::InteractionEndpoint {
        contig: "chr1".to_owned(),
        start,
        end: start + 1,
        name: None,
        strand: Strand::Unstranded,
    };
    let records = [InteractionRecord {
        source: endpoint(0),
        target: endpoint(30_000_000_000),
        name: None,
        score: Some(1.0),
        value: None,
        experiment: None,
        color: None,
        directional: false,
    }];
    let (display, stats) = layout_interactions(
        &records,
        "chr1",
        viewport(),
        TrackLayoutOptions::default(),
        &InteractionTrackSettings::default(),
    );
    assert_eq!(stats.rendered_records, 1);
    let arc = display
        .rectangles
        .iter()
        .find(|rectangle| rectangle.shape == RectShape::JunctionArcUp)
        .unwrap();
    // The arc still spans the whole viewport but within a bounded margin.
    assert!(arc.x < 0.0 && arc.x >= -4_000.0);
    assert!(arc.x + arc.width > 1_000.0 && arc.x + arc.width <= 5_000.0);
    assert!(display.rectangles.iter().all(|rectangle| {
        rectangle.x.is_finite() && rectangle.width.is_finite() && rectangle.x.abs() <= 5_000.0
    }));
}

fn plain_feature(start: u64, end: u64, name: &str, feature_type: Option<&str>) -> FeatureRecord {
    FeatureRecord {
        contig: "chr1".to_owned(),
        start,
        end,
        name: Some(name.to_owned()),
        feature_type: feature_type.map(ToOwned::to_owned),
        score: None,
        strand: Strand::Unstranded,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    }
}

/// Top of the row holding record `index`, read back from its primitives.
fn feature_row_top(
    display: &DisplayList,
    options: TrackLayoutOptions,
    index: usize,
) -> Option<f32> {
    let object_id = options.object_id_base + u32::try_from(index).unwrap();
    display
        .rectangles
        .iter()
        .filter(|rectangle| rectangle.object_id == object_id)
        .map(|rectangle| rectangle.y)
        .min_by(f32::total_cmp)
}

#[test]
fn feature_rows_are_stable_while_panning_and_independent_of_input_order() {
    let options = TrackLayoutOptions {
        maximum_rows: 4,
        height: 96.0,
        ..TrackLayoutOptions::default()
    };
    let features = [
        plain_feature(100, 150, "a", None),
        plain_feature(140, 190, "b", None),
        plain_feature(160, 199, "c", None),
    ];
    let pan = |start: u64| {
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), start, start + 100).unwrap(),
            1_000.0,
        )
        .unwrap()
    };
    let (before, _) = layout_features(&features, pan(100), options);
    // After panning, `a` has left the viewport. Packing only the visible
    // features would promote `b` to the first row and push `c` down.
    let (after, stats) = layout_features(&features, pan(155), options);
    assert_eq!(stats.rendered_records, 2);
    assert_eq!(feature_row_top(&after, options, 0), None);
    for index in [1, 2] {
        assert_eq!(
            feature_row_top(&before, options, index),
            feature_row_top(&after, options, index),
            "feature {index} changed rows while panning"
        );
    }
    assert!(feature_row_top(&before, options, 1) > feature_row_top(&before, options, 0));
    assert_eq!(
        feature_row_top(&before, options, 2),
        feature_row_top(&before, options, 0)
    );

    // The same records in reverse file order occupy the same rows, and none
    // is lost to a row that an earlier, later-starting record already claimed.
    let reversed = [
        features[2].clone(),
        features[1].clone(),
        features[0].clone(),
    ];
    let (shuffled, stats) = layout_features(&reversed, pan(100), options);
    assert_eq!(stats.hidden_records, 0);
    for (index, original) in [(0, 2), (1, 1), (2, 0)] {
        assert_eq!(
            feature_row_top(&shuffled, options, index),
            feature_row_top(&before, options, original)
        );
    }
}

#[test]
fn abutting_features_are_padded_onto_separate_rows() {
    let options = TrackLayoutOptions {
        maximum_rows: 4,
        ..TrackLayoutOptions::default()
    };
    // One base per pixel: features must be two pixels apart to share a row.
    let wide = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 0, 1_000).unwrap(),
        1_000.0,
    )
    .unwrap();
    let features = [
        plain_feature(100, 200, "left", None),
        plain_feature(200, 300, "abutting", None),
        plain_feature(202, 300, "separated", None),
    ];
    let (display, _) = layout_features(&features, wide, options);
    let row = |index| feature_row_top(&display, options, index).unwrap();
    assert!(row(1) > row(0));
    assert!((row(2) - row(0)).abs() < f32::EPSILON);
}

#[test]
fn features_that_fit_no_row_leave_a_visible_overflow_indicator() {
    let options = TrackLayoutOptions {
        maximum_rows: 2,
        ..TrackLayoutOptions::default()
    };
    let mut features = (0..5)
        .map(|index| plain_feature(120 + index, 140 + index, "stacked", None))
        .collect::<Vec<_>>();
    features.push(plain_feature(180, 190, "clear", None));
    let (display, stats) = layout_features(&features, viewport(), options);
    assert_eq!(stats.rendered_records, 3);
    assert_eq!(stats.hidden_records, 3);
    let bottom = options.top + options.height;
    let indicators = display
        .rectangles
        .iter()
        .filter(|rectangle| {
            rectangle.object_id == 0 && (rectangle.y + rectangle.height - bottom).abs() < 1e-3
        })
        .collect::<Vec<_>>();
    // The three hidden features 122..142, 123..143, 124..144 merge into one strip.
    assert_eq!(indicators.len(), 1);
    assert!((indicators[0].x - 220.0).abs() < 1e-3);
    assert!((indicators[0].width - 220.0).abs() < 1e-3);
    assert_eq!(stats.primitive_count, display.rectangles.len());

    // Nothing is marked when everything fits.
    let (display, stats) = layout_features(&features[4..], viewport(), options);
    assert_eq!(stats.hidden_records, 0);
    assert!(
        display
            .rectangles
            .iter()
            .all(|rectangle| rectangle.object_id != 0)
    );
}

#[test]
fn named_features_without_a_feature_type_are_labelled() {
    let options = TrackLayoutOptions {
        maximum_rows: 2,
        ..TrackLayoutOptions::default()
    };
    let features = [
        plain_feature(110, 150, "peak_1", None),
        plain_feature(160, 190, "exon_part", Some("exon")),
        plain_feature(160, 190, "GENE", Some("gene")),
    ];
    let (display, _) = layout_features(&features, viewport(), options);
    let labels = display
        .texts
        .iter()
        .map(|text| text.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(labels, ["peak_1", "GENE"]);

    let mut unnamed = plain_feature(110, 150, "", None);
    unnamed.name = None;
    let (display, _) = layout_features(&[unnamed], viewport(), options);
    assert!(display.texts.is_empty());
}

#[test]
fn layout_object_ids_never_saturate_onto_another_record() {
    let options = TrackLayoutOptions {
        object_id_base: u32::MAX - 1,
        maximum_rows: 4,
        ..TrackLayoutOptions::default()
    };
    let features = [
        plain_feature(110, 120, "a", None),
        plain_feature(130, 140, "b", None),
        plain_feature(150, 160, "c", None),
        plain_feature(170, 180, "d", None),
    ];
    let (display, stats) = layout_features(&features, viewport(), options);
    assert_eq!(stats.rendered_records, 4);
    let ids = display
        .rectangles
        .iter()
        .map(|rectangle| rectangle.object_id)
        .collect::<std::collections::BTreeSet<_>>();
    // Records 2 and 3 no longer both claim u32::MAX; they are unpickable.
    assert_eq!(
        ids.into_iter().collect::<Vec<_>>(),
        [0, u32::MAX - 1, u32::MAX]
    );
    let leftmost = display
        .rectangles
        .iter()
        .filter(|rectangle| rectangle.object_id == u32::MAX)
        .map(|rectangle| rectangle.x)
        .fold(f32::INFINITY, f32::min);
    assert!((leftmost - 300.0).abs() < 1e-3);
}

#[test]
fn variants_extending_beyond_the_viewport_use_clipped_geometry() {
    let variant = |start: u64, end: u64| VariantRecord {
        contig: "chr1".to_owned(),
        start,
        end,
        id: None,
        reference: b"N".to_vec(),
        alternates: vec![b"<DEL>".to_vec()],
        quality: None,
        filters: Vec::new(),
        info: Vec::new(),
        genotypes: Vec::new(),
        variant_types: vec![rgv_model::VariantType::Complex],
        breakends: Vec::new(),
        breakpoint_orientations: Vec::new(),
    };
    let options = TrackLayoutOptions {
        selected_record: Some(0),
        ..TrackLayoutOptions::default()
    };
    let settings = VariantTrackSettings::default();
    // A deletion that starts far to the left and ends far to the right.
    let (display, stats) = layout_variants(
        &[variant(0, 30_000_000_000)],
        viewport(),
        options,
        &settings,
    );
    assert_eq!(stats.rendered_records, 1);
    let baseline = options.top + options.height - 4.0;
    let span = display
        .rectangles
        .iter()
        .find(|rectangle| (rectangle.y - baseline).abs() < 1e-3)
        .unwrap();
    assert!((span.x - 0.0).abs() < 1e-3);
    assert!((span.width - 1_000.0).abs() < 1e-3);
    for rectangle in &display.rectangles {
        assert!(rectangle.x >= -10.0 && rectangle.x + rectangle.width <= 1_010.0);
    }
    // The start marker is not drawn at the clipped edge it does not occupy.
    assert!(
        display
            .rectangles
            .iter()
            .all(|rectangle| (rectangle.y - (options.top + 1.0)).abs() > 1e-3)
    );

    // A variant starting inside the viewport keeps its marker and clips its tail.
    let (display, _) = layout_variants(
        &[variant(150, 30_000_000_000)],
        viewport(),
        TrackLayoutOptions::default(),
        &settings,
    );
    let head = display
        .rectangles
        .iter()
        .find(|rectangle| (rectangle.y - (options.top + 1.0)).abs() < 1e-3)
        .unwrap();
    assert!((head.x - 496.0).abs() < 1e-3);
    let span = display
        .rectangles
        .iter()
        .find(|rectangle| (rectangle.y - baseline).abs() < 1e-3)
        .unwrap();
    assert!((span.x - 500.0).abs() < 1e-3);
    assert!((span.width - 500.0).abs() < 1e-3);
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
        genotypes: vec![rgv_model::VariantGenotype {
            sample_index: 0,
            alleles: vec![
                rgv_model::VariantGenotypeAllele {
                    index: Some(0),
                    phased: false,
                },
                rgv_model::VariantGenotypeAllele {
                    index: Some(1),
                    phased: false,
                },
            ],
            fields: Vec::new(),
        }],
        variant_types: vec![rgv_model::VariantType::Snv],
        breakends: Vec::new(),
        breakpoint_orientations: Vec::new(),
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
fn breakend_and_symbolic_sv_orientation_reaches_layout_shapes() {
    let breakend = VariantRecord {
        contig: "chr1".to_owned(),
        start: 120,
        end: 121,
        id: Some("bnd".to_owned()),
        reference: b"N".to_vec(),
        alternates: vec![b"N]chr1:181]".to_vec()],
        quality: None,
        filters: Vec::new(),
        info: Vec::new(),
        genotypes: Vec::new(),
        variant_types: vec![rgv_model::VariantType::Breakend],
        breakends: vec![rgv_model::BreakendPartner {
            alternate_index: 0,
            contig: "chr1".to_owned(),
            position: 180,
            local_join_after_position: true,
            partner_join_after_position: false,
        }],
        breakpoint_orientations: Vec::new(),
    };
    let symbolic = VariantRecord {
        start: 130,
        end: 170,
        id: Some("inv".to_owned()),
        alternates: vec![b"<INV>".to_vec()],
        variant_types: vec![rgv_model::VariantType::Inversion],
        breakends: Vec::new(),
        breakpoint_orientations: vec![rgv_model::BreakpointOrientation {
            local_join_after_position: false,
            partner_join_after_position: true,
            support: Some(8),
        }],
        ..breakend.clone()
    };
    let (display, _) = layout_variants(
        &[breakend, symbolic],
        viewport(),
        TrackLayoutOptions::default(),
        &VariantTrackSettings::default(),
    );
    assert!(
        display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::BreakendJoinAfter)
    );
    assert!(
        display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.shape == RectShape::BreakendJoinBefore)
    );
}

#[test]
fn every_breakend_join_side_combination_reaches_both_display_endpoints() {
    let join_sides = [(false, false), (false, true), (true, false), (true, true)];
    let records = join_sides
        .iter()
        .enumerate()
        .map(
            |(index, &(local_join_after_position, partner_join_after_position))| {
                let start = 110 + u64::try_from(index).unwrap() * 10;
                let partner_position = 170 + u64::try_from(index).unwrap() * 5;
                VariantRecord {
                    contig: "chr1".to_owned(),
                    start,
                    end: start + 1,
                    id: Some(format!("bnd-{index}")),
                    reference: b"N".to_vec(),
                    alternates: vec![b"N]chr1:181]".to_vec()],
                    quality: None,
                    filters: Vec::new(),
                    info: Vec::new(),
                    genotypes: Vec::new(),
                    variant_types: vec![rgv_model::VariantType::Breakend],
                    breakends: vec![rgv_model::BreakendPartner {
                        alternate_index: 0,
                        contig: "chr1".to_owned(),
                        position: partner_position,
                        local_join_after_position,
                        partner_join_after_position,
                    }],
                    breakpoint_orientations: Vec::new(),
                }
            },
        )
        .collect::<Vec<_>>();
    let options = TrackLayoutOptions::default();
    let (display, _) = layout_variants(
        &records,
        viewport(),
        options,
        &VariantTrackSettings::default(),
    );

    for (index, &(local_after, partner_after)) in join_sides.iter().enumerate() {
        let object_id = options.object_id_base + u32::try_from(index).unwrap();
        let shapes = display
            .rectangles
            .iter()
            .filter(|rectangle| rectangle.object_id == object_id)
            .map(|rectangle| rectangle.shape)
            .collect::<Vec<_>>();
        let local_shape = if local_after {
            RectShape::BreakendJoinAfter
        } else {
            RectShape::BreakendJoinBefore
        };
        let partner_shape = if partner_after {
            RectShape::BreakendJoinAfter
        } else {
            RectShape::BreakendJoinBefore
        };
        assert!(shapes.contains(&local_shape));
        assert!(shapes.contains(&partner_shape));
        if local_shape == partner_shape {
            assert!(shapes.iter().filter(|shape| **shape == local_shape).count() >= 2);
        }
    }
}

#[test]
fn offscreen_breakend_label_keeps_both_join_sides_inside_the_viewport() {
    let record = VariantRecord {
        contig: "chr1".to_owned(),
        start: 100,
        end: 101,
        id: Some("edge-bnd".to_owned()),
        reference: b"N".to_vec(),
        alternates: vec![b"N]chr1:301]".to_vec()],
        quality: None,
        filters: Vec::new(),
        info: Vec::new(),
        genotypes: Vec::new(),
        variant_types: vec![rgv_model::VariantType::Breakend],
        breakends: vec![rgv_model::BreakendPartner {
            alternate_index: 0,
            contig: "chr1".to_owned(),
            position: 300,
            local_join_after_position: true,
            partner_join_after_position: false,
        }],
        breakpoint_orientations: Vec::new(),
    };
    let (display, _) = layout_variants(
        &[record],
        viewport(),
        TrackLayoutOptions::default(),
        &VariantTrackSettings::default(),
    );
    let label = display.texts.first().expect("offscreen partner label");
    assert!(label.text.contains("ALT 1 after <-> chr1:301 before"));
    let approximate_half_width = label.text.chars().count() as f32 * label.font_size * 0.31;
    assert!(label.x >= approximate_half_width);
    assert!(label.x + approximate_half_width <= viewport().pixel_width() as f32);
    assert!(display.rectangles.iter().any(|rectangle| {
        rectangle.shape == RectShape::BreakendJoinBefore
            && rectangle.x + rectangle.width >= viewport().pixel_width() as f32
    }));
}

#[test]
fn multiple_offscreen_and_cross_contig_breakend_partners_get_bounded_edge_markers() {
    let record = VariantRecord {
        contig: "chr1".to_owned(),
        start: 150,
        end: 151,
        id: Some("multi-bnd".to_owned()),
        reference: b"N".to_vec(),
        alternates: vec![
            b"]chr1:51]N".to_vec(),
            b"N[chr1:251[".to_vec(),
            b"N]chr2:401]".to_vec(),
        ],
        quality: None,
        filters: Vec::new(),
        info: Vec::new(),
        genotypes: Vec::new(),
        variant_types: vec![rgv_model::VariantType::Breakend],
        breakends: vec![
            rgv_model::BreakendPartner {
                alternate_index: 0,
                contig: "chr1".to_owned(),
                position: 50,
                local_join_after_position: false,
                partner_join_after_position: true,
            },
            rgv_model::BreakendPartner {
                alternate_index: 1,
                contig: "chr1".to_owned(),
                position: 250,
                local_join_after_position: true,
                partner_join_after_position: false,
            },
            rgv_model::BreakendPartner {
                alternate_index: 2,
                contig: "chr2".to_owned(),
                position: 400,
                local_join_after_position: true,
                partner_join_after_position: true,
            },
        ],
        breakpoint_orientations: Vec::new(),
    };
    let (display, _) = layout_variants(
        &[record],
        viewport(),
        TrackLayoutOptions::default(),
        &VariantTrackSettings::default(),
    );
    assert_eq!(display.texts.len(), 3);
    assert!(display.texts[0].text.starts_with("ALT 1 before"));
    assert!(display.texts[1].text.starts_with("ALT 2 after"));
    assert!(display.texts[2].text.starts_with("ALT 3 after"));
    assert!(display.rectangles.iter().any(|rectangle| {
        rectangle.x == 0.0 && rectangle.shape == RectShape::BreakendJoinAfter
    }));
    assert!(display.rectangles.iter().any(|rectangle| {
        rectangle.x + rectangle.width >= viewport().pixel_width() as f32
            && rectangle.shape == RectShape::BreakendJoinBefore
    }));
    assert!(display.rectangles.iter().any(|rectangle| {
        rectangle.x + rectangle.width >= viewport().pixel_width() as f32
            && rectangle.shape == RectShape::BreakendJoinAfter
    }));
}

#[test]
fn gwas_layout_transforms_p_values_and_preserves_selectable_point_identity() {
    let records = vec![
        rgv_model::GwasRecord {
            contig: "chr1".to_owned(),
            position: 125,
            identifier: "rs-low".to_owned(),
            p_value: 0.1,
            minus_log10_p: 1.0,
            fields: Vec::new(),
        },
        rgv_model::GwasRecord {
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

/// Timing probe, not a correctness test: run with
/// `cargo test --release -p rgv-layout -- --ignored feature_layout_cost --nocapture`.
#[test]
#[ignore = "timing probe"]
fn feature_layout_cost() {
    for count in [500_u64, 5_000, 50_000] {
        let features = (0..count)
            .map(|index| {
                let start = index * 40;
                FeatureRecord {
                    contig: "chr1".to_owned(),
                    start,
                    end: start + 900,
                    name: Some(format!("GENE{index}")),
                    feature_type: Some("transcript".to_owned()),
                    score: None,
                    strand: Strand::Forward,
                    blocks: (0..6)
                        .map(|block| (start + block * 150, start + block * 150 + 80))
                        .collect(),
                    coding_interval: Some((start + 100, start + 800)),
                    peak: None,
                    attributes: Vec::new(),
                }
            })
            .collect::<Vec<_>>();
        let viewport = Viewport::new(
            GenomicInterval::new(ContigId::new(0), 0, count * 40 + 900).unwrap(),
            1280.0,
        )
        .unwrap();
        let started = std::time::Instant::now();
        let iterations = 20;
        for _ in 0..iterations {
            let (display, _) = layout_features(&features, viewport, TrackLayoutOptions::default());
            std::hint::black_box(display);
        }
        println!(
            "{count} features: {:.3} ms per layout",
            started.elapsed().as_secs_f64() * 1e3 / f64::from(iterations)
        );
    }
}
