use super::super::*;
use rgv_layout::RectShape;

#[test]
fn pathological_cigar_is_summarized_without_losing_exact_copy_text() {
    let cigar = (0..70_000)
        .map(|index| rgv_hts::CigarOp {
            kind: if index % 2 == 0 {
                CigarOpKind::Match
            } else {
                CigarOpKind::Insertion
            },
            len: 1,
        })
        .collect::<Vec<_>>();
    let exact = format_cigar(&cigar);
    let summary = inspector_cigar_value(&cigar, &exact);
    assert!(summary.starts_with("70000 operations"));
    assert!(summary.contains("70000 bp query span"));
    assert!(summary.contains("35000 bp reference span"));
    assert!(summary.len() < 320);
    assert!(exact.len() > 100_000);
}

#[test]
fn ordinary_cigar_remains_inline() {
    let cigar = vec![rgv_hts::CigarOp {
        kind: CigarOpKind::Match,
        len: 100,
    }];
    let exact = format_cigar(&cigar);
    assert_eq!(inspector_cigar_value(&cigar, &exact), "100M");
}

#[test]
fn pileup_pointer_maps_canvas_pixels_to_an_exact_query_bin() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch: rgv_hts::AlignmentBatch::default(),
        pileup: rgv_hts::PileupSummary::from_depths([1; 4]),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats::default(),
    };
    let viewport = Viewport::new(interval, 400.0).unwrap();
    let canvas = egui::Rect::from_min_max(egui::pos2(50.0, 100.0), egui::pos2(450.0, 300.0));
    assert_eq!(
        pileup_position_from_pointer(&query, viewport, canvas, egui::pos2(250.0, 180.0)),
        Some(102)
    );
    assert_eq!(
        pileup_position_from_pointer(&query, viewport, canvas, egui::pos2(20.0, 180.0)),
        None
    );
}

#[test]
fn modification_pointer_maps_canvas_pixels_even_when_only_errors_were_aggregated() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 104).unwrap();
    let mut pileup = rgv_hts::PileupSummary::from_depths([1; 4]);
    pileup.modifications.bin_count = 4;
    pileup.modifications.provenance.malformed_records = 1;
    let query = AlignmentQuery {
        interval,
        header: Arc::default(),
        batch: rgv_hts::AlignmentBatch::default(),
        pileup,
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats::default(),
    };
    let viewport = Viewport::new(interval, 400.0).unwrap();
    let canvas = egui::Rect::from_min_max(egui::pos2(50.0, 100.0), egui::pos2(450.0, 300.0));
    assert_eq!(
        modification_position_from_pointer(&query, viewport, canvas, egui::pos2(250.0, 180.0)),
        Some(102)
    );
    assert_eq!(
        modification_position_from_pointer(&query, viewport, canvas, egui::pos2(20.0, 180.0)),
        None
    );
}

fn test_junction_arc_target(
    track_id: TrackId,
    start: u64,
    component: JunctionArcComponent,
    primitive: RectPrimitive,
    draw_order: u32,
    content_revision: u64,
) -> ComposedJunctionArcTarget {
    ComposedJunctionArcTarget {
        key: JunctionArcKey {
            track_id,
            start,
            end: start + 100,
            component,
        },
        primitive,
        stroke_width: junction_arc_stroke_width(primitive.height),
        draw_order,
        interaction_token: draw_order + 1,
        content_revision,
    }
}

fn test_up_arc(y: f32, height: f32) -> RectPrimitive {
    RectPrimitive {
        x: 20.0,
        y,
        width: 100.0,
        height,
        color: Rgba::new(0.84, 0.30, 0.38, 0.82),
        object_id: 0,
        shape: RectShape::JunctionArcUp,
    }
}

fn arc_apex(primitive: RectPrimitive) -> egui::Pos2 {
    let stroke = junction_arc_stroke_width(primitive.height);
    let y = if primitive.shape.is_junction_arc_up() {
        primitive.y + stroke * 0.5
    } else {
        primitive.y + primitive.height - stroke * 0.5
    };
    egui::pos2(primitive.x + primitive.width * 0.5, y)
}

#[test]
fn junction_selection_uses_pointer_y_and_nearest_visible_curve() {
    let track = TrackId::new(7);
    let broad = test_junction_arc_target(
        track,
        100,
        JunctionArcComponent::Positive,
        test_up_arc(0.0, 40.0),
        1,
        9,
    );
    let shallow = test_junction_arc_target(
        track,
        200,
        JunctionArcComponent::Unknown,
        test_up_arc(20.0, 12.0),
        2,
        9,
    );
    let targets = [broad, shallow];

    assert_eq!(
        junction_arc_target_at_point(&targets, track, arc_apex(shallow.primitive), None, 9)
            .unwrap()
            .key,
        shallow.key,
        "a visually shallow curve must not be stolen by a broader prominent arc"
    );
    assert_eq!(
        junction_arc_target_at_point(&targets, track, arc_apex(broad.primitive), None, 9)
            .unwrap()
            .key,
        broad.key,
        "the same screen X must resolve differently at the broad curve's Y"
    );
}

#[test]
fn junction_selection_uses_draw_order_then_semantic_key_for_exact_ties() {
    let track = TrackId::new(7);
    let primitive = test_up_arc(0.0, 20.0);
    let lower =
        test_junction_arc_target(track, 100, JunctionArcComponent::Positive, primitive, 1, 9);
    let upper =
        test_junction_arc_target(track, 200, JunctionArcComponent::Positive, primitive, 2, 9);
    assert_eq!(
        junction_arc_target_at_point(&[lower, upper], track, arc_apex(primitive), None, 9)
            .unwrap()
            .key,
        upper.key
    );

    let semantic_first = ComposedJunctionArcTarget {
        draw_order: 2,
        ..lower
    };
    assert_eq!(
        junction_arc_target_at_point(
            &[upper, semantic_first],
            track,
            arc_apex(primitive),
            None,
            9,
        )
        .unwrap()
        .key,
        semantic_first.key
    );
}

#[test]
fn junction_hover_hysteresis_retains_then_transfers_the_current_target() {
    let track = TrackId::new(7);
    let first = test_junction_arc_target(
        track,
        100,
        JunctionArcComponent::Positive,
        test_up_arc(0.0, 16.0),
        1,
        9,
    );
    let coincident = test_junction_arc_target(
        track,
        200,
        JunctionArcComponent::Unknown,
        first.primitive,
        2,
        9,
    );
    let current = JunctionArcHover {
        key: first.key,
        interaction_token: first.interaction_token,
        distance_points: 0.0,
        content_revision: 9,
    };
    assert_eq!(
        junction_arc_target_at_point(
            &[first, coincident],
            track,
            arc_apex(first.primitive),
            Some(current),
            9,
        )
        .unwrap()
        .key,
        first.key,
        "a current target within its exit radius must not flicker at a tie"
    );

    let distant = test_junction_arc_target(
        track,
        300,
        JunctionArcComponent::Negative,
        RectPrimitive {
            y: 50.0,
            shape: RectShape::JunctionArcDown,
            ..first.primitive
        },
        3,
        9,
    );
    assert_eq!(
        junction_arc_target_at_point(
            &[first, distant],
            track,
            arc_apex(distant.primitive),
            Some(current),
            9,
        )
        .unwrap()
        .key,
        distant.key,
        "leaving the exit radius must transfer acquisition promptly"
    );
}

