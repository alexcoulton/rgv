use super::super::*;
use crate::automation::auxiliary_position_snapshot_is_current;

fn automation_read_query(
    eligible_records: usize,
    emitted_records: usize,
    limit: usize,
    individual_reads: IndividualReadStatus,
) -> AlignmentQuery {
    AlignmentQuery {
        interval: GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        header: Arc::default(),
        batch: rgv_hts::AlignmentBatch::default(),
        pileup: rgv_hts::PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats {
            records_eligible: eligible_records,
            records_emitted: emitted_records,
            complete_read_limit: limit,
            individual_reads,
            ..QueryStats::default()
        },
    }
}

#[test]
fn automation_read_presentation_reports_complete_or_zero_records() {
    let complete = automation_read_query(20, 20, 50_000, IndividualReadStatus::Complete);
    let foreground_dense = automation_read_query(
        50_001,
        0,
        50_000,
        IndividualReadStatus::TooDense { limit: 50_000 },
    );
    let horizon_dense = automation_read_query(
        70_000,
        0,
        50_000,
        IndividualReadStatus::TooDense { limit: 50_000 },
    );

    let admitted = automation_read_presentation_fields(
        Some(&complete),
        Some(&complete),
        ReadStackingMode::Stable,
        false,
        false,
        80_000,
    );
    assert_eq!(admitted.state, AutomationReadPresentationState::Complete);
    assert_eq!(admitted.requested_limit, 80_000);
    assert_eq!(admitted.effective_limit, 50_000);
    assert_eq!(admitted.eligible_records, 20);
    assert_eq!(admitted.complete_records, 20);
    assert_eq!(admitted.refusal_eligible_records, 0);
    assert_eq!(admitted.refusal_scope, None);

    let refused = automation_read_presentation_fields(
        Some(&foreground_dense),
        Some(&foreground_dense),
        ReadStackingMode::Compact,
        false,
        false,
        50_000,
    );
    assert_eq!(refused.state, AutomationReadPresentationState::TooDense);
    assert_eq!(refused.eligible_records, 50_001);
    assert_eq!(refused.complete_records, 0);
    assert_eq!(refused.refusal_eligible_records, 50_001);
    assert_eq!(refused.refusal_scope.as_deref(), Some("foreground"));

    let admitted_with_dense_horizon = automation_read_presentation_fields(
        Some(&complete),
        Some(&horizon_dense),
        ReadStackingMode::Stable,
        false,
        false,
        50_000,
    );
    assert_eq!(
        admitted_with_dense_horizon.state,
        AutomationReadPresentationState::Complete
    );
    assert_eq!(admitted_with_dense_horizon.eligible_records, 20);
    assert_eq!(admitted_with_dense_horizon.complete_records, 20);
    assert_eq!(admitted_with_dense_horizon.refusal_eligible_records, 0);
    assert_eq!(admitted_with_dense_horizon.refusal_scope, None);

    let compact = automation_read_presentation_fields(
        Some(&complete),
        Some(&horizon_dense),
        ReadStackingMode::Compact,
        false,
        false,
        50_000,
    );
    assert_eq!(compact.state, AutomationReadPresentationState::Complete);
    assert_eq!(compact.complete_records, 20);
}

#[test]
fn automation_read_presentation_prioritises_transitional_and_failure_states() {
    let complete = automation_read_query(20, 20, 50_000, IndividualReadStatus::Complete);
    let checking = automation_read_presentation_fields(
        Some(&complete),
        Some(&complete),
        ReadStackingMode::Stable,
        true,
        false,
        50_000,
    );
    assert_eq!(checking.state, AutomationReadPresentationState::Checking);

    let failed = automation_read_presentation_fields(
        Some(&complete),
        Some(&complete),
        ReadStackingMode::Stable,
        true,
        true,
        50_000,
    );
    assert_eq!(failed.state, AutomationReadPresentationState::Failed);

    let unavailable = automation_read_presentation_fields(
        None,
        None,
        ReadStackingMode::Stable,
        false,
        false,
        50_000,
    );
    assert_eq!(
        unavailable.state,
        AutomationReadPresentationState::Unavailable
    );
}

#[test]
fn inspector_redesign_keeps_automation_schema_version_four() {
    assert_eq!(AUTOMATION_SCHEMA_VERSION, 4);
}

#[test]
fn settled_auxiliary_snapshot_supports_exact_base_selection_without_refetch() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let track_id = TrackId::new(7);
    let mut panel = ViewPanelController::new(
        PanelId::new(1),
        Viewport::new(interval, 1_000.0).unwrap(),
        Some("chr1:101-200".to_owned()),
        "chr1".to_owned(),
        1_000,
    );
    panel.track_snapshots.push(TrackSnapshot {
        id: track_id,
        name: "Genes".to_owned(),
        data: TrackSnapshotData::Features(Vec::new()),
    });
    panel.track_snapshot_interval = Some(interval);
    panel.track_settled_generation = Some(panel.generation_clock.current().get());

    assert!(auxiliary_position_snapshot_is_current(
        &panel, track_id, 149
    ));
    assert!(!auxiliary_position_snapshot_is_current(
        &panel, track_id, 250
    ));

    panel.generation_clock.advance();
    assert!(!auxiliary_position_snapshot_is_current(
        &panel, track_id, 149
    ));
}

#[test]
fn automation_runner_stops_at_the_first_failure_and_reports_skipped_commands() {
    let batch = AutomationBatch {
        schema_version: AUTOMATION_SCHEMA_VERSION,
        commands: vec![AutomationCommand::NewWorkspace, AutomationCommand::Exit],
    };
    let mut runner = AutomationRunner::new(batch, PathBuf::from("report.json"));
    assert_eq!(runner.commands.pop_front().unwrap().0, 0);

    assert!(runner.finish_command(
        0,
        Err((
            AutomationErrorCode::InvalidState,
            "fixture failure".to_owned()
        ))
    ));
    assert!(runner.commands.is_empty());
    assert_eq!(runner.results.len(), 2);
    assert_eq!(runner.results[0].command_index, 0);
    assert_eq!(runner.results[0].status, AutomationCommandStatus::Failed);
    assert_eq!(runner.results[1].command_index, 1);
    assert_eq!(runner.results[1].status, AutomationCommandStatus::Skipped);
}

#[test]
fn automation_runner_rejects_a_completion_for_a_different_waiting_command() {
    let batch = AutomationBatch {
        schema_version: AUTOMATION_SCHEMA_VERSION,
        commands: vec![AutomationCommand::Exit],
    };
    let mut runner = AutomationRunner::new(batch, PathBuf::from("report.json"));
    runner.waiting = Some(AutomationWait::File { command_index: 4 });

    assert!(!runner.finish_command(
        5,
        Ok(("wrong completion".to_owned(), serde_json::Value::Null))
    ));
    assert!(runner.results.is_empty());
    assert!(matches!(
        runner.waiting,
        Some(AutomationWait::File { command_index: 4 })
    ));
}

#[test]
fn automation_navigation_telemetry_separates_foreground_and_speculative_work() {
    let batch = AutomationBatch {
        schema_version: AUTOMATION_SCHEMA_VERSION,
        commands: vec![AutomationCommand::Exit],
    };
    let mut runner = AutomationRunner::new(batch, PathBuf::from("report.json"));
    let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let started = Instant::now();
    runner.begin_navigation(
        PanelId::new(1),
        interval,
        &AutomationNavigationState {
            source_queries: 7,
            speculative_source_queries: 3,
            ..AutomationNavigationState::default()
        },
        started,
    );
    runner.record_navigation_frame(
        started + Duration::from_millis(8),
        5.0,
        Some(PrepareStats {
            uploaded_bytes: 144,
            buffer_reallocated: true,
            ..PrepareStats::default()
        }),
        false,
        &AutomationNavigationState::default(),
    );
    runner.record_navigation_frame(
        started + Duration::from_millis(21),
        7.0,
        Some(PrepareStats {
            slab_cache_hit: true,
            ..PrepareStats::default()
        }),
        true,
        &AutomationNavigationState {
            source_queries: 9,
            foreground_source_queries: 0,
            speculative_source_queries: 5,
            cache_outcome: "ResidentExtension".to_owned(),
            row_scene_hit: true,
            row_scene_physical_hit: true,
            row_scene_projection_micros: 3_100,
        },
    );

    let sample = runner.navigation.first().unwrap();
    assert!(sample.complete);
    assert_eq!(sample.input_to_first_presented_frame_ms, Some(8.0));
    assert_eq!(sample.input_to_authoritative_frame_ms, Some(21.0));
    assert_eq!(sample.foreground_source_queries, 0);
    assert_eq!(sample.speculative_source_queries, 2);
    assert_eq!(sample.total_source_queries, 2);
    assert_eq!(sample.gpu_uploaded_bytes, 144);
    assert_eq!(sample.gpu_buffer_reallocations, 1);
    assert_eq!(sample.gpu_slab_cache_hits, 1);
}