#[test]
fn junction_selection_rejects_other_tracks_and_stale_revisions() {
    let track = TrackId::new(7);
    let target = test_junction_arc_target(
        track,
        100,
        JunctionArcComponent::Positive,
        test_up_arc(0.0, 20.0),
        1,
        9,
    );
    let point = arc_apex(target.primitive);
    assert!(junction_arc_target_at_point(&[target], TrackId::new(8), point, None, 9).is_none());
    assert!(junction_arc_target_at_point(&[target], track, point, None, 10).is_none());
    assert!(junction_arc_target_at_point(&[], track, point, None, 9).is_none());
}

#[test]
fn junction_probe_distinguishes_published_targets_from_a_curve_distance_miss() {
    let track = TrackId::new(7);
    let target = test_junction_arc_target(
        track,
        100,
        JunctionArcComponent::Positive,
        test_up_arc(0.0, 20.0),
        1,
        9,
    );
    let miss = egui::pos2(
        target.primitive.x + target.primitive.width * 0.5,
        target.primitive.y + target.primitive.height,
    );

    let result = junction_arc_hit_test_at_point(&[target], track, miss, None, 9);

    assert_eq!(result.gate, SashimiHitTestGate::Tested);
    assert_eq!(result.lane_track_id, Some(track));
    assert_eq!(result.total_targets, 1);
    assert_eq!(result.track_targets, 1);
    assert_eq!(result.current_revision_targets, 1);
    assert_eq!(result.bounds_candidates, 1);
    assert_eq!(result.curve_evaluations, 1);
    assert!(result.hover.is_none());
    assert!(result.nearest_curve_distance.is_some());
    assert!(result.nearest_acquisition_radius.is_some());
    assert!(
        result.nearest_curve_distance.unwrap() > result.nearest_acquisition_radius.unwrap(),
        "a diagnostic miss must preserve the measured distance and acquisition threshold"
    );
}

#[test]
fn junction_inspector_names_the_focused_component_and_its_support() {
    let junction = rgv_hts::JunctionEvidence {
        positive_depth: 7,
        negative_depth: 3,
        unknown_depth: 2,
        ..rgv_hts::JunctionEvidence::default()
    };
    assert_eq!(
        crate::inspection::inspector_controller::focused_junction_component_value(
            JunctionArcComponent::Positive,
            &junction,
        ),
        "positive strand · 7 supporting reads"
    );
    assert_eq!(
        crate::inspection::inspector_controller::focused_junction_component_value(
            JunctionArcComponent::Negative,
            &junction,
        ),
        "negative strand · 3 supporting reads"
    );
    assert_eq!(
        crate::inspection::inspector_controller::focused_junction_component_value(
            JunctionArcComponent::Unknown,
            &junction,
        ),
        "unknown strand · 2 supporting reads"
    );
}

#[test]
fn opposite_strand_components_with_shared_endpoints_are_individually_acquired() {
    let track = TrackId::new(7);
    let up = test_junction_arc_target(
        track,
        100,
        JunctionArcComponent::Positive,
        test_up_arc(0.0, 20.0),
        1,
        9,
    );
    let down = test_junction_arc_target(
        track,
        100,
        JunctionArcComponent::Negative,
        RectPrimitive {
            shape: RectShape::JunctionArcDown,
            ..up.primitive
        },
        2,
        9,
    );
    assert_eq!(
        junction_arc_target_at_point(&[up, down], track, arc_apex(up.primitive), None, 9)
            .unwrap()
            .key
            .component,
        JunctionArcComponent::Positive
    );
    assert_eq!(
        junction_arc_target_at_point(&[up, down], track, arc_apex(down.primitive), None, 9)
            .unwrap()
            .key
            .component,
        JunctionArcComponent::Negative
    );
}

#[test]
fn thin_and_thick_junction_arcs_share_the_minimum_acquisition_comfort() {
    let track = TrackId::new(7);
    for (index, height) in [3.0, 40.0].into_iter().enumerate() {
        let target = test_junction_arc_target(
            track,
            100 + index as u64 * 200,
            JunctionArcComponent::Positive,
            test_up_arc(0.0, height),
            index as u32,
            9,
        );
        let mut point = arc_apex(target.primitive);
        point.y += JUNCTION_ARC_MINIMUM_ACQUISITION_RADIUS - 0.1;
        assert!(junction_arc_target_at_point(&[target], track, point, None, 9).is_some());
    }
}

#[test]
#[ignore = "release-only Sashimi hard-cap performance gate"]
fn maximum_density_junction_hit_test_stays_below_half_a_millisecond_at_p95() {
    const SAMPLE_COUNT: usize = 201;
    const P95_LIMIT_NANOS: u128 = 500_000;
    let track = TrackId::new(7);
    let primitive = test_up_arc(0.0, 40.0);
    let components = [
        JunctionArcComponent::Positive,
        JunctionArcComponent::Negative,
        JunctionArcComponent::Unknown,
    ];
    let mut targets = Vec::with_capacity(rgv_layout::MAX_RENDERED_JUNCTIONS * components.len());
    for index in 0..rgv_layout::MAX_RENDERED_JUNCTIONS {
        for (component_index, component) in components.into_iter().enumerate() {
            let draw_order = u32::try_from(index * components.len() + component_index).unwrap();
            targets.push(test_junction_arc_target(
                track,
                100 + index as u64 * 200,
                component,
                RectPrimitive {
                    shape: if component == JunctionArcComponent::Negative {
                        RectShape::JunctionArcDown
                    } else {
                        RectShape::JunctionArcUp
                    },
                    ..primitive
                },
                draw_order,
                9,
            ));
        }
    }
    let point = arc_apex(primitive);
    std::hint::black_box(junction_arc_hit_test_at_point(
        &targets, track, point, None, 9,
    ));

    let mut durations = [0_u128; SAMPLE_COUNT];
    for duration in &mut durations {
        let started = std::time::Instant::now();
        std::hint::black_box(junction_arc_hit_test_at_point(
            &targets, track, point, None, 9,
        ));
        *duration = started.elapsed().as_nanos();
    }
    durations.sort_unstable();
    let median = durations[(SAMPLE_COUNT - 1) / 2];
    let p95 = durations[((SAMPLE_COUNT - 1) * 95) / 100];
    let maximum = durations[SAMPLE_COUNT - 1];
    eprintln!(
        "Sashimi hard-cap hit test: median {:.3} ms, p95 {:.3} ms, max {:.3} ms",
        median as f64 / 1_000_000.0,
        p95 as f64 / 1_000_000.0,
        maximum as f64 / 1_000_000.0,
    );
    assert!(
        p95 < P95_LIMIT_NANOS,
        "hard-cap nearest-curve p95 was {:.3} ms",
        p95 as f64 / 1_000_000.0,
    );
}

#[test]
fn junction_annotation_requires_adjacent_exons_from_the_same_transcript() {
    let mut configs = TrackCollection::default();
    configs.push(TrackConfig::new(
        TrackId::new(7),
        "Genes".to_owned(),
        TrackKind::GeneModel,
        TrackSource::Local {
            path: "genes.gtf".to_owned(),
        },
    ));
    let exon = |start, end, transcript: &str, gene: &str| rgv_hts::FeatureRecord {
        contig: "chr1".to_owned(),
        start,
        end,
        name: Some(gene.to_owned()),
        feature_type: Some("exon".to_owned()),
        score: None,
        strand: rgv_hts::Strand::Forward,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: vec![
            ("transcript_id".to_owned(), transcript.to_owned()),
            ("gene_name".to_owned(), gene.to_owned()),
        ],
    };
    let snapshots = vec![TrackSnapshot {
        id: TrackId::new(7),
        name: "Genes".to_owned(),
        data: TrackSnapshotData::Features(vec![
            exon(100, 200, "tx1", "GENE1"),
            exon(300, 400, "tx1", "GENE1"),
            exon(500, 600, "tx2", "GENE2"),
        ]),
    }];
    let loaded = GenomicInterval::new(ContigId::new(0), 0, 1_000).unwrap();
    let viewport = GenomicInterval::new(ContigId::new(0), 50, 900).unwrap();
    let context = junction_annotation_context(&configs, &snapshots, Some(loaded), viewport);
    assert_eq!(
        context.status(200, 300),
        JunctionAnnotationStatus::Annotated
    );
    assert_eq!(context.labels(200, 300), &["GENE1"]);
    assert_eq!(context.status(400, 500), JunctionAnnotationStatus::Novel);

    let incomplete = junction_annotation_context(&configs, &snapshots, Some(viewport), viewport);
    assert_eq!(
        incomplete.status(400, 500),
        JunctionAnnotationStatus::Unknown
    );
}

#[test]
fn rna_exports_preserve_filters_annotation_scaling_and_bounds() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 400).unwrap();
    let query = Arc::new(AlignmentQuery {
        interval,
        header: Arc::default(),
        batch: rgv_hts::AlignmentBatch::default(),
        pileup: rgv_hts::PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary {
            junctions: vec![rgv_hts::JunctionEvidence {
                start: 200,
                end: 300,
                depth: 10,
                minimum_left_flank: 12,
                minimum_right_flank: 14,
                positive_depth: 10,
                tagged_positive_depth: 10,
                ..rgv_hts::JunctionEvidence::default()
            }],
            provenance: rgv_hts::JunctionProvenance {
                eligible_records: 20,
                spliced_records: 10,
                supporting_spliced_records: 10,
                junction_observations_examined: 10,
                junction_observations_retained: 10,
                minimum_flank: 8,
                normalization_mapped_records: Some(1_000_000),
                all_eligible_records: true,
                ..rgv_hts::JunctionProvenance::default()
            },
        },
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats::default(),
    });
    let filters = SessionFilters {
        minimum_junction_depth: 5,
        minimum_junction_flank: 8,
        junction_display_mode: JunctionDisplayMode::Expanded,
        sashimi_scale_mode: SashimiScaleMode::Shared,
        sashimi_count_mode: SashimiCountMode::CountsPerMillion,
        ..SessionFilters::default()
    };
    let track = SashimiExportTrack {
        id: TrackId::new(1),
        name: "RNA sample".to_owned(),
        query,
        filters,
    };
    let annotations = JunctionAnnotationContext {
        lookup: JunctionAnnotationLookup::new([(200, 300)], 1, true),
        labels: BTreeMap::from([((200, 300), vec!["GENE1".to_owned()])]),
        source_records_truncated: false,
    };
    let bed = junction_bed_export(&track, "chr1", interval, &annotations);
    assert!(bed.contains("# filters\tminimum_depth=5\tminimum_flank=8"));
    assert!(bed.contains("chr1\t200\t300\tjunction_201_300"));
    assert!(bed.contains("\tannotated\tGENE1\n"));
    assert!(bed.len() <= MAX_RNA_EXPORT_BYTES);

    let bundle = SashimiExportBundle {
        tracks: vec![
            track.clone(),
            SashimiExportTrack {
                id: TrackId::new(2),
                name: "RNA sample 2".to_owned(),
                ..track
            },
        ],
        omitted_tracks: 2,
        viewport: Viewport::new(interval, 800.0).unwrap(),
        contig_name: "chr&1".to_owned(),
        annotations,
    };
    assert_eq!(
        sashimi_export_shared_maximum(&bundle, SashimiCountMode::CountsPerMillion),
        Some(10.0)
    );
    let (display, width, height) = sashimi_export_display(&bundle);
    assert_eq!(width, 800);
    assert!(height > 300);
    assert!(!display.rectangles.is_empty());
    let svg = sashimi_svg_export(&display, width, height, &bundle);
    assert!(svg.contains("locus chr&amp;1:101-400"));
    assert!(svg.contains("RNA sample"));
    assert!(svg.ends_with("</svg>\n"));
    assert!(svg.len() <= MAX_RNA_EXPORT_BYTES);
}

#[test]
fn variant_picking_distance_and_genotype_formatting_preserve_half_open_phasing() {
    assert_eq!(genomic_distance_to_interval(99, 100, 110), 1);
    assert_eq!(genomic_distance_to_interval(100, 100, 110), 0);
    assert_eq!(genomic_distance_to_interval(109, 100, 110), 0);
    assert_eq!(genomic_distance_to_interval(110, 100, 110), 1);
    let genotype = rgv_hts::VariantGenotype {
        sample_index: 0,
        alleles: vec![
            rgv_hts::VariantGenotypeAllele {
                index: Some(0),
                phased: false,
            },
            rgv_hts::VariantGenotypeAllele {
                index: Some(1),
                phased: true,
            },
            rgv_hts::VariantGenotypeAllele {
                index: None,
                phased: false,
            },
        ],
        fields: Vec::new(),
    };
    assert_eq!(format_genotype(&genotype), "GT=0|1/.");
    assert_eq!(
        bounded_variant_allele(&vec![b'A'; VARIANT_ALLELE_PREVIEW_BASES + 7]),
        format!("{}… (+7 bases)", "A".repeat(VARIANT_ALLELE_PREVIEW_BASES))
    );
}