#[test]
fn retained_evidence_is_immutable_when_the_viewport_changes() {
    let retained = AutomationSelectionSnapshot {
        selection_id: "immutable-selection".to_owned(),
        evidence_revision: 11,
        workspace_revision_at_selection: 20,
        panel_id: PanelId::new(1),
        contig: "chr1".to_owned(),
        track_id: Some(TrackId::new(2)),
        kind: AutomationSelectionKind::Pileup,
        granularity: Some(AutomationSelectionGranularity::VisibleBin),
        anchor_position: Some(2_599_999),
        evidence_start: Some(2_599_998),
        evidence_end: Some(2_600_001),
        position: Some(2_599_999),
        start: Some(2_599_998),
        end: Some(2_600_001),
        fields: vec![AutomationEvidenceField {
            key: "Depth".to_owned(),
            value: "2×".to_owned(),
        }],
        provenance: vec![AutomationEvidenceField {
            key: "eligible_reads".to_owned(),
            value: "2".to_owned(),
        }],
        visible_in_active_view: true,
        stale: false,
        stale_reason: None,
        truncated: false,
        presentation_truncated: false,
    };
    let moved = GenomicInterval::new(ContigId::new(0), 2_600_100, 2_600_200).unwrap();
    let observed = observed_retained_selection(&retained, moved, "chr1");
    assert!(!observed.visible_in_active_view);
    assert_eq!(observed.selection_id, retained.selection_id);
    assert_eq!(observed.evidence_revision, retained.evidence_revision);
    assert_eq!(observed.evidence_start, retained.evidence_start);
    assert_eq!(observed.evidence_end, retained.evidence_end);
    assert_eq!(observed.fields, retained.fields);
    assert_eq!(observed.provenance, retained.provenance);
}

#[test]
fn retained_evidence_is_not_visible_on_a_different_contig() {
    let retained = AutomationSelectionSnapshot {
        selection_id: "cross-contig-selection".to_owned(),
        evidence_revision: 1,
        workspace_revision_at_selection: 2,
        panel_id: PanelId::new(1),
        contig: "chr1".to_owned(),
        track_id: Some(TrackId::new(2)),
        kind: AutomationSelectionKind::Read,
        granularity: None,
        anchor_position: Some(10),
        evidence_start: Some(10),
        evidence_end: Some(20),
        position: Some(10),
        start: Some(10),
        end: Some(20),
        fields: Vec::new(),
        provenance: Vec::new(),
        visible_in_active_view: true,
        stale: false,
        stale_reason: None,
        truncated: false,
        presentation_truncated: false,
    };
    let viewport = GenomicInterval::new(ContigId::new(1), 0, 100).unwrap();
    let observed = observed_retained_selection(&retained, viewport, "chr2");
    assert!(!observed.visible_in_active_view);
}