#[test]
fn variant_predicate_drafts_are_typed_and_genotype_groups_are_stable() {
    assert_eq!(
        build_variant_predicate(
            VariantFieldScope::Info,
            "AF",
            VariantPredicateOperator::GreaterOrEqual,
            "0.2",
            VariantMissingValueRule::Exclude,
        )
        .unwrap(),
        VariantFieldPredicate {
            scope: VariantFieldScope::Info,
            key: "AF".to_owned(),
            comparison: VariantFieldComparison::GreaterOrEqual(0.2),
            missing: VariantMissingValueRule::Exclude,
        }
    );
    assert!(
        build_variant_predicate(
            VariantFieldScope::Format,
            "DP",
            VariantPredicateOperator::GreaterThan,
            "not-a-number",
            VariantMissingValueRule::Include,
        )
        .is_err()
    );
    let genotype = rgv_hts::VariantGenotype {
        sample_index: 0,
        alleles: vec![
            rgv_hts::VariantGenotypeAllele {
                index: Some(1),
                phased: false,
            },
            rgv_hts::VariantGenotypeAllele {
                index: Some(1),
                phased: true,
            },
        ],
        fields: Vec::new(),
    };
    assert_eq!(variant_genotype_group_rank(&genotype), 2);
    assert_eq!(variant_genotype_group_label(&genotype), "Hom-alt");
}

#[test]
fn variant_alignment_summary_is_observational_and_allele_specific_when_exact() {
    let mut record = rgv_hts::VariantRecord {
        contig: "chr1".to_owned(),
        start: 100,
        end: 101,
        id: None,
        reference: b"A".to_vec(),
        alternates: vec![b"T".to_vec()],
        quality: None,
        filters: Vec::new(),
        info: Vec::new(),
        genotypes: Vec::new(),
        variant_types: vec![rgv_hts::VariantType::Snv],
        breakends: vec![rgv_hts::BreakendPartner {
            alternate_index: 0,
            contig: "chr2".to_owned(),
            position: 300,
            local_join_after_position: true,
            partner_join_after_position: false,
        }],
        breakpoint_orientations: vec![rgv_hts::BreakpointOrientation {
            local_join_after_position: false,
            partner_join_after_position: true,
            support: Some(8),
        }],
    };
    let bin = rgv_hts::PileupBin {
        alleles: rgv_hts::AlleleSupport {
            a: 8,
            t: 3,
            deletion: 2,
            insertion: 1,
            ..rgv_hts::AlleleSupport::default()
        },
        ..rgv_hts::PileupBin::default()
    };
    assert_eq!(
        variant_allele_evidence_summary(&record, 1, &bin),
        "ALT T: 3 of 11 base observations"
    );
    record.reference = b"AT".to_vec();
    record.alternates = vec![b"A".to_vec()];
    record.variant_types = vec![rgv_hts::VariantType::Deletion];
    assert_eq!(
        variant_allele_evidence_summary(&record, 1, &bin),
        "2 deletion observations"
    );
    let inspector = VariantInspectorSnapshot {
        track_name: "calls\tcohort".to_owned(),
        query: Arc::new(rgv_hts::VariantQuery {
            header: rgv_hts::VariantHeader {
                samples: vec!["sample-1".to_owned()],
                ..rgv_hts::VariantHeader::default()
            },
            records: vec![record.clone()],
            provenance: rgv_hts::VariantQueryProvenance::default(),
        }),
        record_index: 0,
        settings: VariantTrackSettings::default(),
    };
    let variant_tsv = variant_genotype_export_tsv(&inspector);
    assert!(variant_tsv.contains("# RGV selected variant and genotype export v1"));
    assert!(variant_tsv.contains("calls cohort"));
    assert!(variant_tsv.contains("breakend\t1\tafter\tchr2\t301\tbefore"));
    assert!(variant_tsv.contains("breakpoint_orientation\t1\tbefore\tafter\t8"));
    assert!(variant_tsv.len() <= MAX_VARIANT_EXPORT_BYTES);
    let details = variant_details_text(&variant_inspector_fields(&inspector, &record), &record);
    assert!(details.contains("local join after -> chr2:301 partner join before"));
    assert!(details.contains("local join before -> partner join after · support 8"));
    let evidence = VariantAlignmentEvidence {
        track_id: TrackId::new(1),
        track_name: "reads".to_owned(),
        position: 100,
        bin_start: 100,
        bin_end: 101,
        depth: 11,
        forward_depth: 6,
        reverse_depth: 5,
        allele_summary: "2 deletion observations".to_owned(),
        modification_series: 0,
        modification_calls: 0,
        pair_templates_sampled: 20,
        retained_records: 11,
        complete_individual_reads: true,
        complete_pileup: true,
    };
    let evidence_tsv = variant_evidence_export_tsv(&inspector, &[evidence]);
    assert!(evidence_tsv.contains("not an RGV variant call"));
    assert!(evidence_tsv.contains("reads\t101\t101\t11"));
    assert_eq!(
        variant_export_file_name(&inspector.query.records[0], "evidence"),
        "rgv-chr1-101-evidence.tsv"
    );
}

#[test]
fn read_event_selection_and_preview_follow_cigar_coordinates() {
    let mut bases = vec![b'A'; 5];
    bases.extend(std::iter::repeat_n(b'C', 1_000));
    bases.extend(std::iter::repeat_n(b'G', 5));
    bases.extend_from_slice(b"TTTT");
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 610,
        flags: rgv_hts::AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"event-read".to_vec(),
        cigar: vec![
            rgv_hts::CigarOp {
                kind: CigarOpKind::SequenceMatch,
                len: 5,
            },
            rgv_hts::CigarOp {
                kind: CigarOpKind::Insertion,
                len: 1_000,
            },
            rgv_hts::CigarOp {
                kind: CigarOpKind::SequenceMatch,
                len: 5,
            },
            rgv_hts::CigarOp {
                kind: CigarOpKind::Deletion,
                len: 500,
            },
            rgv_hts::CigarOp {
                kind: CigarOpKind::SoftClip,
                len: 4,
            },
        ],
        sequence: SequenceData::Present {
            qualities: Some(vec![40; bases.len()]),
            bases,
        },
        auxiliary_fields: Vec::new(),
    };

    assert_eq!(
        find_read_event(&record, None, 105.1, 0.5, true),
        Some(ReadEventHit {
            operation_index: 1,
            operation_offset: None,
        })
    );
    assert_eq!(
        find_read_event(&record, None, 200.0, 0.5, true),
        Some(ReadEventHit {
            operation_index: 3,
            operation_offset: None,
        })
    );
    assert_eq!(
        find_read_event(&record, None, 610.0, 0.5, true),
        Some(ReadEventHit {
            operation_index: 4,
            operation_offset: None,
        })
    );
    let insertion = read_event_details(&record, 1, None, None, "chr1").unwrap();
    assert_eq!(
        insertion.copy_sequence.as_deref().map(str::len),
        Some(1_000)
    );
    assert!(
        insertion
            .fields
            .iter()
            .any(|(label, value)| { label == "Sequence preview" && value.contains("+880 bases") })
    );
    let deletion = read_event_details(&record, 3, None, None, "chr1").unwrap();
    assert!(deletion.copy_sequence.is_none());
    assert!(
        deletion
            .fields
            .contains(&("Event locus".to_owned(), "chr1:111-610".to_owned()))
    );
    let clip = read_event_details(&record, 4, None, None, "chr1").unwrap();
    assert_eq!(clip.copy_sequence.as_deref(), Some("TTTT"));
    assert_eq!(
        aligned_event_sequence(b"AACG", Some(&[10, 20, 30, 40]), 0..2),
        (b"AA".to_vec(), Some(vec![10, 20]))
    );
}