#[test]
fn active_panel_snapshot_cropping_clips_and_rebases_semantic_primitives() {
    let display = DisplayList {
        rectangles: vec![
            RectPrimitive {
                x: 90.0,
                y: 110.0,
                width: 30.0,
                height: 30.0,
                color: Rgba::new(0.1, 0.2, 0.3, 1.0),
                object_id: 7,
                shape: Default::default(),
            },
            RectPrimitive {
                x: 200.0,
                y: 200.0,
                width: 10.0,
                height: 10.0,
                color: Rgba::new(0.2, 0.3, 0.4, 1.0),
                object_id: 8,
                shape: Default::default(),
            },
        ],
        texts: vec![rgv_layout::TextPrimitive {
            x: 115.0,
            y: 125.0,
            text: "A".to_owned(),
            font_size: 12.0,
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        }],
    };
    let bounds = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(150.0, 150.0));

    let cropped = cropped_display_list(&display, bounds);
    assert_eq!(cropped.rectangles.len(), 1);
    assert!(cropped.rectangles[0].x.abs() < f32::EPSILON);
    assert!((cropped.rectangles[0].y - 10.0).abs() < f32::EPSILON);
    assert!((cropped.rectangles[0].width - 20.0).abs() < f32::EPSILON);
    assert!((cropped.rectangles[0].height - 30.0).abs() < f32::EPSILON);
    assert_eq!(cropped.rectangles[0].object_id, 7);
    assert!((cropped.texts[0].x - 15.0).abs() < f32::EPSILON);
    assert!((cropped.texts[0].y - 25.0).abs() < f32::EPSILON);
}

#[test]
fn visible_workspace_snapshot_crops_and_rebases_to_committed_layout_bounds() {
    let committed_workspace =
        egui::Rect::from_min_max(egui::pos2(0.0, 62.0), egui::pos2(940.0, 776.0));
    let display = DisplayList {
        rectangles: vec![RectPrimitive {
            x: 168.0,
            y: 133.0,
            width: 20.0,
            height: 12.0,
            color: Rgba::new(0.1, 0.2, 0.3, 1.0),
            object_id: 0,
            shape: Default::default(),
        }],
        texts: Vec::new(),
    };

    let cropped = cropped_display_list(&display, committed_workspace);

    assert_eq!(committed_workspace.width().ceil() as u32, 940);
    assert_eq!(committed_workspace.height().ceil() as u32, 714);
    assert_eq!(cropped.rectangles.len(), 1);
    assert!((cropped.rectangles[0].x - 168.0).abs() < f32::EPSILON);
    assert!((cropped.rectangles[0].y - 71.0).abs() < f32::EPSILON);
}

#[test]
fn automation_svg_is_deterministic_bounded_and_xml_escaped() {
    let display = DisplayList {
        rectangles: vec![RectPrimitive {
            x: 1.0,
            y: 2.0,
            width: 3.0,
            height: 4.0,
            color: Rgba::new(1.0, 0.0, 0.0, 0.5),
            object_id: 0,
            shape: Default::default(),
        }],
        texts: vec![rgv_layout::TextPrimitive {
            x: 5.0,
            y: 6.0,
            text: "A<&".to_owned(),
            font_size: 12.0,
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        }],
    };
    let first = display_list_svg_export(&display, 100, 80, "sample <private> & safe");
    let second = display_list_svg_export(&display, 100, 80, "sample <private> & safe");

    assert_eq!(first, second);
    assert!(first.len() < MAX_AUTOMATION_EXPORT_BYTES);
    assert!(first.contains("sample &lt;private&gt; &amp; safe"));
    assert!(first.contains("A&lt;&amp;"));
}

#[test]
fn automation_svg_exports_semantic_direction_shapes_as_polygons() {
    let display = DisplayList {
        rectangles: vec![RectPrimitive {
            x: 10.0,
            y: 10.0,
            width: 8.0,
            height: 6.0,
            color: Rgba::new(0.2, 0.4, 0.8, 1.0),
            object_id: 77,
            shape: rgv_layout::RectShape::ChevronLeft,
        }],
        texts: Vec::new(),
    };
    let svg = display_list_svg_export(&display, 100, 80, "direction");
    assert!(svg.contains("<polygon"));
    assert!(!svg.contains("object_id"));
}