#[test]
fn individual_substitution_selection_reports_exact_read_and_reference_evidence() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 110).unwrap();
    let reference = ReferenceSlice {
        interval,
        bases: vec![b'A'; 10],
    };
    let mut observed = vec![b'A'; 10];
    observed[4] = b'G';
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 110,
        flags: rgv_hts::AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"substitution-read".to_vec(),
        cigar: vec![rgv_hts::CigarOp {
            kind: CigarOpKind::Match,
            len: 10,
        }],
        sequence: SequenceData::Present {
            bases: observed,
            qualities: Some(vec![37; 10]),
        },
        auxiliary_fields: Vec::new(),
    };

    assert_eq!(
        find_read_event(&record, Some(&reference), 104.25, 0.5, true),
        Some(ReadEventHit {
            operation_index: 0,
            operation_offset: Some(4),
        })
    );
    assert_eq!(
        find_read_event(&record, Some(&reference), 103.25, 0.5, true),
        None,
        "a matching base is not a selectable substitution"
    );
    assert_eq!(
        find_read_event(&record, Some(&reference), 104.25, 0.5, false),
        None,
        "substitution hit testing follows the visible base-mark threshold"
    );

    let details = read_event_details(&record, 0, Some(4), Some(&reference), "chr1").unwrap();
    for expected in [
        ("Selected event", "Base substitution"),
        ("Event locus", "chr1:105"),
        ("Aligned read position", "5"),
        ("Reference base", "A"),
        ("Observed base", "G"),
        ("Base quality", "37"),
        ("Substitution evidence", "read/reference comparison"),
    ] {
        assert!(
            details
                .fields
                .contains(&(expected.0.to_owned(), expected.1.to_owned()))
        );
    }
    assert_eq!(details.copy_sequence.as_deref(), Some("G"));

    let mut explicit = record.clone();
    explicit.cigar[0].kind = CigarOpKind::SequenceMismatch;
    assert_eq!(
        find_read_event(&explicit, None, 103.25, 0.5, true),
        Some(ReadEventHit {
            operation_index: 0,
            operation_offset: Some(3),
        }),
        "an explicit CIGAR X remains selectable without loaded reference bases"
    );
    let explicit_details = read_event_details(&explicit, 0, Some(3), None, "chr1").unwrap();
    assert!(explicit_details.fields.contains(&(
        "Substitution evidence".to_owned(),
        "explicit CIGAR X (unvalidated: a base is unavailable)".to_owned(),
    )));

    let conflict_reference = ReferenceSlice {
        interval,
        bases: vec![b'A'; 10],
    };
    assert_eq!(
        find_read_event(&explicit, Some(&conflict_reference), 103.25, 0.5, true),
        Some(ReadEventHit {
            operation_index: 0,
            operation_offset: Some(3),
        }),
        "an explicit X that contradicts equal bases remains selectable as a warning"
    );
    let conflict =
        read_event_details(&explicit, 0, Some(3), Some(&conflict_reference), "chr1").unwrap();
    for expected in [
        ("Selected event", "Alignment encoding conflict"),
        ("Reference base", "A"),
        ("Observed base", "A"),
        ("CIGAR assertion", "X (mismatch)"),
        ("Observed comparison", "bases agree (A = A)"),
        (
            "Validation",
            "CIGAR X asserts a mismatch, but the loaded read and reference bases agree",
        ),
    ] {
        assert!(
            conflict
                .fields
                .contains(&(expected.0.to_owned(), expected.1.to_owned()))
        );
    }

    let mut contradicted_match = record.clone();
    contradicted_match.cigar[0].kind = CigarOpKind::SequenceMatch;
    let contradicted_match_details =
        read_event_details(&contradicted_match, 0, Some(4), Some(&reference), "chr1").unwrap();
    assert!(contradicted_match_details.fields.contains(&(
        "Selected event".to_owned(),
        "Alignment encoding conflict".to_owned(),
    )));
    assert!(
        contradicted_match_details
            .fields
            .contains(&("CIGAR assertion".to_owned(), "= (match)".to_owned()))
    );
}

#[test]
fn inspector_sequence_and_tag_copy_actions_are_bounded_and_oriented() {
    let forward = bounded_oriented_sequence_copy(b"AACG", false);
    assert_eq!(forward.text, "AACG");
    assert_eq!((forward.copied_bases, forward.total_bases), (4, 4));

    let reverse = bounded_oriented_sequence_copy(b"AACG", true);
    assert_eq!(reverse.text, "CGTT");
    assert_eq!((reverse.copied_bases, reverse.total_bases), (4, 4));

    let aligned = bounded_aligned_sequence_copy(b"AACG");
    assert_eq!(aligned.text, "AACG");

    let long_sequence = vec![b'A'; SEQUENCE_COPY_BASES + 17];
    let bounded = bounded_sequence_copy(&long_sequence);
    assert_eq!(bounded.text.len(), SEQUENCE_COPY_BASES);
    assert_eq!(bounded.copied_bases, SEQUENCE_COPY_BASES);
    assert_eq!(bounded.total_bases, SEQUENCE_COPY_BASES + 17);
    assert!(sequence_copy_hover(Some(&bounded), "read").contains("use export"));

    let tags = vec![
        ("AA".to_owned(), "A".repeat(600_000)),
        ("BB".to_owned(), "B".repeat(600_000)),
    ];
    let copied = auxiliary_tags_text(&tags);
    assert!(copied.starts_with("AA:"));
    assert!(copied.ends_with("1 tags omitted at the clipboard limit"));
    assert!(copied.len() < AUXILIARY_TAGS_COPY_BYTES);

    let unicode = "é".repeat(AUXILIARY_VALUE_DISPLAY_BYTES);
    let preview = bounded_auxiliary_value(&unicode);
    assert!(preview.is_char_boundary(preview.len()));
    assert!(preview.contains("bytes)"));
    let mut editor = "é".repeat(1_000);
    truncate_string_to_bytes(&mut editor, 1_024);
    assert_eq!(editor.len(), 1_024);
    assert!(editor.is_char_boundary(editor.len()));
}

#[test]
fn selected_chain_includes_current_and_declared_segments_with_error_provenance() {
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 200,
        flags: rgv_hts::AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"split-read".to_vec(),
        cigar: vec![rgv_hts::CigarOp {
            kind: CigarOpKind::Match,
            len: 100,
        }],
        sequence: SequenceData::NotLoaded,
        auxiliary_fields: vec![
            rgv_hts::AuxField {
                tag: *b"NM",
                value: AuxValue::Unsigned(2),
            },
            rgv_hts::AuxField {
                tag: *b"SA",
                value: AuxValue::String("chr2,501,-,25S75M,40,3;broken;".to_owned()),
            },
        ],
    };
    let chain = selected_supplementary_chain(&record, "chr1").unwrap();
    assert_eq!(chain.read_name, b"split-read");
    assert_eq!(chain.segments.len(), 2);
    assert_eq!(chain.segments[0].reference_name, "chr1");
    assert_eq!(chain.segments[0].edit_distance, 2);
    assert_eq!(chain.segments[1].reference_name, "chr2");
    assert_eq!((chain.segments[1].start, chain.segments[1].end), (500, 575));
    assert!(chain.segments[1].reverse);
    assert_eq!(chain.malformed_entries, 1);

    let diagram = supplementary_chain_diagram(&chain);
    assert_eq!(diagram.read_length, 100);
    assert_eq!(diagram.alignment_order, vec![0, 1]);
    assert_eq!(diagram.read_order, vec![1, 0]);
    assert_eq!(
        diagram.segments[1],
        ChainDiagramSegment {
            segment_index: 1,
            read_start: 0,
            read_end: 75,
            read_length: 100,
        }
    );
    assert_eq!(
        segment_read_coordinates(
            &[
                rgv_hts::CigarOp {
                    kind: CigarOpKind::HardClip,
                    len: 10,
                },
                rgv_hts::CigarOp {
                    kind: CigarOpKind::SoftClip,
                    len: 10,
                },
                rgv_hts::CigarOp {
                    kind: CigarOpKind::Match,
                    len: 80,
                },
            ],
            false,
        ),
        (20, 100, 100)
    );
}

#[test]
fn alignment_edit_summary_separates_substitutions_and_indels() {
    let mut record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 120,
        flags: rgv_hts::AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"edits".to_vec(),
        cigar: vec![
            rgv_hts::CigarOp {
                kind: CigarOpKind::Match,
                len: 10,
            },
            rgv_hts::CigarOp {
                kind: CigarOpKind::Insertion,
                len: 2,
            },
            rgv_hts::CigarOp {
                kind: CigarOpKind::Deletion,
                len: 3,
            },
            rgv_hts::CigarOp {
                kind: CigarOpKind::Insertion,
                len: 1,
            },
        ],
        sequence: SequenceData::NotLoaded,
        auxiliary_fields: vec![rgv_hts::AuxField {
            tag: *b"NM",
            value: AuxValue::Unsigned(12),
        }],
    };
    let summary = alignment_edit_summary(&record);
    assert_eq!(summary.substitutions, Some(6));
    assert_eq!(
        summary.substitution_source,
        Some(SubstitutionCountSource::Nm)
    );
    assert_eq!((summary.insertion_bases, summary.insertion_events), (3, 2));
    assert_eq!((summary.deletion_bases, summary.deletion_events), (3, 1));
    assert_eq!(summary.edit_distance, Some(12));

    record.auxiliary_fields = vec![rgv_hts::AuxField {
        tag: *b"MD",
        value: AuxValue::String("10A5^AC3T0G4".to_owned()),
    }];
    let summary = alignment_edit_summary(&record);
    assert_eq!(summary.substitutions, Some(3));
    assert_eq!(
        summary.substitution_source,
        Some(SubstitutionCountSource::Md)
    );

    record.cigar.push(rgv_hts::CigarOp {
        kind: CigarOpKind::SequenceMismatch,
        len: 4,
    });
    record.auxiliary_fields.clear();
    let summary = alignment_edit_summary(&record);
    assert_eq!(summary.substitutions, Some(4));
    assert_eq!(
        summary.substitution_source,
        Some(SubstitutionCountSource::ExplicitCigar)
    );

    let fields = alignment_edit_fields(&record);
    assert!(fields.iter().any(|(label, value)| {
        label == "Base substitutions" && value == "4 (from explicit CIGAR X operations)"
    }));
    assert!(
        fields
            .iter()
            .any(|(label, value)| label == "Insertions" && value == "3 bases across 2 events")
    );
}

#[test]
fn supplementary_chain_segments_expose_complete_accessibility_names() {
    let chain = SelectedSupplementaryChain {
        read_name: b"split-read".to_vec(),
        segments: vec![SupplementarySegment {
            reference_name: "chr2".to_owned(),
            start: 500,
            end: 575,
            reverse: true,
            cigar: vec![rgv_hts::CigarOp {
                kind: CigarOpKind::Match,
                len: 75,
            }],
            mapping_quality: Some(40),
            edit_distance: 3,
        }],
        active_segment: 0,
        malformed_entries: 0,
        truncated: false,
        invalid_tag_type: false,
    };
    let context = egui::Context::default();
    context.enable_accesskit();
    let output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(700.0, 180.0),
            )),
            ..egui::RawInput::default()
        },
        |ui| {
            let _ = paint_supplementary_chain_diagram(ui, &chain);
        },
    );
    let update = output.platform_output.accesskit_update.unwrap();
    assert!(update.nodes.iter().any(|(_, node)| {
        node.role() == egui::accesskit::Role::Button
            && node.label().is_some_and(|label| {
                label.contains("Supplementary alignment segment 1")
                    && label.contains("chr2")
                    && label.contains("strand −")
            })
            && node.toggled() == Some(egui::accesskit::Toggled::True)
    }));
    let document = supplementary_chain_inspector_document(&chain, 0);
    assert!(
        document
            .all_fields()
            .any(|field| { matches!(&field.value, InspectorValue::SupplementaryChainDiagram(_)) })
    );
    let table = document
        .all_fields()
        .find_map(|field| match &field.value {
            InspectorValue::Table(table) => Some(table),
            _ => None,
        })
        .unwrap();
    assert_eq!(table.rows[0].last().map(String::as_str), Some("75M"));
    assert!(document.actions.iter().any(|action| {
        matches!(action.id, InspectorActionId::OpenChainSegment(0)) && !action.enabled
    }));
}