#[test]
fn automation_svg_exports_junction_arcs_as_curved_paths() {
    let display = DisplayList {
        rectangles: vec![RectPrimitive {
            x: 10.0,
            y: 10.0,
            width: 40.0,
            height: 20.0,
            color: Rgba::new(0.8, 0.2, 0.4, 0.8),
            object_id: 0,
            shape: rgv_layout::RectShape::JunctionArcUp,
        }],
        texts: Vec::new(),
    };
    let svg = display_list_svg_export(&display, 100, 80, "junction");
    assert!(svg.contains("<path"));
    assert!(svg.contains(" C "));
    assert!(svg.contains("stroke-linecap=\"round\""));
}

#[test]
fn semantic_exports_omit_transient_sashimi_hover_overlays() {
    let base = RectPrimitive {
        x: 10.0,
        y: 10.0,
        width: 40.0,
        height: 20.0,
        color: Rgba::new(0.8, 0.2, 0.4, 0.8),
        object_id: 0,
        shape: rgv_layout::RectShape::JunctionArcUp,
    };
    let display = DisplayList {
        rectangles: vec![
            base,
            RectPrimitive {
                shape: rgv_layout::RectShape::JunctionArcHoverUp,
                color: Rgba::new(0.7, 0.3, 0.5, 1.0),
                ..base
            },
        ],
        texts: Vec::new(),
    };

    assert_eq!(
        semantic_export_display_list(&display).rectangles,
        vec![base]
    );
    assert_eq!(
        cropped_display_list(
            &display,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 80.0)),
        )
        .rectangles,
        vec![base]
    );
    let svg = display_list_svg_export(&display, 100, 80, "junction hover");
    assert_eq!(svg.matches("<path").count(), 1);
}

#[test]
fn automation_svg_exports_breakends_as_brackets_instead_of_arrows() {
    let display = DisplayList {
        rectangles: vec![RectPrimitive {
            x: 10.0,
            y: 10.0,
            width: 8.0,
            height: 8.0,
            color: Rgba::new(0.8, 0.2, 0.4, 1.0),
            object_id: 77,
            shape: rgv_layout::RectShape::BreakendJoinBefore,
        }],
        texts: Vec::new(),
    };
    let svg = display_list_svg_export(&display, 100, 80, "breakend");
    assert_eq!(
        svg.matches("<rect").count(),
        3,
        "background plus bracket parts"
    );
    assert!(!svg.contains("<polygon"));
}

#[test]
fn automation_report_writes_replace_the_previous_file_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested/report.json");
    atomic_write_text(&path, "first\n").unwrap();
    atomic_write_text(&path, "second\n").unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "second\n");
}

#[test]
fn alignment_projection_generation_distinguishes_old_data_from_settled_data() {
    let mut projection = AlignmentPanelProjection::default();
    projection.settled_generation = Some(projection.clock.current().get());
    assert_eq!(
        projection.settled_generation,
        Some(projection.clock.current().get())
    );
    projection.clock.advance();
    assert_ne!(
        projection.settled_generation,
        Some(projection.clock.current().get())
    );
}

#[test]
fn automation_viewport_operations_use_magnification_semantics_and_clamp() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 1_000, 2_000).unwrap(),
        1_000.0,
    )
    .unwrap();
    let zoomed = automation_viewport_after_operation(
        viewport,
        10_000,
        AutomationViewportOperation::Zoom {
            factor: 2.0,
            anchor_position: Some(1_250),
        },
    )
    .unwrap();
    assert!((zoomed.span() - 500.0).abs() < f64::EPSILON);
    assert!((zoomed.genomic_at_pixel(250.0) - 1_250.0).abs() <= 1.0);

    let panned = automation_viewport_after_operation(
        zoomed,
        10_000,
        AutomationViewportOperation::PanBases { delta: -10_000 },
    )
    .unwrap();
    assert_eq!(panned.interval().start(), 0);

    let fitted = automation_viewport_after_operation(
        viewport,
        10_000,
        AutomationViewportOperation::FitInterval {
            start: 9_900,
            end: 20_000,
        },
    )
    .unwrap();
    assert_eq!(fitted.interval().start(), 9_900);
    assert_eq!(fitted.interval().end(), 10_000);
}