#[test]
fn accessibility_preferences_apply_scale_contrast_and_reduced_motion() {
    let context = egui::Context::default();
    let settings = DesktopSettings {
        interface_scale_percent: 150,
        high_contrast: true,
        reduced_motion: true,
        ..DesktopSettings::default()
    };

    apply_accessibility_preferences(&context, &settings);
    let _ = context.run_ui(egui::RawInput::default(), |_| {});

    assert!((context.zoom_factor() - 1.5).abs() < f32::EPSILON);
    let style = context.style_of(egui::Theme::Light);
    assert_eq!(
        style.visuals.override_text_color,
        Some(egui::Color32::BLACK)
    );
    assert_eq!(
        style.visuals.selection.bg_fill,
        egui::Color32::from_rgb(0, 64, 160)
    );
    assert!(style.animation_time.abs() < f32::EPSILON);
}

#[test]
fn supplementary_flag_alone_still_creates_an_explicit_single_segment_chain() {
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 200,
        flags: rgv_hts::AlignmentFlags::from_bits(0x800),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"orphan-supplementary".to_vec(),
        cigar: vec![rgv_hts::CigarOp {
            kind: CigarOpKind::Match,
            len: 100,
        }],
        sequence: SequenceData::NotLoaded,
        auxiliary_fields: Vec::new(),
    };
    let chain = selected_supplementary_chain(&record, "chr1").unwrap();
    assert_eq!(chain.segments.len(), 1);
    assert!(!chain.invalid_tag_type);
}

#[test]
fn run_marker_detects_unclean_exit_and_clears_on_finish() {
    let directory = tempfile::tempdir().unwrap();
    let settings = directory.path().join("settings.json");
    let first = RunMarker::start(&settings);
    assert!(!first.previous_unclean_exit());
    drop(first);

    let mut recovered = RunMarker::start(&settings);
    assert!(recovered.previous_unclean_exit());
    recovered.finish();
    assert!(!directory.path().join("rgv-running.marker").exists());
}

#[test]
fn autosave_names_are_stable_and_retention_is_scoped_per_project() {
    let directory = tempfile::tempdir().unwrap();
    let settings = directory.path().join("settings.json");
    let first_project = directory.path().join("tumour review.json");
    let second_project = directory.path().join("normal.json");
    let first_prefix = autosave_group_prefix(Some(&first_project));
    let second_prefix = autosave_group_prefix(Some(&second_project));
    assert!(first_prefix.starts_with("tumour_review-"));
    assert_ne!(first_prefix, second_prefix);

    let autosaves = autosave_directory(&settings);
    std::fs::create_dir(&autosaves).unwrap();
    for millisecond in 1..=4 {
        let (path, prefix) = autosave_snapshot_path(
            &settings,
            Some(&first_project),
            UNIX_EPOCH + Duration::from_millis(millisecond),
        );
        assert_eq!(prefix, first_prefix);
        std::fs::write(path, b"snapshot").unwrap();
    }
    let (other, _) = autosave_snapshot_path(
        &settings,
        Some(&second_project),
        UNIX_EPOCH + Duration::from_millis(1),
    );
    std::fs::write(&other, b"other").unwrap();

    prune_autosaves(&autosaves, &first_prefix, 2).unwrap();
    let candidates = discover_recovery_candidates(&settings);
    assert_eq!(candidates.len(), 3);
    assert!(candidates.contains(&other));
    assert_eq!(
        candidates
            .iter()
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&first_prefix)
            })
            .count(),
        2
    );
}

#[test]
fn custom_style_tags_follow_sam_tag_syntax() {
    assert_eq!(parse_style_tag("NM"), Some(*b"NM"));
    assert_eq!(parse_style_tag("X1"), Some(*b"X1"));
    assert_eq!(parse_style_tag("1X"), None);
    assert_eq!(parse_style_tag("ABC"), None);
    assert_ne!(
        alignment_color_mode_fingerprint(AlignmentColorMode::Tag(*b"NM")),
        alignment_color_mode_fingerprint(AlignmentColorMode::Tag(*b"HP"))
    );
    let smrt_modes = [
        SmrtKineticsMode::SubreadIpd,
        SmrtKineticsMode::SubreadPulseWidth,
        SmrtKineticsMode::CcsForwardIpd,
        SmrtKineticsMode::CcsForwardPulseWidth,
        SmrtKineticsMode::CcsReverseIpd,
        SmrtKineticsMode::CcsReversePulseWidth,
    ];
    let fingerprints = smrt_modes
        .map(|mode| alignment_color_mode_fingerprint(AlignmentColorMode::SmrtKinetics(mode)));
    assert_eq!(
        fingerprints
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        6
    );
    let sbx_default = SbxDisplayOptions::default();
    assert_ne!(
        alignment_color_mode_fingerprint(AlignmentColorMode::Sbx(sbx_default)),
        alignment_color_mode_fingerprint(AlignmentColorMode::Sbx(SbxDisplayOptions {
            simplex_tail_mode: SbxSimplexTailMode::Hide,
            ..sbx_default
        }))
    );
    let ultima_default = UltimaDisplayOptions::default();
    assert_ne!(
        alignment_color_mode_fingerprint(AlignmentColorMode::Ultima(ultima_default)),
        alignment_color_mode_fingerprint(AlignmentColorMode::Ultima(UltimaDisplayOptions {
            quality_mode: UltimaQualityMode::Minimum,
            ..ultima_default
        }))
    );
    assert_ne!(
        alignment_color_mode_fingerprint(AlignmentColorMode::Ultima(ultima_default)),
        alignment_color_mode_fingerprint(AlignmentColorMode::Ultima(UltimaDisplayOptions {
            maximum_homopolymer_length_override: Some(12),
            ..ultima_default
        }))
    );
    assert_ne!(
        alignment_color_mode_fingerprint(AlignmentColorMode::Sbx(sbx_default)),
        alignment_color_mode_fingerprint(AlignmentColorMode::Sbx(SbxDisplayOptions {
            discordant_quality_maximum: 11,
            ..sbx_default
        }))
    );
}