#[test]
fn automation_read_evidence_excludes_private_identities_and_sequence() {
    for prohibited in [
        "Read",
        "Sequence",
        "Sequence copy orientation",
        "Template identity",
        "Ultima read group",
    ] {
        assert!(!automation_read_evidence_field_is_public(prohibited));
    }
    for public in ["Position", "MAPQ", "CIGAR", "Pair state"] {
        assert!(automation_read_evidence_field_is_public(public));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn automation_structured_exports_preserve_active_interval_semantics() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 110).unwrap();
    let mut batch = rgv_hts::AlignmentBatch::default();
    batch
        .push(&AlignmentRecord {
            contig: ContigId::new(0),
            start: 101,
            end: 106,
            flags: rgv_hts::AlignmentFlags::from_bits(0x10),
            mapping_quality: Some(42),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"read-1".to_vec(),
            cigar: vec![rgv_hts::CigarOp {
                kind: CigarOpKind::Match,
                len: 5,
            }],
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        })
        .unwrap();
    let alignment_tracks = vec![AutomationAlignmentExportTrack {
        id: TrackId::new(1),
        name: "Tumour".to_owned(),
        query: Arc::new(AlignmentQuery {
            interval,
            header: Arc::default(),
            batch,
            pileup: rgv_hts::PileupSummary::from_depths([3, 4]),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: QueryStats {
                records_seen: 1,
                records_eligible: 1,
                records_emitted: 1,
                truncated: false,
                cancelled: false,
                ..QueryStats::default()
            },
        }),
    }];
    let alignment = automation_alignments_export("GRCh38", "chr1", interval, &alignment_tracks);
    let pileup = automation_pileup_export("GRCh38", "chr1", interval, &alignment_tracks);
    assert!(alignment.contains("read-1\t0x0010\t42\t5M"));
    assert!(alignment.contains("read_presentation=complete"));
    assert!(alignment.contains("complete_read_limit="));
    assert!(!alignment.contains("retained_truncated"));
    assert!(alignment.contains("\t102\t106\t"));
    assert!(pileup.contains("eligible_records=0"));
    assert!(pileup.contains("\t101\t105\t3\t"));

    let variant = TrackSnapshot {
        id: TrackId::new(2),
        name: "Calls".to_owned(),
        data: TrackSnapshotData::Variants(Arc::new(rgv_hts::VariantQuery {
            header: rgv_hts::VariantHeader {
                samples: vec!["S1".to_owned()],
                ..rgv_hts::VariantHeader::default()
            },
            records: vec![rgv_hts::VariantRecord {
                contig: "chr1".to_owned(),
                start: 104,
                end: 105,
                id: Some("rsTest".to_owned()),
                reference: b"A".to_vec(),
                alternates: vec![b"T".to_vec()],
                quality: Some(99.0),
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
                breakpoint_orientations: Vec::new(),
            }],
            provenance: rgv_hts::VariantQueryProvenance {
                records_examined: 1,
                selected_samples: 1,
                total_samples: 1,
                ..rgv_hts::VariantQueryProvenance::default()
            },
        })),
    };
    let quantitative = TrackSnapshot {
        id: TrackId::new(3),
        name: "Signal".to_owned(),
        data: TrackSnapshotData::Quantitative(Arc::new(rgv_hts::QuantitativeQuery {
            records: vec![rgv_hts::QuantitativeRecord {
                start: 102,
                end: 108,
                value: 2.5,
            }],
            provenance: rgv_hts::QuantitativeQueryProvenance {
                records_examined: 1,
                records_retained: 1,
                ..rgv_hts::QuantitativeQueryProvenance::default()
            },
            source_metadata: None,
        })),
    };
    let variants = automation_variants_export("GRCh38", "chr1", interval, &[variant]);
    let quantitative = automation_quantitative_export("GRCh38", "chr1", interval, &[quantitative]);
    assert!(variants.contains("record\t2\tCalls\tchr1\t105\t105\trsTest\tA\tT\t99\tPASS\tSNV"));
    assert!(variants.contains("genotype\t2\t0\tS1\tGT=0/1\tHet"));
    assert!(quantitative.contains("3\tSignal\tchr1\t103\t108\t2.5"));
}