#[test]
fn alignment_document_preserves_bam_evidence_actions_and_complete_bounded_values() {
    let fields = vec![
        ("Track".to_owned(), "tumour reads".to_owned()),
        ("Alignment segment".to_owned(), "Primary".to_owned()),
        (
            "Read".to_owned(),
            "read-with-a-very-long-stable-name".to_owned(),
        ),
        ("Position".to_owned(), "chr2:101-250".to_owned()),
        ("MAPQ".to_owned(), "missing".to_owned()),
        ("CIGAR".to_owned(), "summarized CIGAR".to_owned()),
        ("Selected event".to_owned(), "Insertion".to_owned()),
    ];
    let sequence = "ACGT".repeat(80);
    let cigar = "1M1I".repeat(100);
    let snapshot = InspectorSnapshot {
        fields: fields.clone(),
        auxiliary: vec![
            ("XX".to_owned(), "first".to_owned()),
            ("XX".to_owned(), "second".to_owned()),
        ],
        event_sequence: Some("TGCA".to_owned()),
        cigar: Some(cigar.clone()),
        read_sequence: Some(BoundedSequenceCopy {
            text: sequence.clone(),
            copied_bases: sequence.len(),
            total_bases: sequence.len() + 20,
        }),
    };

    let document = alignment_inspector_document(&snapshot, "read:1", None, true, 1);

    assert_eq!(document.header.source.as_deref(), Some("tumour reads"));
    assert_eq!(
        document
            .header
            .location
            .as_ref()
            .map(|location| location.start),
        Some(100)
    );
    assert!(
        document
            .sections
            .iter()
            .any(|section| section.id == "selected-event")
    );
    assert_eq!(
        document
            .raw_fields
            .iter()
            .map(|field| field.label.as_str())
            .collect::<Vec<_>>(),
        vec!["XX", "XX"]
    );
    assert_eq!(document.flatten_automation_fields(), fields);
    assert!(
        document.sections[0]
            .fields
            .iter()
            .all(|field| field.label != "Track"
                && field.label != "Read"
                && field.label != "Position")
    );
    assert!(
        document.sections[0]
            .fields
            .iter()
            .all(|field| !matches!(field.value, InspectorValue::Missing { .. }))
    );
    assert_eq!(
        document
            .all_fields()
            .find(|field| field.id == "cigar")
            .unwrap()
            .complete_copy_value(),
        cigar
    );
    let read_sequence = document
        .all_fields()
        .find(|field| field.id == "read-sequence")
        .unwrap();
    assert_eq!(read_sequence.complete_copy_value(), sequence);
    assert!(matches!(
        read_sequence.provenance,
        InspectorProvenance::Truncated { total: Some(_), .. }
    ));
    assert!(
        document
            .actions
            .iter()
            .any(|action| { action.id == InspectorActionId::OpenMate && action.enabled })
    );
    assert!(
        document.actions.iter().any(|action| {
            action.id == InspectorActionId::CopyVisibleReference && !action.enabled
        })
    );
}

#[test]
fn variant_document_preserves_custom_info_multiallelic_copy_and_large_sample_bounds() {
    let sample_count = 600;
    let long_alternate = vec![b'T'; VARIANT_ALLELE_PREVIEW_BASES + 7];
    let record = rgv_hts::VariantRecord {
        contig: "chr9".to_owned(),
        start: 999,
        end: 1_000,
        id: None,
        reference: b"A".to_vec(),
        alternates: vec![b"C".to_vec(), long_alternate.clone()],
        quality: None,
        filters: Vec::new(),
        info: vec![
            rgv_hts::VariantField {
                key: "CUSTOM".to_owned(),
                value: rgv_hts::VariantValue::Strings(vec!["alpha".to_owned(), "beta".to_owned()]),
                truncated: false,
            },
            rgv_hts::VariantField {
                key: "BOUNDED".to_owned(),
                value: rgv_hts::VariantValue::Strings(vec!["loaded-prefix".to_owned()]),
                truncated: true,
            },
        ],
        genotypes: (0..sample_count)
            .map(|sample_index| rgv_hts::VariantGenotype {
                sample_index,
                alleles: vec![
                    rgv_hts::VariantGenotypeAllele {
                        index: Some(0),
                        phased: false,
                    },
                    rgv_hts::VariantGenotypeAllele {
                        index: Some(u32::from(sample_index % 2 == 0)),
                        phased: false,
                    },
                ],
                fields: Vec::new(),
            })
            .collect(),
        variant_types: vec![rgv_hts::VariantType::Snv],
        breakends: Vec::new(),
        breakpoint_orientations: Vec::new(),
    };
    let inspector = VariantInspectorSnapshot {
        track_name: "cohort".to_owned(),
        query: Arc::new(rgv_hts::VariantQuery {
            header: rgv_hts::VariantHeader {
                samples: (0..sample_count)
                    .map(|index| format!("sample-{index:04}"))
                    .collect(),
                ..rgv_hts::VariantHeader::default()
            },
            records: vec![record.clone()],
            provenance: rgv_hts::VariantQueryProvenance {
                total_samples: sample_count,
                selected_samples: sample_count,
                ..rgv_hts::VariantQueryProvenance::default()
            },
        }),
        record_index: 0,
        settings: VariantTrackSettings {
            genotype_grouping: VariantGenotypeGrouping::Genotype,
            ..VariantTrackSettings::default()
        },
    };

    let document = variant_inspector_presentation_document(&inspector, &record, &[], None, "", 1);
    let alternate = document
        .all_fields()
        .find(|field| field.label == "ALT")
        .unwrap();
    let copied_alternates = alternate.complete_copy_value();
    assert!(copied_alternates.starts_with("C,"));
    assert_eq!(copied_alternates.len(), long_alternate.len() + 2);
    assert!(matches!(
        alternate.provenance,
        InspectorProvenance::Truncated { total: Some(_), .. }
    ));
    assert_eq!(
        document
            .raw_fields
            .iter()
            .map(|field| field.label.as_str())
            .collect::<Vec<_>>(),
        vec!["CUSTOM", "BOUNDED"]
    );
    assert!(matches!(
        document.raw_fields[1].provenance,
        InspectorProvenance::Truncated { total: None, .. }
    ));
    let table = document
        .all_fields()
        .find_map(|field| match &field.value {
            InspectorValue::Table(table) if field.id == "genotype-table" => Some(table),
            _ => None,
        })
        .unwrap();
    assert_eq!(table.rows.len(), 512);
    assert_eq!(table.total_rows, sample_count);
    assert_eq!(table.omitted_rows, sample_count - 512);
    let filtered =
        variant_inspector_presentation_document(&inspector, &record, &[], None, "sample-0599", 1);
    let filtered_table = filtered
        .all_fields()
        .find_map(|field| match &field.value {
            InspectorValue::Table(table) if field.id == "genotype-table" => Some(table),
            _ => None,
        })
        .unwrap();
    assert_eq!(filtered_table.total_rows, 1);
    assert_eq!(filtered_table.omitted_rows, 0);
    assert_eq!(filtered_table.rows[0][0], "sample-0599");
    assert_eq!(
        document.flatten_automation_fields(),
        vec![
            ("Track".to_owned(), "cohort".to_owned()),
            ("Locus".to_owned(), "chr9:1000-1000".to_owned()),
            ("ID".to_owned(), "missing".to_owned()),
            ("Type".to_owned(), "SNV".to_owned()),
            ("REF".to_owned(), "A".to_owned()),
            (
                "ALT".to_owned(),
                format!("C,{}… (+7 bases)", "T".repeat(VARIANT_ALLELE_PREVIEW_BASES))
            ),
            ("QUAL".to_owned(), "missing".to_owned()),
            ("FILTER".to_owned(), "PASS".to_owned()),
        ]
    );
}
