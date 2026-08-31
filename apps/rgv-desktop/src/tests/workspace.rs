use super::super::*;
use crate::workspace::alignment_management::{
    alignment_gpu_bundle_error_is_deferred, alignment_read_camera_capture_error_is_deferred,
    alignment_scene_covered_clip, alignment_scene_frame_for_ui_against_retained_interval,
    alignment_scene_intervals_share_authority, alignment_scene_staging_frame_is_current,
    alignment_summary_camera_capture_error_is_deferred,
};
use crate::workspace::queries::{
    AlignmentReferenceLanePlan, AlignmentReferenceLaneRequirement, AlignmentSceneFrameSettlement,
    alignment_scene_budget_event_is_current, alignment_scene_event_token_is_current,
    alignment_scene_frame_is_current, alignment_scene_frame_settlement,
    alignment_scene_pressure_is_current, alignment_scene_pressure_preserves_visible_epoch,
    alignment_scene_pressure_requires_resume, alignment_scene_refusal_is_current,
    plan_alignment_reference_lanes, reference_dependency_is_settled,
    reference_failure_matches_requirement, reference_query_completion_matches,
};

fn alignment_publication_target(
    viewport: Viewport,
    workspace_layout_revision: u64,
    surface_size: [u32; 2],
    scale_factor: f64,
    row_scroll: f64,
    reads_visible: bool,
    coverage_visible: bool,
) -> AlignmentSceneTargetIdentity {
    let clip = CpuClipRect {
        x: 12.0,
        y: 30.0,
        width: viewport.pixel_width(),
        height: 80.0,
    };
    let read_camera = reads_visible
        .then(|| AlignmentCamera::from_viewport(viewport, row_scroll, 8.0, 7.0, clip).unwrap());
    let preparation_camera = read_camera.map_or_else(
        || AlignmentSceneCamera::without_visible_rows(viewport),
        AlignmentSceneCamera::from,
    );
    let derived_lanes = coverage_visible
        .then_some(AlignmentDerivedLaneTargetIdentity {
            kind: AlignmentDerivedLaneKind::Coverage,
            clip: CpuClipRect {
                x: 12.0,
                y: 10.0,
                width: viewport.pixel_width(),
                height: 20.0,
            },
        })
        .into_iter()
        .collect();
    AlignmentSceneTargetIdentity {
        workspace_layout_revision,
        surface_size,
        scale_factor,
        panels: vec![AlignmentPanelTargetIdentity {
            panel_id: PanelId::new(1),
            viewport,
            lanes: vec![AlignmentLaneTargetIdentity {
                track_id: TrackId::new(2),
                preparation_camera,
                read_camera,
                derived_lanes,
            }],
        }],
    }
}

#[test]
fn publication_authority_separates_same_target_failure_from_changed_target_failure() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        100.0,
    )
    .unwrap();
    let target = alignment_publication_target(viewport, 4, [1_200, 800], 2.0, 3.5, true, true);
    let mut authority = AlignmentScenePublicationAuthority::default();

    let published = authority.begin_attempt(&target);
    assert_eq!(published.target_revision, 1);
    assert_eq!(published.sequence, 1);
    assert!(published.target_changed);
    assert!(authority.authorizes_target(published.target_revision));
    assert!(authority.commit_attempt(&published));
    assert!(authority.authorizes_target(published.target_revision));

    // Model a failed hover/selection or same-target content refresh. The new attempt is distinct,
    // but the previously published target remains exact and therefore remains renderable/pickable.
    let failed_same_target = authority.begin_attempt(&target);
    assert_eq!(
        failed_same_target.target_revision,
        published.target_revision
    );
    assert_eq!(failed_same_target.sequence, 2);
    assert!(!failed_same_target.target_changed);
    assert!(authority.authorizes_target(published.target_revision));
    assert!(alignment_gpu_sequences_match_attempt(
        published.sequence,
        Some(&[published.sequence]),
        Some(&[published.sequence]),
    ));

    // A real pan changes the target before its fallible publication starts. If that attempt fails,
    // the old pixels remain privately owned but cannot claim the new destination.
    let moved_viewport = viewport.pan_pixels(1.0, 10_000).unwrap();
    let moved_target =
        alignment_publication_target(moved_viewport, 4, [1_200, 800], 2.0, 3.5, true, true);
    let failed_moved_target = authority.begin_attempt(&moved_target);
    assert_eq!(failed_moved_target.target_revision, 2);
    assert_eq!(failed_moved_target.sequence, 3);
    assert!(failed_moved_target.target_changed);
    assert!(!authority.authorizes_target(published.target_revision));
    assert!(authority.authorizes_target(failed_moved_target.target_revision));

    // A subsequent success for that same moved target gets a newer publication sequence without
    // manufacturing yet another camera revision.
    let published_moved_target = authority.begin_attempt(&moved_target);
    assert_eq!(published_moved_target.target_revision, 2);
    assert_eq!(published_moved_target.sequence, 4);
    assert!(published_moved_target.target_changed);
    assert!(!authority.commit_attempt(&failed_moved_target));
    assert!(authority.commit_attempt(&published_moved_target));
    assert!(authority.authorizes_target(published_moved_target.target_revision));
    assert!(alignment_gpu_sequences_match_attempt(
        published_moved_target.sequence,
        Some(&[published_moved_target.sequence]),
        Some(&[published_moved_target.sequence]),
    ));
    assert!(!alignment_gpu_sequences_match_attempt(
        published_moved_target.sequence,
        Some(&[failed_moved_target.sequence]),
        Some(&[published_moved_target.sequence]),
    ));
}

#[test]
fn vertical_camera_change_preserves_row_target_but_genomic_pan_does_not() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        100.0,
    )
    .unwrap();
    let committed = alignment_publication_target(viewport, 4, [1_200, 800], 2.0, 0.0, true, true);
    let vertically_scrolled =
        alignment_publication_target(viewport, 4, [1_200, 800], 2.0, 90.0, true, true);
    let panned = alignment_publication_target(
        viewport.pan_pixels(1.0, 10_000).unwrap(),
        4,
        [1_200, 800],
        2.0,
        90.0,
        true,
        true,
    );
    let mut authority = AlignmentScenePublicationAuthority::default();
    let first = authority.begin_attempt(&committed);
    assert!(authority.commit_attempt(&first));

    authority.begin_attempt(&vertically_scrolled);
    assert!(authority.committed_row_target_matches_desired(PanelId::new(1), TrackId::new(2)));

    authority.begin_attempt(&panned);
    assert!(!authority.committed_row_target_matches_desired(PanelId::new(1), TrackId::new(2)));
}

#[test]
fn returning_to_the_committed_target_reauthorizes_it_without_reusing_failed_target_ids() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        100.0,
    )
    .unwrap();
    let committed = alignment_publication_target(viewport, 4, [1_200, 800], 2.0, 0.0, true, true);
    let moved = alignment_publication_target(
        viewport.pan_pixels(1.0, 10_000).unwrap(),
        4,
        [1_200, 800],
        2.0,
        0.0,
        true,
        true,
    );
    let resized = alignment_publication_target(viewport, 4, [1_201, 800], 2.0, 0.0, true, true);
    let mut authority = AlignmentScenePublicationAuthority::default();
    let first = authority.begin_attempt(&committed);
    assert!(authority.commit_attempt(&first));

    let failed_moved = authority.begin_attempt(&moved);
    assert_eq!(failed_moved.target_revision, 2);
    assert!(!authority.authorizes_target(first.target_revision));

    let returned = authority.begin_attempt(&committed);
    assert_eq!(returned.target_revision, first.target_revision);
    assert!(authority.authorizes_target(first.target_revision));

    let later_changed = authority.begin_attempt(&resized);
    assert_eq!(later_changed.target_revision, 3);
    assert_ne!(later_changed.target_revision, failed_moved.target_revision);
}

#[test]
fn staging_capture_requires_the_exact_desired_request_and_camera_pair() {
    let first = AlignmentSceneCamera::without_visible_rows(
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
            800.0,
        )
        .unwrap(),
    );
    let second = AlignmentSceneCamera::without_visible_rows(
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 110, 210).unwrap(),
            800.0,
        )
        .unwrap(),
    );
    let first_token = AlignmentSceneRequestToken::new(41);
    let second_token = AlignmentSceneRequestToken::new(42);

    assert!(!alignment_scene_staging_frame_is_current(
        Some(second_token),
        Some(first_token),
        Some(second),
        first,
        second,
    ));
    assert!(!alignment_scene_staging_frame_is_current(
        Some(second_token),
        Some(second_token),
        Some(second),
        second,
        first,
    ));
    assert!(alignment_scene_staging_frame_is_current(
        Some(second_token),
        Some(second_token),
        Some(second),
        second,
        second,
    ));
}

#[test]
fn active_replacement_transition_is_deferred_in_both_ledger_error_forms() {
    assert!(alignment_gpu_bundle_error_is_deferred(
        &AllocationBundleError::ReplacementTransitionActive { bundle_id: 17 }
    ));
    assert!(alignment_gpu_bundle_error_is_deferred(
        &AllocationBundleError::Ledger(LedgerError::ReplacementTransitionActive { bundle_id: 18 })
    ));
    assert!(alignment_gpu_bundle_error_is_deferred(
        &AllocationBundleError::PolicyRevisionChanged {
            planned: 1,
            current: 2,
        }
    ));
}

#[test]
fn camera_capture_defers_only_typed_transient_ledger_contention() {
    let read_transition =
        AlignmentGpuCameraCaptureError::Prepare(AlignmentGpuPrepareError::Ledger(
            LedgerError::ReplacementTransitionActive { bundle_id: 21 },
        ));
    let summary_transient = AlignmentSummaryGpuCameraCaptureError::Prepare(
        AlignmentSummaryGpuPrepareError::Ledger(LedgerError::TransientBudgetExceeded {
            class: rgv_alignment_scene::TransientResourceClass::GpuDriver,
            requested: 64,
            transient: 256,
            limit: 1_024,
        }),
    );
    let label_projection_transient =
        AlignmentGpuCameraCaptureError::Prepare(AlignmentGpuPrepareError::LabelProjection(
            CpuProjectionError::Ledger(LedgerError::TransientBudgetExceeded {
                class: rgv_alignment_scene::TransientResourceClass::GpuUpload,
                requested: 64,
                transient: 256,
                limit: 1_024,
            }),
        ));
    assert!(alignment_read_camera_capture_error_is_deferred(
        &read_transition
    ));
    assert!(alignment_summary_camera_capture_error_is_deferred(
        &summary_transient
    ));
    assert!(alignment_read_camera_capture_error_is_deferred(
        &label_projection_transient
    ));

    let permanent_read =
        AlignmentGpuCameraCaptureError::Prepare(AlignmentGpuPrepareError::InvalidFrameGeometry);
    let poisoned_summary = AlignmentSummaryGpuCameraCaptureError::Prepare(
        AlignmentSummaryGpuPrepareError::Ledger(LedgerError::Poisoned),
    );
    let intrinsically_oversized = AlignmentGpuCameraCaptureError::Prepare(
        AlignmentGpuPrepareError::Ledger(LedgerError::BudgetExceeded {
            requested: 2_048,
            charged: 0,
            reserved: 0,
            limit: 1_024,
        }),
    );
    let busy_slot = AlignmentGpuCameraCaptureError::FrameSlotBusy { slots: 2 };
    let invalid_label_camera = AlignmentGpuCameraCaptureError::Prepare(
        AlignmentGpuPrepareError::LabelProjection(CpuProjectionError::InvalidCamera),
    );
    assert!(!alignment_read_camera_capture_error_is_deferred(
        &permanent_read
    ));
    assert!(!alignment_summary_camera_capture_error_is_deferred(
        &poisoned_summary
    ));
    assert!(!alignment_read_camera_capture_error_is_deferred(
        &intrinsically_oversized
    ));
    assert!(!alignment_read_camera_capture_error_is_deferred(&busy_slot));
    assert!(!alignment_read_camera_capture_error_is_deferred(
        &invalid_label_camera
    ));
}

#[test]
fn camera_target_identity_covers_every_gpu_geometry_input() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        100.0,
    )
    .unwrap();
    let base = alignment_publication_target(viewport, 4, [1_200, 800], 2.0, 3.5, true, true);
    let changed = [
        alignment_publication_target(viewport, 5, [1_200, 800], 2.0, 3.5, true, true),
        alignment_publication_target(viewport, 4, [1_201, 800], 2.0, 3.5, true, true),
        alignment_publication_target(viewport, 4, [1_200, 800], 1.5, 3.5, true, true),
        alignment_publication_target(
            viewport.pan_pixels(0.5, 10_000).unwrap(),
            4,
            [1_200, 800],
            2.0,
            3.5,
            true,
            true,
        ),
        alignment_publication_target(viewport, 4, [1_200, 800], 2.0, 4.5, true, true),
        alignment_publication_target(viewport, 4, [1_200, 800], 2.0, 3.5, false, true),
        alignment_publication_target(viewport, 4, [1_200, 800], 2.0, 3.5, true, false),
    ];
    assert!(changed.iter().all(|candidate| candidate != &base));
}

#[test]
fn native_alignment_live_clip_owns_only_the_snapshot_covered_horizontal_interval() {
    let contig = ContigId::new(0);
    let viewport = Viewport::new(GenomicInterval::new(contig, 100, 200).unwrap(), 1_000.0).unwrap();
    let clip = CpuClipRect {
        x: 10.0,
        y: 20.0,
        width: 1_000.0,
        height: 80.0,
    };
    let covered = GenomicInterval::new(contig, 120, 180).unwrap();

    assert_eq!(
        alignment_scene_covered_clip(viewport, clip, covered),
        Some(CpuClipRect {
            x: 210.0,
            y: 20.0,
            width: 600.0,
            height: 80.0,
        })
    );
    assert!(
        alignment_scene_covered_clip(
            viewport,
            clip,
            GenomicInterval::new(ContigId::new(1), 120, 180).unwrap(),
        )
        .is_none()
    );
}

#[test]
fn scene_coverage_options_share_one_scale_and_require_reference_for_substitutions() {
    let filters = SessionFilters {
        minimum_allele_fraction: 0.37,
        quality_weighted_alleles: false,
        coverage_scale_maximum: Some(321),
        coverage_scale_transform: CoverageScaleTransform::SquareRoot,
        show_coverage_center_line: false,
        ..SessionFilters::default()
    };
    let without_reference = alignment_scene_coverage_lane_options(&filters, false, Some(123));
    assert!(without_reference.alleles.is_none());
    assert_eq!(without_reference.depth.scale_maximum, Some(321));
    assert_eq!(
        without_reference.depth.transform,
        CoverageScaleTransform::SquareRoot
    );
    assert!(!without_reference.depth.show_center_line);
    assert_eq!(without_reference.depth.selected_position, Some(123));

    let with_reference = alignment_scene_coverage_lane_options(&filters, true, Some(123));
    let alleles = with_reference.alleles.unwrap();
    assert!((alleles.minimum_fraction - 0.37).abs() < 1.0e-6);
    assert!(!alleles.quality_weighted);
    assert_eq!(alleles.scale_maximum, with_reference.depth.scale_maximum);
    assert_eq!(alleles.transform, with_reference.depth.transform);
    assert_eq!(
        alleles.interpretation,
        CoverageAlleleInterpretation::GenericReferenceAware
    );

    let bisulfite = alignment_scene_coverage_lane_options(
        &SessionFilters {
            alignment_color_mode: AlignmentColorMode::Bisulfite(BisulfiteContext::Chh),
            ..filters
        },
        true,
        None,
    );
    assert_eq!(
        bisulfite.alleles.unwrap().interpretation,
        CoverageAlleleInterpretation::Bisulfite(BisulfiteContext::Chh)
    );
}

#[test]
fn scene_coverage_and_modification_selection_are_captured_only_for_the_selected_track() {
    let selected = TrackId::new(11);
    let other = TrackId::new(12);
    assert_eq!(
        alignment_scene_selected_position(Some(selected), selected, Some(123)),
        Some(123)
    );
    assert_eq!(
        alignment_scene_selected_position(Some(selected), other, Some(123)),
        None
    );
    assert_eq!(
        alignment_scene_selected_position(None, selected, Some(123)),
        None
    );
    assert_eq!(
        alignment_scene_selected_position(Some(selected), selected, None),
        None
    );
}

#[test]
fn scene_sashimi_options_preserve_unresolved_shared_epoch_policy() {
    let mut filters = SessionFilters {
        minimum_junction_depth: 17,
        minimum_junction_flank: 9,
        junction_display_mode: JunctionDisplayMode::Expanded,
        sashimi_count_mode: SashimiCountMode::CountsPerMillion,
        show_junction_labels: false,
        ..SessionFilters::default()
    };
    filters.sashimi_scale_mode = SashimiScaleMode::Shared;
    assert!(alignment_scene_sashimi_lane_options(&filters, None).is_none());
    let group = AlignmentCoherenceGroupId::new(42);
    let shared = alignment_scene_sashimi_lane_options(&filters, Some(group)).unwrap();
    assert_eq!(shared.scale, AlignmentSashimiScalePolicy::Shared(group));
    assert_eq!(shared.projection.scale_maximum, None);
    assert_eq!(shared.projection.minimum_depth, 17);
    assert_eq!(shared.projection.minimum_flank, 9);
    assert_eq!(
        shared.projection.count_mode,
        SashimiCountMode::CountsPerMillion
    );
    assert!(!shared.projection.show_labels);

    filters.sashimi_scale_mode = SashimiScaleMode::Fixed;
    filters.sashimi_fixed_scale_maximum = 250;
    let fixed = alignment_scene_sashimi_lane_options(&filters, None).unwrap();
    assert_eq!(fixed.scale, AlignmentSashimiScalePolicy::Fixed(250.0));
    assert_eq!(fixed.projection.scale_maximum, Some(250.0));
}

#[test]
fn shared_sashimi_units_use_distinct_stable_coherence_groups() {
    let mut filters = SessionFilters {
        sashimi_scale_mode: SashimiScaleMode::Shared,
        sashimi_count_mode: SashimiCountMode::Raw,
        ..SessionFilters::default()
    };
    let raw = alignment_scene_sashimi_group(&filters).unwrap();
    assert_eq!(raw, ALIGNMENT_SASHIMI_RAW_GROUP);
    filters.sashimi_count_mode = SashimiCountMode::CountsPerMillion;
    let cpm = alignment_scene_sashimi_group(&filters).unwrap();
    assert_eq!(cpm, ALIGNMENT_SASHIMI_CPM_GROUP);
    assert_ne!(raw, cpm);
    filters.sashimi_scale_mode = SashimiScaleMode::Automatic;
    assert_eq!(alignment_scene_sashimi_group(&filters), None);
}

#[test]
fn scene_modification_options_capture_style_selection_and_normalized_hidden_codes() {
    let filters = SessionFilters {
        modified_base_color_mode: ModifiedBaseColorMode::ModifiedVsUnmodified,
        hidden_modification_codes: vec![
            "m".to_owned(),
            "CHEBI:76792".to_owned(),
            "m".to_owned(),
            "not-a-code".to_owned(),
        ],
        ..SessionFilters::default()
    };
    let options = alignment_scene_modification_lane_options(&filters, Some(123));
    assert!(options.projection.probability_weighted);
    assert_eq!(options.projection.hidden_codes.len(), 2);
    assert!(
        options
            .projection
            .hidden_codes
            .contains(&ModificationCode::Letter(b'm'))
    );
    assert!(
        options
            .projection
            .hidden_codes
            .contains(&ModificationCode::Chebi(76_792))
    );
    assert_eq!(
        options.color_mode,
        ModifiedBaseColorMode::ModifiedVsUnmodified
    );
    assert_eq!(options.selected_position, Some(123));
}

#[test]
fn scene_lane_clips_use_final_pane_layout_and_stable_subtrack_identity() {
    let track_id = TrackId::new(91);
    let mut annotation = ui::PaneLayoutSnapshot {
        channel: ui::TrackPane::Annotation,
        pane_rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 100.0)),
        content_bottom: 150.0,
        scroll_points: 0.0,
        stack_scroll_points: 0.0,
        items: vec![ui::PaneItemLayout {
            id: PresentationNodeId::Track {
                channel: ui::TrackPane::Annotation,
                track_id,
            },
            frame: TrackFrame {
                key: TrackFrameKey::Alignment(track_id),
                channel: ui::TrackPane::Annotation,
                name: "RNA".to_owned(),
                kind: TrackKind::Alignment,
                source_label: None,
                top: 20.0,
                bottom: 150.0,
                visible: true,
                collapsed: false,
                frozen_bottom: None,
                subtracks: vec![TrackSubtrack {
                    alignment_subtrack: Some(ui::AlignmentSubtrack::Coverage),
                    // Deliberately not the canonical label: semantics must come from the identity.
                    label: "Depth profile".to_owned(),
                    detail: None,
                    top: 50.0,
                    bottom: 120.0,
                    collapsed: false,
                    composite_member: None,
                }],
            },
            row_rect: egui::Rect::ZERO,
            header_rect: egui::Rect::ZERO,
            content_rect: None,
            clip_rect: egui::Rect::ZERO,
            desired_content_visible: true,
            effective_content_visible: true,
        }],
    };
    annotation.reproject_scroll(10.0);
    let captures = alignment_scene_derived_lane_captures(
        &annotation,
        track_id,
        168.0,
        632.0,
        &SessionFilters::default(),
        true,
        None,
        Some(117),
        None,
    )
    .unwrap();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].clip.x, 168.0);
    assert_eq!(captures[0].clip.width, 632.0);
    assert_eq!(captures[0].clip.y, 40.0);
    assert_eq!(captures[0].clip.height, 60.0);
    let AlignmentDerivedLaneOptions::Coverage(options) = captures[0].options else {
        panic!("expected coverage capture");
    };
    assert_eq!(options.depth.selected_position, Some(117));
}

#[test]
fn read_scroll_is_reset_only_when_its_scene_row_authority_changes() {
    let mut state = ReadPanelScrollState {
        scroll_points: 420.0,
        content_origin_points: 20.0,
        latent_scroll_fraction: Some(0.75),
        content_bottom_points: 900.0,
        density: Some(2.5),
        stable_row_anchor: Some(StableRowCameraAnchor {
            row: 40,
            offset_fraction: 0.5,
            coordinate_space: RowCoordinateSpaceRevision::new(
                rgv_alignment_scene::RevisionFingerprint::new(1, 7),
            ),
            row_height: 8.0,
        }),
        scene_contig: Some(ContigId::new(6)),
    };

    assert!(!state.bind_scene_contig(ContigId::new(6), false));
    assert_eq!(state.scroll_points, 420.0);
    assert!(state.bind_scene_contig(ContigId::new(2), false));
    assert_eq!(state.scroll_points, 0.0);
    assert_eq!(state.content_bottom_points, 0.0);
    assert_eq!(state.latent_scroll_fraction, None);
    assert_eq!(state.stable_row_anchor, None);
    assert_eq!(state.density, Some(2.5));
    assert_eq!(state.scene_contig, Some(ContigId::new(2)));
}

#[test]
fn index_only_overview_retires_read_row_authority_but_preserves_density() {
    let mut state = ReadPanelScrollState {
        scroll_points: 420.0,
        content_origin_points: 20.0,
        latent_scroll_fraction: Some(0.75),
        content_bottom_points: 900.0,
        density: Some(2.5),
        stable_row_anchor: Some(StableRowCameraAnchor {
            row: 40,
            offset_fraction: 0.5,
            coordinate_space: RowCoordinateSpaceRevision::new(
                rgv_alignment_scene::RevisionFingerprint::new(1, 7),
            ),
            row_height: 8.0,
        }),
        scene_contig: Some(ContigId::new(6)),
    };

    assert!(state.retire_scene_authority());
    assert_eq!(state.scroll_points, 0.0);
    assert_eq!(state.content_origin_points, 0.0);
    assert_eq!(state.content_bottom_points, 0.0);
    assert_eq!(state.latent_scroll_fraction, None);
    assert_eq!(state.stable_row_anchor, None);
    assert_eq!(state.scene_contig, None);
    assert_eq!(state.density, Some(2.5));
    assert!(!state.retire_scene_authority());
}

#[test]
fn stale_atlas_intervals_cannot_authorize_a_discontinuous_camera() {
    let actb = GenomicInterval::new(ContigId::new(6), 5_500_000, 5_600_000).unwrap();
    let actb_edge = GenomicInterval::new(ContigId::new(6), 5_590_000, 5_610_000).unwrap();
    let same_contig_jump = GenomicInterval::new(ContigId::new(6), 10_000_000, 10_010_000).unwrap();
    let bap1 = GenomicInterval::new(ContigId::new(2), 52_400_000, 52_410_000).unwrap();

    assert!(alignment_scene_intervals_share_authority(actb, actb_edge));
    assert!(!alignment_scene_intervals_share_authority(
        actb,
        same_contig_jump
    ));
    assert!(!alignment_scene_intervals_share_authority(actb, bap1));
}

#[test]
fn stripping_a_discontinuous_snapshot_preserves_the_new_camera_reference_requirement() {
    let actb = GenomicInterval::new(ContigId::new(6), 5_500_000, 5_600_000).unwrap();
    let bap1 = GenomicInterval::new(ContigId::new(2), 52_400_000, 52_410_000).unwrap();
    let revision = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));
    let requirement = AlignmentReferenceRequirement::Pending {
        revision,
        horizon: bap1,
    };
    let camera = AlignmentSceneCamera::from(
        AlignmentCamera::new(
            bap1,
            0.0,
            7.0,
            5.0,
            CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 500.0,
            },
        )
        .unwrap(),
    );
    let frame = AlignmentCoordinatorFrame {
        camera: camera.into(),
        camera_sequence: 2,
        snapshot: None,
        row_activity: None,
        covered_interval: Some(actb),
        requested_rows: 0..10,
        covered_rows: 0..10,
        reference_requirement: requirement,
        scene_contracts_current: true,
        reference_products_current: false,
        individual_reads: AlignmentIndividualReadCapability::Pending,
        vertical_residency: AlignmentVerticalResidency::ViewportBounded,
    };

    let normalized = alignment_scene_frame_for_ui_against_retained_interval(frame, Some(actb));
    assert_eq!(normalized.reference_requirement, requirement);
    assert!(!normalized.scene_contracts_current);
    assert_eq!(normalized.covered_interval, None);
    assert!(!normalized.visible_complete());
}

#[test]
fn stripping_a_discontinuous_snapshot_preserves_only_a_camera_covering_refusal() {
    let actb = GenomicInterval::new(ContigId::new(6), 5_500_000, 5_600_000).unwrap();
    let bap1 = GenomicInterval::new(ContigId::new(2), 52_400_000, 52_410_000).unwrap();
    let camera = AlignmentCamera::new(
        bap1,
        0.0,
        7.0,
        5.0,
        CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 1_000.0,
            height: 500.0,
        },
    )
    .unwrap();
    let reference_revision =
        ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));
    let frame = AlignmentCoordinatorFrame {
        camera: camera.into(),
        camera_sequence: 3,
        snapshot: None,
        row_activity: None,
        covered_interval: Some(actb),
        requested_rows: 0..10,
        covered_rows: 0..10,
        reference_requirement: AlignmentReferenceRequirement::Pending {
            revision: reference_revision,
            horizon: bap1,
        },
        scene_contracts_current: true,
        reference_products_current: false,
        individual_reads: AlignmentIndividualReadCapability::Refused {
            target: GenomicInterval::new(ContigId::new(2), 52_390_000, 52_420_000).unwrap(),
            complete_read_limit: 50_000,
            eligible_records: 86_798,
        },
        vertical_residency: AlignmentVerticalResidency::ViewportBounded,
    };

    let normalized =
        alignment_scene_frame_for_ui_against_retained_interval(frame.clone(), Some(actb));
    assert!(normalized.individual_reads_refused_for_camera());
    assert_eq!(
        alignment_scene_frame_settlement(&normalized),
        AlignmentSceneFrameSettlement::Settled,
        "a later typed frame must settle the request even if its FASTA is still pending"
    );

    let mut stale = frame;
    stale.individual_reads = AlignmentIndividualReadCapability::Refused {
        target: GenomicInterval::new(ContigId::new(2), 52_405_000, 52_406_000).unwrap(),
        complete_read_limit: 50_000,
        eligible_records: 86_798,
    };
    let normalized_stale =
        alignment_scene_frame_for_ui_against_retained_interval(stale, Some(actb));
    assert_eq!(
        normalized_stale.individual_reads,
        AlignmentIndividualReadCapability::Pending,
        "a refusal which does not cover the current camera is stale, not terminal"
    );
    assert_eq!(
        alignment_scene_frame_settlement(&normalized_stale),
        AlignmentSceneFrameSettlement::WaitingForScene
    );
}

#[test]
fn overview_resolution_is_index_only_and_regional_resolution_requests_records() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 1_000_000).unwrap();
    let overview =
        AlignmentSceneResolution::for_viewport(Viewport::new(interval, 1_000.0).unwrap());
    let regional =
        AlignmentSceneResolution::for_viewport(Viewport::new(interval, 1_001.0).unwrap());

    assert_eq!(overview.detail, DetailLevel::Overview);
    assert!(!overview.requires_alignment_records());
    assert_eq!(regional.detail, DetailLevel::Regional);
    assert!(regional.requires_alignment_records());
}

#[test]
fn browsing_buffer_setting_controls_horizontal_and_vertical_scene_margins() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 1_000).unwrap();
    let camera = AlignmentSceneCamera::from(
        AlignmentCamera::new(
            interval,
            0.0,
            10.0,
            8.0,
            CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 100.0,
            },
        )
        .unwrap(),
    );
    let visible_rows = u32::try_from(camera.visible_rows().len()).unwrap();

    let disabled = alignment_scene_preparation_policy(0, camera);
    assert_eq!(disabled.behind_viewports, 0.0);
    assert_eq!(disabled.vertical_margin_rows, 0);
    assert_eq!(disabled.vertical_trigger_rows, 1);
    assert!(disabled.full_vertical_residency);

    let default = alignment_scene_preparation_policy(100, camera);
    assert_eq!(default.behind_viewports, 1.0);
    assert_eq!(default.vertical_margin_rows, visible_rows);
    assert_eq!(
        default.vertical_trigger_rows,
        visible_rows.saturating_add(3) / 4
    );

    let expanded = alignment_scene_preparation_policy(400, camera);
    assert_eq!(expanded.behind_viewports, 4.0);
    assert_eq!(
        expanded.vertical_margin_rows,
        visible_rows.saturating_mul(4)
    );
    assert_eq!(expanded.vertical_trigger_rows, visible_rows);
    assert!(expanded.full_vertical_residency);
}

#[test]
fn projection_is_checking_until_the_current_generation_settles() {
    let mut projection = AlignmentPanelProjection::default();
    assert!(projection.scene_is_checking());

    projection.navigation.begin(
        projection.clock.current().get(),
        AlignmentSceneRequestToken::new(1),
        "test target",
    );
    projection
        .navigation
        .transition(AlignmentNavigationPhase::Running, "source owns target");
    projection
        .navigation
        .transition(AlignmentNavigationPhase::CandidateReady, "candidate ready");
    projection
        .navigation
        .transition(AlignmentNavigationPhase::Committed, "candidate committed");
    assert!(projection.scene_is_current());

    projection.clock.advance();
    assert!(projection.scene_is_checking());
    assert!(!projection.scene_is_current());

    projection.navigation.begin(
        projection.clock.current().get(),
        AlignmentSceneRequestToken::new(2),
        "new target",
    );
    assert!(projection.scene_is_checking());

    projection
        .navigation
        .transition(AlignmentNavigationPhase::Running, "source owns target");
    projection
        .navigation
        .transition(AlignmentNavigationPhase::CandidateReady, "candidate ready");
    projection
        .navigation
        .transition(AlignmentNavigationPhase::Committed, "committed");
    assert!(projection.scene_is_current());
}

#[test]
fn terminal_scene_error_settles_generation_without_discarding_scene_authority() {
    let mut projection = AlignmentPanelProjection::default();
    projection.advance_scene_generation();
    projection.navigation.begin(
        projection.clock.current().get(),
        AlignmentSceneRequestToken::new(7),
        "test target",
    );
    projection.begin_checking(Instant::now());
    projection.scene_owner = Some(SourceOwnerId::new(7));

    projection.settle_unavailable("row atlas failed".to_owned());

    assert!(projection.scene_is_current());
    assert_eq!(projection.unavailable.as_deref(), Some("row atlas failed"));
    assert_eq!(projection.scene_owner, Some(SourceOwnerId::new(7)));
    assert!(projection.checking_started.is_none());
}

#[test]
fn checking_notice_is_debounced_without_delaying_the_truth_state() {
    let mut projection = AlignmentPanelProjection::default();
    projection.clock.advance();
    projection.navigation.begin(
        projection.clock.current().get(),
        AlignmentSceneRequestToken::new(1),
        "test target",
    );
    projection
        .navigation
        .transition(AlignmentNavigationPhase::Running, "source owns target");
    let started = Instant::now();
    projection.begin_checking(started);

    assert!(projection.scene_is_checking());
    assert!(!projection.checking_notice_visible);
    assert!(!projection.reveal_checking_notice_if_due(
        started + READ_CHECKING_NOTICE_DELAY - Duration::from_millis(1)
    ));
    assert!(projection.scene_is_checking());
    assert!(projection.reveal_checking_notice_if_due(started + READ_CHECKING_NOTICE_DELAY));
    assert!(projection.checking_notice_visible);
    assert!(!projection.reveal_checking_notice_if_due(
        started + READ_CHECKING_NOTICE_DELAY + Duration::from_secs(1)
    ));

    projection.finish_checking();
    projection
        .navigation
        .transition(AlignmentNavigationPhase::CandidateReady, "candidate ready");
    projection
        .navigation
        .transition(AlignmentNavigationPhase::Committed, "committed");
    assert!(projection.scene_is_current());
    assert!(!projection.checking_notice_visible);
    assert!(projection.checking_started.is_none());
}

#[test]
fn navigation_transaction_coalesces_targets_without_owning_pixels() {
    let initial = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let first_target = GenomicInterval::new(ContigId::new(0), 75, 225).unwrap();
    let second_target = GenomicInterval::new(ContigId::new(0), 50, 250).unwrap();
    let started = Instant::now();
    let mut presentation = NavigationPresentation::initial(initial, 1, started);
    presentation.phase = NavigationPresentationPhase::Committing;
    assert!(presentation.finish_commit(9, 4));

    presentation.begin_navigation(
        test_viewport(first_target),
        2,
        9,
        4,
        "100 complete reads",
        started,
    );
    presentation.begin_navigation(
        test_viewport(second_target),
        3,
        9,
        4,
        "replacement status must not win",
        started + Duration::from_millis(20),
    );

    assert_eq!(presentation.phase, NavigationPresentationPhase::Acquiring);
    assert_eq!(presentation.target_interval, second_target);
    assert_eq!(presentation.source_status, "100 complete reads");
    assert!(!presentation.is_interactive());
}

#[test]
fn navigation_contract_invalidation_is_control_state_only() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let now = Instant::now();
    let mut presentation = NavigationPresentation::initial(interval, 1, now);
    presentation.invalidate_contract("workspace layout changed");
    assert_eq!(presentation.phase, NavigationPresentationPhase::Acquiring);
    assert_eq!(
        presentation.invalidation_reason.as_deref(),
        Some("workspace layout changed")
    );
}

#[test]
fn navigation_notice_is_delayed_but_transaction_state_is_immediate() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let started = Instant::now();
    let mut presentation = NavigationPresentation::initial(interval, 1, started);
    assert!(presentation.is_unsettled());
    assert!(!presentation.is_interactive());
    assert!(
        !presentation
            .reveal_notice_if_due(started + NAVIGATION_NOTICE_DELAY - Duration::from_millis(1))
    );
    assert!(presentation.reveal_notice_if_due(started + NAVIGATION_NOTICE_DELAY));
    assert!(presentation.notice_visible);
}

#[test]
fn unchanged_viewports_do_not_start_duplicate_navigation_targets() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let viewport = test_viewport(interval);
    let mut presentation = NavigationPresentation::initial(interval, 1, Instant::now());
    assert!(presentation.targets_viewport(viewport));

    let shifted = viewport.pan_pixels(1.0, 10_000).unwrap();
    assert!(!presentation.targets_viewport(shifted));
    presentation.begin_navigation(shifted, 2, 0, 0, "", Instant::now());
    assert!(presentation.targets_viewport(shifted));
}

#[test]
fn navigation_scheduler_coalesces_idle_bursts_and_dispatches_the_latest_target() {
    let interval_a = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let interval_b = GenomicInterval::new(ContigId::new(0), 110, 210).unwrap();
    let viewport_a = Viewport::new(interval_a, 1_000.0).unwrap();
    let viewport_b = Viewport::new(interval_b, 1_000.0).unwrap();
    let started = Instant::now();
    let mut schedule = NavigationQuerySchedule::default();
    schedule.update(
        NavigationQueryTarget {
            fingerprint: NavigationTargetFingerprint::from_viewport(viewport_a),
            generation: 1,
        },
        started,
        true,
        false,
    );
    schedule.update(
        NavigationQueryTarget {
            fingerprint: NavigationTargetFingerprint::from_viewport(viewport_b),
            generation: 2,
        },
        started + Duration::from_millis(20),
        true,
        false,
    );

    assert!(
        schedule
            .take_due(started + Duration::from_millis(69))
            .is_none()
    );
    let dispatched = schedule
        .take_due(started + Duration::from_millis(70))
        .unwrap();
    assert_eq!(dispatched.fingerprint.interval, interval_b);
    assert_eq!(dispatched.generation, 2);
    assert_eq!(schedule.coalesced_updates, 1);
}

#[test]
fn navigation_scheduler_gives_independent_wheel_bursts_fresh_idle_windows() {
    let started = Instant::now();
    let first = NavigationQueryTarget {
        fingerprint: NavigationTargetFingerprint::from_viewport(test_viewport(
            GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        )),
        generation: 1,
    };
    let second = NavigationQueryTarget {
        fingerprint: NavigationTargetFingerprint::from_viewport(test_viewport(
            GenomicInterval::new(ContigId::new(0), 120, 220).unwrap(),
        )),
        generation: 2,
    };
    let mut schedule = NavigationQuerySchedule::default();
    schedule.update(first, started, true, false);
    assert_eq!(
        schedule.take_due(started + NAVIGATION_QUERY_IDLE_DELAY),
        Some(first)
    );
    assert!(!schedule.gesture_active);

    let next_burst = started + Duration::from_secs(1);
    schedule.update(second, next_burst, true, false);
    assert!(
        schedule
            .take_due(next_burst + NAVIGATION_QUERY_IDLE_DELAY - Duration::from_millis(1))
            .is_none()
    );
    assert_eq!(
        schedule.take_due(next_burst + NAVIGATION_QUERY_IDLE_DELAY),
        Some(second)
    );
}

#[test]
fn navigation_scheduler_uses_bounded_cadence_and_immediate_gesture_end() {
    let started = Instant::now();
    let interval_a = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let interval_b = GenomicInterval::new(ContigId::new(0), 120, 220).unwrap();
    let mut schedule = NavigationQuerySchedule::default();
    schedule.update(
        NavigationQueryTarget {
            fingerprint: NavigationTargetFingerprint::from_viewport(
                Viewport::new(interval_a, 1_000.0).unwrap(),
            ),
            generation: 1,
        },
        started,
        true,
        true,
    );
    assert!(
        schedule
            .take_due(started + NAVIGATION_QUERY_MAX_CADENCE)
            .is_some()
    );

    schedule.update(
        NavigationQueryTarget {
            fingerprint: NavigationTargetFingerprint::from_viewport(
                Viewport::new(interval_b, 1_000.0).unwrap(),
            ),
            generation: 2,
        },
        started + Duration::from_millis(160),
        true,
        true,
    );
    let final_target = schedule
        .take_immediate(started + Duration::from_millis(161), true)
        .unwrap();
    assert_eq!(final_target.fingerprint.interval, interval_b);
    assert!(!schedule.gesture_active);
    assert!(!schedule.hold_commit_until_end);
    assert_eq!(schedule.gesture_end_dispatches, 1);
}

#[test]
fn navigation_scheduler_suppresses_only_the_same_target_generation() {
    let now = Instant::now();
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        1_000.0,
    )
    .unwrap();
    let target = NavigationQueryTarget {
        fingerprint: NavigationTargetFingerprint::from_viewport(viewport),
        generation: 1,
    };
    let mut schedule = NavigationQuerySchedule::default();
    schedule.update(target, now, false, false);
    assert_eq!(schedule.take_immediate(now, false), Some(target));

    schedule.update(target, now, false, false);
    assert!(schedule.take_immediate(now, false).is_none());

    let replacement_generation = NavigationQueryTarget {
        generation: 2,
        ..target
    };
    schedule.update(replacement_generation, now, false, false);
    assert_eq!(
        schedule.take_immediate(now, false),
        Some(replacement_generation)
    );
}

#[test]
fn pointer_gesture_holds_atomic_commit_after_a_cadence_query_finishes() {
    let started = Instant::now();
    let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let mut schedule = NavigationQuerySchedule::default();
    schedule.update(
        NavigationQueryTarget {
            fingerprint: NavigationTargetFingerprint::from_viewport(
                Viewport::new(interval, 1_000.0).unwrap(),
            ),
            generation: 1,
        },
        started,
        true,
        true,
    );

    assert!(
        schedule
            .take_due(started + NAVIGATION_QUERY_MAX_CADENCE)
            .is_some()
    );
    assert!(schedule.hold_commit_until_end);
    assert!(
        schedule
            .take_immediate(started + NAVIGATION_QUERY_MAX_CADENCE, true)
            .is_none()
    );
    assert!(!schedule.hold_commit_until_end);
}

#[test]
fn pointer_release_keeps_transaction_open_until_target_row_geometry_is_ready() {
    assert!(!navigation_target_ready_for_commit(
        NavigationPresentationPhase::Acquiring,
        false,
        true,
        false,
    ));
    assert!(navigation_target_ready_for_commit(
        NavigationPresentationPhase::Acquiring,
        false,
        true,
        true,
    ));
    assert!(!navigation_target_ready_for_commit(
        NavigationPresentationPhase::Acquiring,
        true,
        true,
        true,
    ));
}

#[test]
fn navigation_commit_remains_noninteractive_until_frame_submission() {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let now = Instant::now();
    let mut presentation = NavigationPresentation::initial(interval, 1, now);

    presentation.mark_ready_to_commit();
    assert!(!presentation.is_interactive());
    assert!(presentation.begin_commit());
    assert!(!presentation.is_interactive());
    assert!(presentation.finish_commit(1, 1));
    assert!(presentation.is_interactive());
}

#[test]
fn zoom_out_during_pan_has_a_deterministic_truthful_atomic_trace() {
    let source = GenomicInterval::new(ContigId::new(0), 1_000, 1_100).unwrap();
    let first_target = GenomicInterval::new(ContigId::new(0), 950, 1_150).unwrap();
    let final_target = GenomicInterval::new(ContigId::new(0), 900, 1_300).unwrap();
    let started = Instant::now();
    let mut presentation = NavigationPresentation::initial(source, 1, started);
    presentation.phase = NavigationPresentationPhase::Committing;
    assert!(presentation.finish_commit(1, 1));
    let mut schedule = NavigationQuerySchedule::default();
    let mut trace = Vec::new();

    presentation.begin_navigation(test_viewport(first_target), 2, 1, 1, "complete", started);
    schedule.update(
        NavigationQueryTarget {
            fingerprint: NavigationTargetFingerprint::from_viewport(test_viewport(first_target)),
            generation: 2,
        },
        started,
        true,
        true,
    );
    trace.push((presentation.phase, presentation.is_interactive()));

    presentation.begin_navigation(
        test_viewport(final_target),
        3,
        1,
        1,
        "must remain source status",
        started + Duration::from_millis(20),
    );
    let final_query = NavigationQueryTarget {
        fingerprint: NavigationTargetFingerprint::from_viewport(test_viewport(final_target)),
        generation: 3,
    };
    schedule.record_superseded_target();
    schedule.update(final_query, started + Duration::from_millis(20), true, true);
    trace.push((presentation.phase, presentation.is_interactive()));

    assert_eq!(
        schedule.take_due(started + NAVIGATION_QUERY_MAX_CADENCE),
        Some(final_query)
    );
    assert!(
        schedule
            .take_immediate(started + Duration::from_millis(151), true)
            .is_none()
    );
    assert!(!schedule.hold_commit_until_end);
    presentation.mark_ready_to_commit();
    trace.push((presentation.phase, presentation.is_interactive()));
    assert!(presentation.begin_commit());
    trace.push((presentation.phase, presentation.is_interactive()));
    assert!(presentation.finish_commit(1, 1));
    schedule.record_commit(Duration::from_millis(180));
    trace.push((presentation.phase, presentation.is_interactive()));

    assert_eq!(
        trace,
        vec![
            (NavigationPresentationPhase::Acquiring, false),
            (NavigationPresentationPhase::Acquiring, false),
            (NavigationPresentationPhase::ReadyToCommit, false),
            (NavigationPresentationPhase::Committing, false),
            (NavigationPresentationPhase::Settled, true),
        ]
    );
    assert_eq!(schedule.dispatched_targets, 1);
    assert_eq!(schedule.coalesced_updates, 1);
    assert_eq!(schedule.cancelled_targets, 1);
    assert_eq!(schedule.committed_targets, 1);
    assert_eq!(schedule.last_commit_micros, 180_000);
}

#[test]
fn index_only_overview_settles_as_an_explicit_scene_state() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 1_000_000).unwrap();
    let resolution =
        AlignmentSceneResolution::for_viewport(Viewport::new(interval, 1_000.0).unwrap());
    let mut projection = AlignmentPanelProjection {
        unavailable: Some("stale failure".to_owned()),
        ..AlignmentPanelProjection::default()
    };

    projection.settle_index_only_overview(resolution);

    assert!(projection.unavailable.is_none());
    assert!(projection.index_only_overview);
    assert_eq!(projection.last_scene_resolution, Some(resolution));
    assert!(projection.scene_is_current());
}

#[test]
fn shell_resize_refreshes_only_after_crossing_a_scene_resolution_boundary() {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 20_000).unwrap();
    let previous =
        AlignmentSceneResolution::for_viewport(Viewport::new(interval, 1_000.0).unwrap());
    let same_bucket =
        AlignmentSceneResolution::for_viewport(Viewport::new(interval, 1_020.0).unwrap());
    let next_bucket =
        AlignmentSceneResolution::for_viewport(Viewport::new(interval, 1_090.0).unwrap());

    assert!(!same_bucket.requires_refresh_from(previous));
    assert!(next_bucket.requires_refresh_from(previous));

    let read_interval = GenomicInterval::new(ContigId::new(0), 0, 1_000).unwrap();
    let allele_exact =
        AlignmentSceneResolution::for_viewport(Viewport::new(read_interval, 600.0).unwrap());
    let aggregated =
        AlignmentSceneResolution::for_viewport(Viewport::new(read_interval, 400.0).unwrap());
    assert!(aggregated.requires_refresh_from(allele_exact));
}

#[test]
fn coverage_alleles_are_collected_at_read_detail_with_exact_bounded_bins() {
    let (bins, collect_alleles) = alignment_coverage_query_plan(1_060, 1_390.0, 0.763);
    assert_eq!(bins, 1_060);
    assert!(collect_alleles);

    let (bins, collect_alleles) = alignment_coverage_query_plan(
        MAXIMUM_COVERAGE_BINS + 1,
        5_000.0,
        MAXIMUM_ALLELE_COVERAGE_BASES_PER_PIXEL,
    );
    assert_eq!(bins, MAXIMUM_COVERAGE_BINS);
    assert!(!collect_alleles);

    let (bins, collect_alleles) = alignment_coverage_query_plan(1_930, 1_390.0, 1.39);
    assert_eq!(bins, 1_930);
    assert!(collect_alleles);

    let (bins, collect_alleles) = alignment_coverage_query_plan(2_800, 1_390.0, 2.01);
    assert_eq!(bins, 1_390);
    assert!(!collect_alleles);
}

#[test]
fn reference_aware_read_modes_request_context_before_base_detail() {
    assert!(alignment_query_needs_reference(
        AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
        DetailLevel::Reads,
        5.0,
    ));
    assert!(alignment_query_needs_reference(
        AlignmentColorMode::None,
        DetailLevel::Bases,
        0.125,
    ));
    assert!(alignment_query_needs_reference(
        AlignmentColorMode::None,
        DetailLevel::Reads,
        MAXIMUM_BASE_MARK_BASES_PER_PIXEL,
    ));
    assert!(alignment_query_needs_reference(
        AlignmentColorMode::Ultima(UltimaDisplayOptions::default()),
        DetailLevel::Reads,
        5.0,
    ));
    assert!(!alignment_query_needs_reference(
        AlignmentColorMode::None,
        DetailLevel::Reads,
        MAXIMUM_BASE_MARK_BASES_PER_PIXEL + 0.01,
    ));
    assert!(!alignment_query_needs_reference(
        AlignmentColorMode::Bisulfite(BisulfiteContext::Cg),
        DetailLevel::Regional,
        10.0,
    ));
    assert!(!alignment_query_needs_reference(
        AlignmentColorMode::SmrtKinetics(SmrtKineticsMode::CcsForwardIpd),
        DetailLevel::Reads,
        5.0,
    ));
    assert!(!alignment_query_needs_reference(
        AlignmentColorMode::Ultima(UltimaDisplayOptions::default()),
        DetailLevel::Regional,
        10.0,
    ));
}

#[test]
fn only_base_scene_geometry_depends_on_the_current_fasta_revision() {
    let revision = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));

    assert_eq!(
        alignment_scene_reference_revision(GeometryLod::Base, Some(revision)),
        Some(revision)
    );
    for lod in [GeometryLod::Overview, GeometryLod::Span, GeometryLod::Read] {
        assert_eq!(
            alignment_scene_reference_revision(lod, Some(revision)),
            None
        );
    }
    assert_eq!(
        alignment_scene_reference_revision(GeometryLod::Base, None),
        None
    );
}

#[test]
fn fasta_completion_must_match_the_open_revision_and_exact_requested_horizon() {
    let current = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));
    let stale = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 10));
    let requested = GenomicInterval::new(ContigId::new(6), 100, 300).unwrap();
    let superseded = GenomicInterval::new(ContigId::new(6), 125, 325).unwrap();

    assert!(reference_query_completion_matches(
        Some(current),
        Some(current),
        Some(requested),
        current,
        requested,
    ));
    assert!(!reference_query_completion_matches(
        Some(current),
        Some(stale),
        Some(requested),
        stale,
        requested,
    ));
    assert!(!reference_query_completion_matches(
        Some(current),
        Some(current),
        Some(superseded),
        current,
        requested,
    ));
}

#[test]
fn fasta_failure_settles_only_the_pending_dependency_it_attempted() {
    let current = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));
    let stale = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 10));
    let requested = GenomicInterval::new(ContigId::new(6), 100, 300).unwrap();
    let horizon = GenomicInterval::new(ContigId::new(6), 125, 275).unwrap();
    let outside = GenomicInterval::new(ContigId::new(6), 75, 325).unwrap();

    assert!(reference_failure_matches_requirement(
        current,
        requested,
        AlignmentReferenceRequirement::Pending {
            revision: current,
            horizon,
        },
    ));
    assert!(!reference_failure_matches_requirement(
        current,
        requested,
        AlignmentReferenceRequirement::Pending {
            revision: stale,
            horizon,
        },
    ));
    assert!(!reference_failure_matches_requirement(
        current,
        requested,
        AlignmentReferenceRequirement::Pending {
            revision: current,
            horizon: outside,
        },
    ));
    assert!(!reference_failure_matches_requirement(
        current,
        requested,
        AlignmentReferenceRequirement::Ready {
            revision: current,
            horizon,
        },
    ));
}

#[test]
fn fasta_open_failure_clears_authority_and_settles_the_current_panel_generation() {
    let mut panel = test_panel(1);
    panel.generation_clock.advance();
    let generation = panel.generation_clock.current().get();
    let revision = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));
    let interval = panel.viewport.interval();
    panel.reference_slice = Some(Arc::new(ReferenceSlice {
        interval,
        bases: vec![b'A'; interval.len() as usize],
    }));
    panel.reference_slice_revision = Some(revision);
    panel.reference_slice_generation = Some(generation);
    panel.reference_request_revision = Some(revision);
    panel.reference_request_interval = Some(interval);
    panel.pending_queries.reference = true;

    panel.settle_reference_open_failure();

    assert!(panel.reference_slice.is_none());
    assert_eq!(panel.reference_slice_revision, None);
    assert_eq!(panel.reference_request_revision, None);
    assert_eq!(panel.reference_request_interval, None);
    assert!(!panel.pending_queries.reference);
    assert_eq!(panel.reference_settled_generation, Some(generation));
    assert_eq!(panel.reference_query_error_generation, Some(generation));
}

#[test]
fn compatible_pending_reference_lanes_share_one_bounded_union_query() {
    let revision = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));
    let left = GenomicInterval::new(ContigId::new(6), 100, 250).unwrap();
    let right = GenomicInterval::new(ContigId::new(6), 200, 400).unwrap();
    let requirements = [
        AlignmentReferenceLaneRequirement {
            owner: SourceOwnerId::new(2),
            revision,
            horizon: right,
        },
        AlignmentReferenceLaneRequirement {
            owner: SourceOwnerId::new(1),
            revision,
            horizon: left,
        },
    ];

    assert_eq!(
        plan_alignment_reference_lanes(&requirements, &BTreeMap::new()),
        AlignmentReferenceLanePlan::Query {
            revision,
            interval: GenomicInterval::new(ContigId::new(6), 100, 400).unwrap(),
        }
    );
}

#[test]
fn queued_reference_lane_receipts_sequence_remaining_incompatible_owners() {
    let revision = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));
    let first = AlignmentReferenceLaneRequirement {
        owner: SourceOwnerId::new(1),
        revision,
        horizon: GenomicInterval::new(ContigId::new(6), 100, 250).unwrap(),
    };
    let second = AlignmentReferenceLaneRequirement {
        owner: SourceOwnerId::new(2),
        revision,
        horizon: GenomicInterval::new(ContigId::new(2), 500, 700).unwrap(),
    };
    let requirements = [first, second];
    assert_eq!(
        plan_alignment_reference_lanes(&requirements, &BTreeMap::new()),
        AlignmentReferenceLanePlan::Query {
            revision,
            interval: first.horizon,
        }
    );

    let mut queued = BTreeMap::from([(first.owner, (first.revision, first.horizon))]);
    assert_eq!(
        plan_alignment_reference_lanes(&requirements, &queued),
        AlignmentReferenceLanePlan::Query {
            revision,
            interval: second.horizon,
        }
    );
    queued.insert(second.owner, (second.revision, second.horizon));
    assert_eq!(
        plan_alignment_reference_lanes(&requirements, &queued),
        AlignmentReferenceLanePlan::AllQueued
    );
}

#[test]
fn automation_and_navigation_share_current_reference_error_settlement() {
    let revision = ReferenceRevision::new(rgv_alignment_scene::RevisionFingerprint::new(7, 11));
    let interval = GenomicInterval::new(ContigId::new(6), 100, 250).unwrap();
    let secondary_lane_interval = GenomicInterval::new(ContigId::new(2), 100, 250).unwrap();

    assert!(reference_dependency_is_settled(
        true,
        9,
        Some(revision),
        None,
        None,
        interval,
        None,
        Some(9),
    ));
    assert!(!reference_dependency_is_settled(
        true,
        9,
        Some(revision),
        None,
        None,
        interval,
        None,
        Some(8),
    ));
    assert!(!reference_dependency_is_settled(
        true,
        9,
        Some(revision),
        Some(revision),
        Some(secondary_lane_interval),
        interval,
        Some(9),
        None,
    ));
}

#[test]
fn scene_event_tokens_never_treat_absent_or_superseded_requests_as_current() {
    let current = AlignmentSceneRequestToken::new(12);
    let stale = AlignmentSceneRequestToken::new(11);

    assert!(alignment_scene_event_token_is_current(
        Some(current),
        Some(current)
    ));
    assert!(!alignment_scene_event_token_is_current(None, None));
    assert!(!alignment_scene_event_token_is_current(
        Some(current),
        Some(stale)
    ));
    assert!(!alignment_scene_event_token_is_current(None, Some(current)));
}

#[test]
fn scene_frame_acceptance_waits_for_the_exact_transaction_camera() {
    let current = AlignmentSceneRequestToken::new(12);
    let stale = AlignmentSceneRequestToken::new(11);
    let interval = GenomicInterval::new(ContigId::new(6), 100, 200).unwrap();
    let shifted = GenomicInterval::new(ContigId::new(6), 150, 250).unwrap();
    let camera = AlignmentSceneCamera::from(
        AlignmentCamera::new(
            interval,
            0.0,
            7.0,
            5.0,
            CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 500.0,
            },
        )
        .unwrap(),
    );
    let shifted_camera = AlignmentSceneCamera::from(
        AlignmentCamera::new(
            shifted,
            0.0,
            7.0,
            5.0,
            CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 500.0,
            },
        )
        .unwrap(),
    );

    assert!(alignment_scene_frame_is_current(
        Some(current),
        current,
        Some(camera),
        camera,
    ));
    assert!(!alignment_scene_frame_is_current(
        Some(current),
        stale,
        Some(camera),
        camera,
    ));
    assert!(!alignment_scene_frame_is_current(
        Some(current),
        current,
        Some(camera),
        shifted_camera,
    ));
    assert!(!alignment_scene_frame_is_current(
        None,
        current,
        Some(camera),
        camera,
    ));
}

#[test]
fn refusal_acceptance_requires_the_exact_request_camera_and_covering_horizon() {
    let current = AlignmentSceneRequestToken::new(12);
    let stale = AlignmentSceneRequestToken::new(11);
    let interval = GenomicInterval::new(ContigId::new(6), 100, 200).unwrap();
    let shifted = GenomicInterval::new(ContigId::new(6), 150, 250).unwrap();
    let camera = AlignmentSceneCamera::from(
        AlignmentCamera::new(
            interval,
            0.0,
            7.0,
            5.0,
            CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 500.0,
            },
        )
        .unwrap(),
    );
    let shifted_camera = AlignmentSceneCamera::from(
        AlignmentCamera::new(
            shifted,
            0.0,
            7.0,
            5.0,
            CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 500.0,
            },
        )
        .unwrap(),
    );
    let horizon = GenomicInterval::new(ContigId::new(6), 50, 250).unwrap();
    let short = GenomicInterval::new(ContigId::new(6), 125, 175).unwrap();

    assert!(alignment_scene_refusal_is_current(
        Some(current),
        current,
        Some(camera),
        camera,
        horizon,
    ));
    assert!(!alignment_scene_refusal_is_current(
        Some(current),
        stale,
        Some(camera),
        camera,
        horizon,
    ));
    assert!(!alignment_scene_refusal_is_current(
        Some(current),
        current,
        Some(camera),
        shifted_camera,
        horizon,
    ));
    assert!(!alignment_scene_refusal_is_current(
        Some(current),
        current,
        Some(camera),
        camera,
        short,
    ));
}

#[test]
fn resource_pressure_retry_requires_the_current_token_and_a_target_covering_the_live_camera() {
    let current = AlignmentSceneRequestToken::new(12);
    let stale = AlignmentSceneRequestToken::new(11);
    let interval = GenomicInterval::new(ContigId::new(6), 100, 200).unwrap();
    let camera = AlignmentSceneCamera::from(
        AlignmentCamera::new(
            interval,
            0.0,
            7.0,
            5.0,
            CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 500.0,
            },
        )
        .unwrap(),
    );
    let horizon = GenomicInterval::new(ContigId::new(6), 50, 250).unwrap();
    let short = GenomicInterval::new(ContigId::new(6), 125, 175).unwrap();

    assert!(alignment_scene_pressure_is_current(
        Some(current),
        current,
        Some(camera),
        horizon,
    ));
    assert!(!alignment_scene_pressure_is_current(
        Some(current),
        stale,
        Some(camera),
        horizon,
    ));
    assert!(!alignment_scene_pressure_is_current(
        Some(current),
        current,
        Some(camera),
        short,
    ));
    assert!(!alignment_scene_pressure_is_current(
        Some(current),
        current,
        None,
        horizon,
    ));
}

#[test]
fn vertical_presentation_pressure_is_resumed_even_though_its_work_is_speculative() {
    assert!(alignment_scene_pressure_requires_resume(
        AlignmentSceneEngineOperation::PumpPresentation,
        SourceIntentKind::Speculative,
    ));
    assert!(!alignment_scene_pressure_requires_resume(
        AlignmentSceneEngineOperation::PumpSource,
        SourceIntentKind::Speculative,
    ));
    assert!(!alignment_scene_pressure_requires_resume(
        AlignmentSceneEngineOperation::PrewarmPresentation,
        SourceIntentKind::Speculative,
    ));
    assert!(alignment_scene_pressure_requires_resume(
        AlignmentSceneEngineOperation::PumpSource,
        SourceIntentKind::Foreground,
    ));
}

#[test]
fn only_infeasible_speculative_presentation_pressure_preserves_the_visible_epoch() {
    assert!(alignment_scene_pressure_preserves_visible_epoch(
        AlignmentSceneEngineOperation::PumpPresentation,
        SourceIntentKind::Speculative,
        AlignmentSceneResourcePressureDisposition::Infeasible,
    ));
    assert!(!alignment_scene_pressure_preserves_visible_epoch(
        AlignmentSceneEngineOperation::PumpPresentation,
        SourceIntentKind::Speculative,
        AlignmentSceneResourcePressureDisposition::Retryable,
    ));
    assert!(!alignment_scene_pressure_preserves_visible_epoch(
        AlignmentSceneEngineOperation::PumpSource,
        SourceIntentKind::Speculative,
        AlignmentSceneResourcePressureDisposition::Infeasible,
    ));
    assert!(!alignment_scene_pressure_preserves_visible_epoch(
        AlignmentSceneEngineOperation::PumpPresentation,
        SourceIntentKind::Foreground,
        AlignmentSceneResourcePressureDisposition::Infeasible,
    ));
}

#[test]
fn budget_pressure_reducer_accepts_only_the_latest_workspace_and_source_value() {
    assert!(alignment_scene_budget_event_is_current(2_048, 2_048, 2_048));
    assert!(!alignment_scene_budget_event_is_current(
        4_096, 2_048, 2_048
    ));
    assert!(!alignment_scene_budget_event_is_current(
        4_096, 4_096, 2_048
    ));
}

#[test]
fn local_control_is_disabled_by_default_and_requires_a_secret_file() {
    let default = LaunchOptions::try_parse_from(["rgv-desktop"]).unwrap();
    assert!(default.control_port.is_none());
    assert!(LaunchOptions::try_parse_from(["rgv-desktop", "--control-port", "0"]).is_err());
    let enabled = LaunchOptions::try_parse_from([
        "rgv-desktop",
        "--control-port",
        "0",
        "--control-secret-file",
        "control.secret",
        "--control-root",
        "workspace",
    ])
    .unwrap();
    assert_eq!(enabled.control_port, Some(0));
}

#[test]
fn keyboard_evidence_navigation_wraps_both_directions_and_handles_empty_tracks() {
    assert_eq!(next_evidence_index(None, 0, true), None);
    assert_eq!(next_evidence_index(None, 3, true), Some(0));
    assert_eq!(next_evidence_index(Some(0), 3, true), Some(1));
    assert_eq!(next_evidence_index(Some(2), 3, true), Some(0));
    assert_eq!(next_evidence_index(None, 3, false), Some(2));
    assert_eq!(next_evidence_index(Some(0), 3, false), Some(2));
}

fn test_panel(id: u64) -> ViewPanelController {
    let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    ViewPanelController::new(
        PanelId::new(id),
        Viewport::new(interval, 1_000.0).unwrap(),
        None,
        "chr1".to_owned(),
        1_000_000,
    )
}

fn test_viewport(interval: GenomicInterval) -> Viewport {
    Viewport::new(interval, 1_000.0).unwrap()
}

#[test]
fn auxiliary_removal_clears_every_track_owned_panel_reference() {
    let removed_id = TrackId::new(5);
    let surviving_id = TrackId::new(6);
    let mut panel = test_panel(1);
    panel.track_snapshots = vec![
        TrackSnapshot {
            id: removed_id,
            name: "removed".to_owned(),
            data: TrackSnapshotData::Features(Vec::new()),
        },
        TrackSnapshot {
            id: surviving_id,
            name: "surviving".to_owned(),
            data: TrackSnapshotData::Features(Vec::new()),
        },
    ];
    for id in [removed_id, surviving_id] {
        panel
            .track_snapshot_intervals
            .insert(id, panel.viewport.interval());
        panel
            .track_snapshot_generations
            .insert(id, panel.generation_clock.current().get());
    }
    for id in [removed_id, surviving_id] {
        panel.track_frames.push(TrackFrame {
            key: TrackFrameKey::Auxiliary(id),
            channel: ui::TrackPane::Annotation,
            name: format!("track-{id:?}"),
            kind: TrackKind::Feature,
            source_label: None,
            top: 0.0,
            bottom: 40.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        });
    }
    panel.selected_feature = Some(FeatureSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.selected_variant = Some(VariantSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.selected_gwas = Some(GwasSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.selected_quantitative = Some(QuantitativeSelection {
        track_id: removed_id,
        record_index: 0,
        position: 100,
    });
    panel.selected_copy_number = Some(CopyNumberSelection {
        track_id: removed_id,
        segment_index: 0,
    });
    panel.selected_mutation = Some(MutationSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.selected_interaction = Some(InteractionSelection {
        track_id: removed_id,
        record_index: 0,
    });
    panel.hover_inspector_target = Some(HoverInspectorTarget::Pileup(removed_id, 100));
    panel.selected_track = Some(TrackFrameKey::Auxiliary(removed_id));
    panel.track_settled_generation = Some(panel.generation_clock.current().get());

    clear_removed_auxiliary_panel_state(&mut panel, &HashSet::from([removed_id]));

    assert_eq!(
        panel
            .track_snapshots
            .iter()
            .map(|snapshot| snapshot.id)
            .collect::<Vec<_>>(),
        [surviving_id]
    );
    assert_eq!(
        panel
            .track_snapshot_intervals
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [surviving_id]
    );
    assert_eq!(
        panel
            .track_snapshot_generations
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        [surviving_id]
    );
    assert_eq!(
        panel
            .track_frames
            .iter()
            .map(|frame| frame.key)
            .collect::<Vec<_>>(),
        [TrackFrameKey::Auxiliary(surviving_id)]
    );
    assert!(panel.selected_feature.is_none());
    assert!(panel.selected_variant.is_none());
    assert!(panel.selected_gwas.is_none());
    assert!(panel.selected_quantitative.is_none());
    assert!(panel.selected_copy_number.is_none());
    assert!(panel.selected_mutation.is_none());
    assert!(panel.selected_interaction.is_none());
    assert!(panel.hover_inspector_target.is_none());
    assert!(panel.selected_track.is_none());
    assert!(panel.track_settled_generation.is_none());
}

#[test]
fn auxiliary_removal_disables_arithmetic_that_depended_on_the_track() {
    let track = |id| {
        TrackConfig::new(
            TrackId::new(id),
            format!("signal-{id}"),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: format!("signal-{id}.bedGraph"),
            },
        )
    };
    let mut tracks = vec![track(5), track(6), track(7)];
    tracks[1].quantitative.arithmetic = QuantitativeArithmetic::Add;
    tracks[1].quantitative.operand_track = Some(TrackId::new(5));
    tracks[2].quantitative.arithmetic = QuantitativeArithmetic::Subtract;
    tracks[2].quantitative.operand_track = Some(TrackId::new(6));

    assert_eq!(
        clear_removed_quantitative_dependencies(&mut tracks, &HashSet::from([TrackId::new(5)]),),
        1
    );
    assert_eq!(
        tracks[1].quantitative.arithmetic,
        QuantitativeArithmetic::None
    );
    assert_eq!(tracks[1].quantitative.operand_track, None);
    assert_eq!(
        tracks[2].quantitative.arithmetic,
        QuantitativeArithmetic::Subtract
    );
    assert_eq!(tracks[2].quantitative.operand_track, Some(TrackId::new(6)));
}

#[test]
fn auxiliary_queries_follow_source_lifecycle_not_presentation_eyes() {
    let quantitative = |id| {
        TrackConfig::new(
            TrackId::new(id),
            format!("signal-{id}"),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: format!("signal-{id}.bedGraph"),
            },
        )
    };
    let mut tracks = vec![
        quantitative(2),
        quantitative(1),
        TrackConfig::new(
            TrackId::new(3),
            "hidden features".to_owned(),
            TrackKind::Feature,
            TrackSource::Local {
                path: "hidden.bed".to_owned(),
            },
        ),
        TrackConfig::new(
            TrackId::new(4),
            "visible variants".to_owned(),
            TrackKind::Variant,
            TrackSource::Local {
                path: "visible.vcf".to_owned(),
            },
        ),
    ];
    tracks[0].visible = false;
    tracks[1].quantitative.arithmetic = QuantitativeArithmetic::Add;
    tracks[1].quantitative.operand_track = Some(TrackId::new(2));
    tracks[2].visible = false;

    assert_eq!(
        auxiliary_query_track_ids(&tracks),
        [
            TrackId::new(2),
            TrackId::new(1),
            TrackId::new(3),
            TrackId::new(4)
        ]
    );

    tracks[1].quantitative.arithmetic = QuantitativeArithmetic::None;
    assert_eq!(
        auxiliary_query_track_ids(&tracks),
        [
            TrackId::new(2),
            TrackId::new(1),
            TrackId::new(3),
            TrackId::new(4)
        ]
    );

    tracks[1].visible = false;
    assert_eq!(
        auxiliary_query_track_ids(&tracks),
        [
            TrackId::new(2),
            TrackId::new(1),
            TrackId::new(3),
            TrackId::new(4)
        ]
    );
}

#[test]
fn alignment_removal_clears_deleted_identity_from_every_panel() {
    let removed = TrackId::new(5);
    let surviving = TrackId::new(6);
    let mut selected = test_panel(1);
    selected.selected_alignment_track = Some(removed);
    selected.selected_alignment_tracks = vec![surviving, removed];
    selected.selected_read_tracks = vec![surviving, removed];
    selected.read_selection_anchor = Some(removed);
    selected.selected_pileup_position = Some(120);
    selected.selected_modification_position = Some(121);
    selected.selected_junction = Some((130, 150));
    selected.selected_chain = Some(SelectedSupplementaryChain {
        read_name: b"removed-read".to_vec(),
        segments: Vec::new(),
        active_segment: 0,
        malformed_entries: 0,
        truncated: false,
        invalid_tag_type: false,
    });
    selected.pending_read_selection =
        PendingReadSelection::by_name(removed, b"pending-read".to_vec());
    selected.hover_inspector_target = Some(HoverInspectorTarget::Pileup(removed, 120));
    selected.selected_track = Some(TrackFrameKey::Alignment(removed));

    let mut unaffected = test_panel(2);
    unaffected.selected_alignment_track = Some(surviving);
    unaffected.selected_alignment_tracks = vec![removed, surviving];
    unaffected.selected_read_tracks = vec![removed, surviving];
    unaffected.read_selection_anchor = Some(surviving);
    unaffected.selected_track = Some(TrackFrameKey::Alignment(surviving));

    for panel in [&mut selected, &mut unaffected] {
        for id in [removed, surviving] {
            panel.track_frames.push(TrackFrame {
                key: TrackFrameKey::Alignment(id),
                channel: ui::TrackPane::Annotation,
                name: format!("reads-{id:?}"),
                kind: TrackKind::Alignment,
                source_label: None,
                top: 0.0,
                bottom: 80.0,
                visible: true,
                collapsed: false,
                frozen_bottom: None,
                subtracks: Vec::new(),
            });
        }
        clear_removed_alignment_panel_state(panel, removed, true);
    }

    assert_eq!(selected.selected_alignment_track, None);
    assert_eq!(selected.selected_alignment_tracks, [surviving]);
    assert_eq!(selected.selected_read_tracks, [surviving]);
    assert_eq!(selected.read_selection_anchor, Some(surviving));
    assert!(selected.selected_pileup_position.is_none());
    assert!(selected.selected_modification_position.is_none());
    assert!(selected.selected_junction.is_none());
    assert!(selected.selected_chain.is_none());
    assert!(selected.pending_read_selection.is_none());
    assert!(selected.hover_inspector_target.is_none());
    assert_eq!(
        selected.selected_track,
        Some(TrackFrameKey::Alignment(surviving))
    );
    assert_eq!(
        selected
            .track_frames
            .iter()
            .map(|frame| frame.key)
            .collect::<Vec<_>>(),
        [TrackFrameKey::Alignment(surviving)]
    );

    assert_eq!(unaffected.selected_alignment_track, Some(surviving));
    assert_eq!(unaffected.selected_alignment_tracks, [surviving]);
    assert_eq!(unaffected.selected_read_tracks, [surviving]);
    assert_eq!(unaffected.read_selection_anchor, Some(surviving));
    assert_eq!(
        unaffected.selected_track,
        Some(TrackFrameKey::Alignment(surviving))
    );
    assert_eq!(
        unaffected
            .track_frames
            .iter()
            .map(|frame| frame.key)
            .collect::<Vec<_>>(),
        [TrackFrameKey::Alignment(surviving)]
    );
}

#[test]
fn alignment_projections_keep_each_panel_scene_transaction_independent() {
    let first_id = PanelId::new(1);
    let second_id = PanelId::new(2);
    let mut projections = alignment_panel_projections(&[first_id, second_id]);
    let first = projections.get_mut(&first_id).unwrap();
    first.navigation.bind_identity(first_id, TrackId::new(10));
    first.navigation.begin(
        first.clock.current().get(),
        AlignmentSceneRequestToken::new(100),
        "first panel",
    );
    let second = projections.get_mut(&second_id).unwrap();
    second.navigation.bind_identity(second_id, TrackId::new(10));
    second.navigation.begin(
        second.clock.current().get(),
        AlignmentSceneRequestToken::new(200),
        "second panel",
    );

    assert_eq!(
        projections[&first_id].navigation.request_token(),
        Some(AlignmentSceneRequestToken::new(100))
    );
    assert_eq!(
        projections[&second_id].navigation.request_token(),
        Some(AlignmentSceneRequestToken::new(200))
    );
}

#[test]
fn panel_accepts_only_its_current_generation() {
    let first = test_panel(1);
    let second = test_panel(2);
    let current = first.request();

    assert!(first.accepts(current));
    assert!(!second.accepts(current));
    first.generation_clock.advance();
    assert!(!first.accepts(current));
    assert!(first.accepts(first.request()));
}

#[test]
fn review_navigation_follows_queue_order_wraps_and_honours_filter() {
    let item = |id, contig: &str, start, status| ReviewItem {
        id,
        label: format!("item-{id}"),
        locus: SessionLocus {
            contig: contig.to_owned(),
            start,
            end: start + 10,
        },
        status,
        notes: String::new(),
        assignment: None,
    };
    let items = vec![
        item(1, "chr1", 100, ReviewStatus::Pending),
        item(2, "chr2", 200, ReviewStatus::Complete),
        item(3, "chr3", 300, ReviewStatus::Pending),
    ];

    assert_eq!(
        adjacent_review_locus(&items, None, "chr1", 100, 110, true)
            .unwrap()
            .contig,
        "chr2"
    );
    assert_eq!(
        adjacent_review_locus(&items, None, "chr1", 100, 110, false)
            .unwrap()
            .contig,
        "chr3"
    );
    assert_eq!(
        adjacent_review_locus(&items, Some(ReviewStatus::Pending), "chr3", 300, 310, true,)
            .unwrap()
            .contig,
        "chr1"
    );
    assert!(
        adjacent_review_locus(&items, Some(ReviewStatus::Skipped), "chr1", 100, 110, true,)
            .is_none()
    );
}

#[test]
fn imported_regions_receive_unique_ids_and_respect_workspace_capacity() {
    let region = |id, start| RegionOfInterest {
        id,
        name: format!("region-{start}"),
        locus: SessionLocus {
            contig: "chr1".to_owned(),
            start,
            end: start + 10,
        },
        color: [1, 2, 3, 48],
    };
    let mut destination = vec![region(1, 0), region(3, 20)];
    let (added, omitted) =
        merge_imported_regions(&mut destination, vec![region(1, 40), region(2, 60)], 3);

    assert_eq!((added, omitted), (1, 1));
    assert_eq!(
        destination.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![1, 3, 2]
    );
}

#[test]
fn metadata_attribute_selection_spans_alignment_and_auxiliary_tracks() {
    let metadata = SampleMetadataTable::from_tsv(concat!(
        "sample\tcondition\n",
        "normal\tcontrol\n",
        "tumour\tcase\n",
    ))
    .unwrap();
    let tracks = vec![
        (
            TrackId::new(1),
            "reads".to_owned(),
            Some("tumour".to_owned()),
            TrackKind::Alignment,
        ),
        (
            TrackId::new(2),
            "signal".to_owned(),
            Some("tumour".to_owned()),
            TrackKind::Quantitative,
        ),
        (
            TrackId::new(3),
            "control".to_owned(),
            Some("normal".to_owned()),
            TrackKind::Variant,
        ),
        (
            TrackId::new(4),
            "unassigned".to_owned(),
            None,
            TrackKind::Feature,
        ),
    ];

    assert_eq!(
        metadata_matching_track_ids(&tracks, &metadata, 0, "case"),
        BTreeSet::from([TrackId::new(1), TrackId::new(2)])
    );
}

#[test]
fn quantitative_bulk_overlay_is_compatible_bounded_and_reversible() {
    let quantitative = |id| {
        TrackConfig::new(
            TrackId::new(id),
            format!("signal-{id}"),
            TrackKind::Quantitative,
            TrackSource::Local {
                path: format!("signal-{id}.bw"),
            },
        )
    };
    let mut tracks = vec![
        quantitative(1),
        quantitative(2),
        TrackConfig::new(
            TrackId::new(3),
            "variants".to_owned(),
            TrackKind::Variant,
            TrackSource::Local {
                path: "variants.bcf".to_owned(),
            },
        ),
    ];
    let selected = BTreeSet::from([TrackId::new(1), TrackId::new(2), TrackId::new(3)]);

    assert!(apply_quantitative_bulk_action(
        &mut tracks,
        &selected,
        QuantitativeBulkAction::Overlay,
    ));
    assert_eq!(tracks[0].quantitative.overlay_group, Some(1));
    assert_eq!(tracks[1].quantitative.overlay_group, Some(1));
    assert_eq!(tracks[2].quantitative.overlay_group, None);
    tracks[0].quantitative.positive_color = [1, 2, 3, 4];
    tracks[1].quantitative.arithmetic = QuantitativeArithmetic::Subtract;
    tracks[1].quantitative.operand_track = Some(TrackId::new(1));
    let quantitative_selected = BTreeSet::from([TrackId::new(1), TrackId::new(2)]);
    assert!(apply_compatible_auxiliary_appearance(
        &mut tracks,
        &quantitative_selected,
    ));
    assert_eq!(tracks[1].quantitative.positive_color, [1, 2, 3, 4]);
    assert_eq!(
        tracks[1].quantitative.arithmetic,
        QuantitativeArithmetic::Subtract
    );
    assert_eq!(tracks[1].quantitative.operand_track, Some(TrackId::new(1)));
    assert_eq!(tracks[1].quantitative.overlay_group, Some(1));
    assert!(apply_quantitative_bulk_action(
        &mut tracks,
        &selected,
        QuantitativeBulkAction::Separate,
    ));
    assert_eq!(tracks[0].quantitative.overlay_group, None);
    assert_eq!(
        tracks[0].quantitative.scale_mode,
        QuantitativeScaleMode::Automatic
    );
}

#[test]
fn region_overlay_clips_half_open_intervals_to_each_panel_viewport() {
    let viewport = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
    let region = RegionOfInterest {
        id: 1,
        name: "roi".to_owned(),
        locus: SessionLocus {
            contig: "chr1".to_owned(),
            start: 50,
            end: 120,
        },
        color: [1, 2, 3, 48],
    };
    assert_eq!(
        region_overlay_horizontal_span(&region, "chr1", viewport, 1.0, 40.0),
        Some((40.0, 60.0))
    );
    assert_eq!(
        region_overlay_horizontal_span(&region, "chr2", viewport, 1.0, 40.0),
        None
    );
    let touching = RegionOfInterest {
        locus: SessionLocus {
            contig: "chr1".to_owned(),
            start: 200,
            end: 210,
        },
        ..region
    };
    assert_eq!(
        region_overlay_horizontal_span(&touching, "chr1", viewport, 1.0, 40.0),
        None
    );
}

#[test]
fn panel_workspace_mutations_preserve_stable_identity_and_active_panel() {
    let mut workspace = PanelWorkspace::new(test_panel(7));
    assert!(workspace.push(test_panel(2)));
    assert!(workspace.push(test_panel(11)));
    assert!(!workspace.push(test_panel(2)));
    assert_eq!(workspace.next_id(), PanelId::new(12));

    assert!(workspace.activate(PanelId::new(2)));
    assert!(workspace.move_panel(PanelId::new(11), 0));
    assert_eq!(
        workspace.iter().map(|panel| panel.id).collect::<Vec<_>>(),
        vec![PanelId::new(11), PanelId::new(7), PanelId::new(2)]
    );
    assert_eq!(workspace.active().id, PanelId::new(2));

    assert!(workspace.remove(PanelId::new(2)).is_some());
    assert_eq!(workspace.active().id, PanelId::new(7));
    assert!(workspace.remove(PanelId::new(11)).is_some());
    assert!(workspace.remove(PanelId::new(7)).is_none());
}

#[test]
fn session_panel_workspace_restores_order_active_locus_and_width() {
    let active_locus = SessionLocus {
        contig: "chr2".to_owned(),
        start: 500,
        end: 800,
    };
    let mut session = SessionDocument::new(active_locus.clone());
    session.panels = vec![
        SessionViewPanel::new(
            PanelId::new(9),
            SessionLocus {
                contig: "chr1".to_owned(),
                start: 100,
                end: 200,
            },
        ),
        SessionViewPanel::new(PanelId::new(4), active_locus),
    ];
    session.panels[0].vertical_scroll_points = 73.0;
    session.panels[1].vertical_scroll_points = 145.0;
    session.panels[0].annotation_scroll_points = 19.0;
    session.panels[1].annotation_scroll_points = 81.0;
    session.panels[0].vertical_content_origin_points = 33.0;
    session.panels[1].vertical_content_origin_points = 105.0;
    session.panels[1].read_panel_states = vec![SessionReadPanelState {
        track_id: TrackId::new(12),
        scroll_points: 210.0,
        content_origin_points: 40.0,
        latent_scroll_fraction: Some(0.6),
        content_bottom_points: 900.0,
        density: Some(3.0),
    }];
    session.active_panel = PanelId::new(4);

    let workspace = PanelWorkspace::from_session(&session, 400.0, 120.0).unwrap();

    assert_eq!(
        workspace.iter().map(|panel| panel.id).collect::<Vec<_>>(),
        vec![PanelId::new(9), PanelId::new(4)]
    );
    assert_eq!(workspace.active().id, PanelId::new(4));
    assert!((workspace.panels[0].vertical_scroll_points - 73.0).abs() < f32::EPSILON);
    assert!((workspace.panels[1].vertical_scroll_points - 145.0).abs() < f32::EPSILON);
    assert!((workspace.annotation_scroll_points - 81.0).abs() < f32::EPSILON);
    assert!((workspace.panels[0].vertical_content_origin_points - 33.0).abs() < f32::EPSILON);
    assert!((workspace.panels[1].vertical_content_origin_points - 105.0).abs() < f32::EPSILON);
    assert!((workspace.panels[1].effective_vertical_scroll_points() - 40.0).abs() < f32::EPSILON);
    assert_eq!(
        workspace.panels[1]
            .read_panel_scrolls
            .get(&TrackId::new(12)),
        Some(&ReadPanelScrollState {
            scroll_points: 210.0,
            content_origin_points: 40.0,
            latent_scroll_fraction: Some(0.6),
            content_bottom_points: 900.0,
            density: Some(3.0),
            stable_row_anchor: None,
            scene_contig: None,
        })
    );
    assert_eq!(workspace.active().contig_name, "chr2");
    assert_eq!(workspace.active().viewport.interval().start(), 500);
    assert_eq!(workspace.active().viewport.interval().end(), 800);
    assert!((workspace.active().viewport.pixel_width() - 400.0).abs() < f64::EPSILON);
    assert!(
        workspace
            .iter()
            .all(|panel| (panel.content_bottom_points - 120.0).abs() < f32::EPSILON)
    );
}

#[test]
fn every_alignment_controller_projection_is_created_for_every_panel() {
    let ids = [PanelId::new(9), PanelId::new(4), PanelId::new(20)];
    let projections = alignment_panel_projections(&ids);

    assert_eq!(projections.len(), ids.len());
    assert!(ids.iter().all(|id| projections.contains_key(id)));
    assert!(!projections.contains_key(&INITIAL_PANEL_ID));
}

#[test]
fn composite_display_keeps_gpu_identity_only_for_the_active_panel() {
    let mut first = test_panel(7);
    first.display_list = DisplayList::synthetic_reads(100.0, 1, 1);
    first.display_list.rectangles[0].object_id = 41;
    let mut second = test_panel(2);
    second.display_list = DisplayList::synthetic_reads(100.0, 1, 1);
    second.display_list.rectangles[0].object_id = 41;
    let mut workspace = PanelWorkspace::new(first);
    assert!(workspace.push(second));

    let first_active = composite_panel_display_lists(&workspace);
    assert_eq!(
        first_active
            .rectangles
            .iter()
            .map(|rectangle| rectangle.object_id)
            .collect::<Vec<_>>(),
        vec![41, 0]
    );

    assert!(workspace.activate(PanelId::new(2)));
    let second_active = composite_panel_display_lists(&workspace);
    assert_eq!(
        second_active
            .rectangles
            .iter()
            .map(|rectangle| rectangle.object_id)
            .collect::<Vec<_>>(),
        vec![0, 41]
    );
}

#[test]
fn generic_gpu_composite_excludes_scene_owned_contextual_rectangles() {
    let mut panel = test_panel(7);
    panel.display_list = DisplayList::synthetic_reads(100.0, 1, 1);
    panel.display_list.rectangles[0].object_id = 41;
    panel.generic_display_list = DisplayList::synthetic_reads(100.0, 1, 1);
    panel.generic_display_list.rectangles[0].object_id = 7;
    let workspace = PanelWorkspace::new(panel);

    let semantic = composite_panel_display_lists(&workspace);
    let generic = composite_panel_generic_display_lists(&workspace);
    assert_eq!(semantic.rectangles[0].object_id, 41);
    assert_eq!(generic.rectangles[0].object_id, 7);
    assert!(
        generic
            .rectangles
            .iter()
            .all(|rectangle| rectangle.object_id != 41)
    );
}

#[test]
fn multi_panel_composite_export_matches_reviewed_golden_hash() {
    let mut first = test_panel(7);
    first.display_list = DisplayList::synthetic_coverage_and_reads_at(200.0, 12, 80, 8.0);
    let mut second = test_panel(2);
    second.display_list = DisplayList::synthetic_coverage_and_reads_at(200.0, 18, 140, 8.0);
    second
        .display_list
        .translate_and_clip_horizontal(200.0, 200.0, 400.0);
    let mut workspace = PanelWorkspace::new(first);
    assert!(workspace.push(second));
    let composite = composite_panel_display_lists(&workspace);
    let directory = tempfile::tempdir().unwrap();
    let first_path = directory.path().join("multi-panel-first.png");
    let second_path = directory.path().join("multi-panel-second.png");
    export_png(&composite, 400, 200, &first_path).unwrap();
    export_png(&composite, 400, 200, &second_path).unwrap();
    let first_bytes = std::fs::read(&first_path).unwrap();
    assert_eq!(first_bytes, std::fs::read(&second_path).unwrap());
    if let Some(review_path) = std::env::var_os("RGV_MULTI_PANEL_VISUAL_REVIEW_PATH") {
        std::fs::copy(&first_path, review_path).unwrap();
    }
    let hash = first_bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    assert_eq!(
        hash, 561_290_389_687_490_317,
        "update only after reviewing an intentional multi-panel raster change"
    );
}

#[test]
fn multi_panel_annotation_geometry_uses_the_tallest_frame_for_every_row() {
    let alignment_id = TrackId::new(5);
    let auxiliary_id = TrackId::new(6);
    let add_layout = |panel: &mut ViewPanelController| {
        for frame in panel.track_frames.clone() {
            let track_id = match frame.key {
                TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => track_id,
                TrackFrameKey::Reference | TrackFrameKey::Group { .. } => unreachable!(),
            };
            let row_rect = egui::Rect::from_min_max(
                egui::pos2(0.0, frame.top),
                egui::pos2(100.0, frame.bottom),
            );
            panel.pane_layout.annotation.items.push(ui::PaneItemLayout {
                id: PresentationNodeId::Track {
                    channel: ui::TrackPane::Annotation,
                    track_id,
                },
                frame,
                row_rect,
                header_rect: egui::Rect::from_min_max(
                    row_rect.min,
                    egui::pos2(row_rect.right(), row_rect.top() + 22.0),
                ),
                content_rect: Some(egui::Rect::from_min_max(
                    egui::pos2(row_rect.left(), row_rect.top() + 22.0),
                    row_rect.max,
                )),
                clip_rect: row_rect,
                desired_content_visible: true,
                effective_content_visible: true,
            });
        }
    };
    let mut first = test_panel(7);
    first.track_frames.push(TrackFrame {
        key: TrackFrameKey::Alignment(alignment_id),
        channel: ui::TrackPane::Annotation,
        name: "Reads".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 100.0,
        bottom: 220.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    });
    first.track_frames.push(TrackFrame {
        key: TrackFrameKey::Auxiliary(auxiliary_id),
        channel: ui::TrackPane::Annotation,
        name: "Genes".to_owned(),
        kind: TrackKind::GeneModel,
        source_label: None,
        top: 220.0,
        bottom: 520.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    });
    add_layout(&mut first);
    let mut second = test_panel(2);
    second.track_frames.push(TrackFrame {
        key: TrackFrameKey::Alignment(alignment_id),
        channel: ui::TrackPane::Annotation,
        name: "Reads".to_owned(),
        kind: TrackKind::Alignment,
        source_label: None,
        top: 100.0,
        bottom: 460.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    });
    second.track_frames.push(TrackFrame {
        key: TrackFrameKey::Auxiliary(auxiliary_id),
        channel: ui::TrackPane::Annotation,
        name: "Genes".to_owned(),
        kind: TrackKind::GeneModel,
        source_label: None,
        top: 460.0,
        bottom: 540.0,
        visible: true,
        collapsed: false,
        frozen_bottom: None,
        subtracks: Vec::new(),
    });
    add_layout(&mut second);
    let mut workspace = PanelWorkspace::new(first);
    assert!(workspace.push(second));

    let shared = maximum_annotation_frame_heights(&workspace);
    assert_eq!(
        shared.get(&PresentationNodeId::Track {
            channel: ui::TrackPane::Annotation,
            track_id: alignment_id,
        }),
        Some(&360.0)
    );
    assert_eq!(
        shared.get(&PresentationNodeId::Track {
            channel: ui::TrackPane::Annotation,
            track_id: auxiliary_id,
        }),
        Some(&300.0)
    );
}

#[test]
fn workspace_annotation_camera_uses_the_range_available_to_every_panel() {
    let cache = |content_bottom, clip_bottom| PanelScrollDisplayCache {
        annotations: DisplayList::default(),
        pinned: DisplayList::default(),
        annotation_overview: ui::ScrollbarOverview::default(),
        annotation_content_bottom: content_bottom,
        annotation_clip_top: 0.0,
        annotation_clip_bottom: clip_bottom,
        supports_fast_scroll: true,
        read_panels: Vec::new(),
    };
    let mut first = test_panel(7);
    first.scroll_display_cache = Some(cache(500.0, 300.0));
    let mut second = test_panel(2);
    second.scroll_display_cache = Some(cache(420.0, 300.0));
    let mut workspace = PanelWorkspace::new(first);
    assert!(workspace.push(second));

    assert_eq!(workspace.maximum_annotation_scroll_points(), 120.0);
    workspace.annotation_scroll_points = 95.0;
    assert_eq!(workspace.annotation_scroll_points, 95.0);
}

#[test]
fn multi_locus_expression_is_ordered_bounded_and_atomic_to_parse() {
    assert_eq!(
        split_locus_expression("chr2:20-40  chr1:1-10\nchrX:5-8").unwrap(),
        vec!["chr2:20-40", "chr1:1-10", "chrX:5-8"]
    );
    assert!(split_locus_expression("   ").is_err());
    let excessive = (0..=MAXIMUM_VIEW_PANELS)
        .map(|index| format!("chr1:{}-{}", index + 1, index + 2))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(split_locus_expression(&excessive).is_err());
}

#[test]
fn mate_panel_locus_is_centered_bounded_and_requires_a_mapped_pair() {
    let dictionary =
        ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000), ("chr2".to_owned(), 2_000)]);
    let mut record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 100,
        end: 150,
        flags: rgv_hts::AlignmentFlags::from_bits(0x1),
        mapping_quality: Some(60),
        mate_contig: Some(ContigId::new(1)),
        mate_start: Some(1_900),
        template_length: 0,
        read_name: b"pair".to_vec(),
        cigar: Vec::new(),
        sequence: SequenceData::Missing,
        auxiliary_fields: Vec::new(),
    };

    assert_eq!(
        mate_locus(&record, &dictionary, 400).as_deref(),
        Some("chr2:1601-2000")
    );
    record.flags = rgv_hts::AlignmentFlags::from_bits(0x9);
    assert!(mate_locus(&record, &dictionary, 400).is_none());
    record.flags = rgv_hts::AlignmentFlags::from_bits(0x1);
    record.mate_start = Some(2_000);
    assert!(mate_locus(&record, &dictionary, 400).is_none());
}

#[test]
fn locus_parser_converts_one_based_inclusive_input() {
    let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000_000)]);
    let (interval, name, length) =
        resolve_initial_locus(&dictionary, &ContigAliases::default(), Some("chr1:101-200"))
            .unwrap();
    assert_eq!(name, "chr1");
    assert_eq!(length, 1_000_000);
    assert_eq!(interval.start(), 100);
    assert_eq!(interval.end(), 200);
}

#[test]
fn locus_parser_rejects_unknown_contigs() {
    let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 100)]);
    assert!(
        resolve_initial_locus(&dictionary, &ContigAliases::default(), Some("chr2:1-10")).is_err()
    );
}

#[test]
fn locus_parser_uses_only_explicit_unambiguous_contig_aliases() {
    let dictionary =
        ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000), ("1".to_owned(), 1_000)]);
    let aliases = parse_contig_aliases(&["one=chr1".to_owned()]).unwrap();
    let (interval, canonical, _) =
        resolve_initial_locus(&dictionary, &aliases, Some("one:11-20")).unwrap();
    assert_eq!(canonical, "chr1");
    assert_eq!((interval.start(), interval.end()), (10, 20));

    let conflicting = parse_contig_aliases(&["1=chr1".to_owned()]).unwrap();
    assert!(resolve_initial_locus(&dictionary, &conflicting, Some("1:1-10")).is_err());
    assert!(parse_contig_aliases(&["one=chr1".to_owned(), "one=1".to_owned()]).is_err());
}

#[test]
fn annotation_search_requires_a_unique_exact_match_before_navigation() {
    let hit = |track_id, matched_term: &str, name: &str, start| TrackSearchHit {
        track_id: TrackId::new(track_id),
        track_name: "Genes".to_owned(),
        matched_term: matched_term.to_owned(),
        record: rgv_hts::FeatureRecord {
            contig: "chr1".to_owned(),
            start,
            end: start + 100,
            name: Some(name.to_owned()),
            feature_type: Some("gene".to_owned()),
            score: None,
            strand: rgv_hts::Strand::Unstranded,
            blocks: Vec::new(),
            coding_interval: None,
            peak: None,
            attributes: Vec::new(),
        },
    };
    let exact = hit(1, "brca1", "BRCA1", 100);
    let prefix = hit(1, "brca2", "BRCA2", 300);
    let results = TrackSearchResults {
        query: "brca1".to_owned(),
        hits: vec![exact.clone(), prefix],
        ..TrackSearchResults::default()
    };
    assert_eq!(
        preferred_annotation_search_hit(&results, "BRCA1"),
        Some(&exact)
    );
    assert_eq!(annotation_search_hit_primary_label(&exact), "BRCA1");
    assert_eq!(annotation_search_hit_locus(&exact), "chr1:101-200");

    let ambiguous = TrackSearchResults {
        query: "brca1".to_owned(),
        hits: vec![exact.clone(), hit(2, "brca1", "BRCA1", 500)],
        ..TrackSearchResults::default()
    };
    assert_eq!(preferred_annotation_search_hit(&ambiguous, "brca1"), None);
}

#[test]
fn annotation_search_presents_symbol_before_stable_identifiers() {
    let hit = TrackSearchHit {
        track_id: TrackId::new(1),
        track_name: "Genes".to_owned(),
        matched_term: "ensg000001".to_owned(),
        record: rgv_hts::FeatureRecord {
            contig: "chr7".to_owned(),
            start: 100,
            end: 200,
            name: Some("ENST000001".to_owned()),
            feature_type: Some("transcript".to_owned()),
            score: None,
            strand: rgv_hts::Strand::Forward,
            blocks: vec![(100, 200)],
            coding_interval: None,
            peak: None,
            attributes: vec![
                ("gene_name".to_owned(), "ACTB".to_owned()),
                ("gene_id".to_owned(), "ENSG000001".to_owned()),
                ("transcript_id".to_owned(), "ENST000001".to_owned()),
            ],
        },
    };

    assert_eq!(annotation_search_hit_primary_label(&hit), "ACTB");
    assert_eq!(
        annotation_search_hit_secondary_identity(&hit),
        "ENSG000001 · ENST000001"
    );
}

#[test]
fn standalone_locus_supports_track_only_sessions() {
    let (interval, name, length) = standalone_locus("chr7:1,001-2,000").unwrap();
    assert_eq!(name, "chr7");
    assert_eq!((interval.start(), interval.end()), (1_000, 2_000));
    assert!(length > interval.end());
}

#[test]
fn primary_alignment_upsert_preserves_deferred_session_tracks() {
    let mut alignment_tracks = vec![
        SessionAlignmentTrack::new(
            TrackId::new(10),
            "Tumour".to_owned(),
            TrackSource::Local {
                path: "tumour.bam".to_owned(),
            },
        ),
        SessionAlignmentTrack::new(
            TrackId::new(11),
            "Normal".to_owned(),
            TrackSource::Local {
                path: "normal.bam".to_owned(),
            },
        ),
    ];
    alignment_tracks[1].filters.minimum_mapping_quality = Some(30);
    let deferred = alignment_tracks[1].clone();
    let source = TrackSource::Local {
        path: "replacement.bam".to_owned(),
    };

    let primary =
        upsert_primary_alignment_config(&mut alignment_tracks, &[], source.clone(), "New").clone();

    assert_eq!(primary.id, TrackId::new(10));
    assert_eq!(primary.name, "New");
    assert_eq!(primary.source, source);
    assert_eq!(alignment_tracks, vec![primary, deferred]);
}

#[test]
fn primary_alignment_upsert_allocates_an_id_after_auxiliary_tracks() {
    let mut alignment_tracks = Vec::new();
    let auxiliary = vec![TrackConfig::new(
        TrackId::new(7),
        "Genes".to_owned(),
        TrackKind::GeneModel,
        TrackSource::Local {
            path: "genes.gtf".to_owned(),
        },
    )];

    let primary = upsert_primary_alignment_config(
        &mut alignment_tracks,
        &auxiliary,
        TrackSource::Local {
            path: "reads.bam".to_owned(),
        },
        "Reads",
    );

    assert_eq!(primary.id, TrackId::new(8));
}

#[test]
fn explicit_complete_read_limit_can_raise_the_default_workspace_budget() {
    assert_eq!(
        expanded_workspace_alignment_limit(50_000, 100_000),
        (100_000, 100_000)
    );
    assert_eq!(
        expanded_workspace_alignment_limit(100_000, 25_000),
        (100_000, 25_000)
    );
    assert_eq!(
        expanded_workspace_alignment_limit(50_000, 10_000_001),
        (10_000_000, 10_000_000)
    );
}

#[test]
fn alignment_ordering_can_promote_or_demote_the_primary_track() {
    let mut order = vec![TrackId::new(1), TrackId::new(2), TrackId::new(3)];

    assert!(move_ordered_item(&mut order, 0, 2));
    assert_eq!(
        order,
        vec![TrackId::new(2), TrackId::new(3), TrackId::new(1)]
    );
    assert!(move_ordered_item(&mut order, 2, 0));
    assert_eq!(
        order,
        vec![TrackId::new(1), TrackId::new(2), TrackId::new(3)]
    );
    assert!(!move_ordered_item(&mut order, 0, 0));
}

#[test]
fn additive_alignment_selection_toggles_stable_track_ids() {
    let first = TrackId::new(1);
    let second = TrackId::new(2);

    let selected = updated_alignment_selection(&[], first, false);
    let selected = updated_alignment_selection(&selected, second, true);
    assert_eq!(selected, vec![first, second]);
    let selected = updated_alignment_selection(&selected, first, true);
    assert_eq!(selected, vec![second]);
    assert_eq!(
        updated_alignment_selection(&selected, first, false),
        vec![first]
    );
}

#[test]
fn shift_alignment_selection_includes_the_complete_ordered_range() {
    let order = (1..=6).map(TrackId::new).collect::<Vec<_>>();

    assert_eq!(
        alignment_range_selection(&order, TrackId::new(2), TrackId::new(5)),
        order[1..=4]
    );
    assert_eq!(
        alignment_range_selection(&order, TrackId::new(5), TrackId::new(2)),
        order[1..=4]
    );
    assert_eq!(
        alignment_range_selection(&order, TrackId::new(99), TrackId::new(3)),
        [TrackId::new(3)]
    );
}

#[test]
fn every_alignment_track_filter_is_parsed_independently() {
    let tumour = SessionFilters {
        minimum_mapping_quality: Some(30),
        auxiliary_predicates: vec!["RG=tumour".to_owned()],
        ..SessionFilters::default()
    };
    let normal = SessionFilters {
        minimum_mapping_quality: Some(10),
        auxiliary_predicates: vec!["HP=1".to_owned()],
        ..SessionFilters::default()
    };

    let tumour = active_filters_from_session(&tumour).unwrap();
    let normal = active_filters_from_session(&normal).unwrap();

    assert_eq!(tumour.alignment.minimum_mapping_quality, Some(30));
    assert_eq!(normal.alignment.minimum_mapping_quality, Some(10));
    assert_ne!(
        tumour.alignment.fingerprint(),
        normal.alignment.fingerprint()
    );

    let invalid = SessionFilters {
        auxiliary_predicates: vec!["not a predicate".to_owned()],
        ..SessionFilters::default()
    };
    assert!(active_filters_from_session(&invalid).is_err());
}

#[test]
fn full_settings_update_changes_only_the_target_alignment_config() {
    let source = TrackSource::Local {
        path: "normal.bam".to_owned(),
    };
    let mut normal = SessionAlignmentTrack::new(TrackId::new(2), "Normal".to_owned(), source);
    let untouched = SessionAlignmentTrack::new(
        TrackId::new(1),
        "Tumour".to_owned(),
        TrackSource::Local {
            path: "tumour.bam".to_owned(),
        },
    );
    let (mut editor, mut active) = filter_state_from_session(&normal.filters).unwrap();
    let mut filters = normal.filters.clone();
    filters.minimum_allele_fraction = 0.07;
    filters.alignment_color_mode = AlignmentColorMode::Haplotype;

    let outcome = commit_alignment_settings_draft(
        &mut normal,
        &mut editor,
        &mut active,
        AlignmentSettingsDraft {
            filters,
            filter_editor: FilterEditor {
                minimum_mapq: 30,
                include_secondary: false,
                include_supplementary: true,
                expression: "RG=normal;HP=1".to_owned(),
                error: None,
            },
            maximum_alignments: 80_000,
            apply_filter: true,
        },
        50_000,
    );

    assert_eq!(
        outcome,
        AlignmentSettingsOutcome {
            configuration_changed: true,
            query_changed: true,
        }
    );
    assert_eq!(normal.filters.minimum_mapping_quality, Some(30));
    assert!((normal.filters.minimum_allele_fraction - 0.07).abs() < f32::EPSILON);
    assert_eq!(
        normal.filters.alignment_color_mode,
        AlignmentColorMode::Haplotype
    );
    assert_eq!(
        normal.filters.auxiliary_predicates,
        vec!["RG=normal", "HP=1"]
    );
    assert!(!normal.filters.include_secondary);
    assert_eq!(normal.maximum_alignments, 50_000);
    assert_eq!(untouched.filters, SessionFilters::default());
}

#[test]
fn ultima_mode_transition_refetches_reference_but_quality_method_only_relayouts() {
    let make_track = || {
        SessionAlignmentTrack::new(
            TrackId::new(2),
            "Ultima".to_owned(),
            TrackSource::Local {
                path: "ultima.bam".to_owned(),
            },
        )
    };
    let mut entering = make_track();
    let (mut editor, mut active) = filter_state_from_session(&entering.filters).unwrap();
    let mut filters = entering.filters.clone();
    filters.alignment_color_mode = AlignmentColorMode::Ultima(UltimaDisplayOptions::default());
    let outcome = commit_alignment_settings_draft(
        &mut entering,
        &mut editor,
        &mut active,
        AlignmentSettingsDraft {
            filters,
            filter_editor: FilterEditor {
                minimum_mapq: 0,
                include_secondary: false,
                include_supplementary: true,
                expression: String::new(),
                error: None,
            },
            maximum_alignments: 50_000,
            apply_filter: false,
        },
        50_000,
    );
    assert!(outcome.query_changed);

    let mut changing_method = make_track();
    changing_method.filters.alignment_color_mode =
        AlignmentColorMode::Ultima(UltimaDisplayOptions::default());
    let (mut editor, mut active) = filter_state_from_session(&changing_method.filters).unwrap();
    let mut filters = changing_method.filters.clone();
    filters.alignment_color_mode = AlignmentColorMode::Ultima(UltimaDisplayOptions {
        quality_mode: UltimaQualityMode::Minimum,
        ..UltimaDisplayOptions::default()
    });
    let outcome = commit_alignment_settings_draft(
        &mut changing_method,
        &mut editor,
        &mut active,
        AlignmentSettingsDraft {
            filters,
            filter_editor: FilterEditor {
                minimum_mapq: 0,
                include_secondary: false,
                include_supplementary: true,
                expression: String::new(),
                error: None,
            },
            maximum_alignments: 50_000,
            apply_filter: false,
        },
        50_000,
    );
    assert!(outcome.configuration_changed);
    assert!(!outcome.query_changed);
}

#[test]
fn bulk_appearance_preserves_sample_specific_filters() {
    let source = SessionFilters {
        alignment_color_mode: AlignmentColorMode::Haplotype,
        alignment_group_mode: AlignmentGroupMode::ReadGroup,
        alignment_group_order: AlignmentOrderDirection::Reverse,
        alignment_sort_mode: AlignmentSortMode::Base,
        alignment_sort_order: AlignmentOrderDirection::Reverse,
        alignment_sort_anchor: Some(4_242),
        read_stacking_mode: ReadStackingMode::Compact,
        alignment_density: 2.5,
        show_junctions: false,
        minimum_junction_flank: 14,
        junction_display_mode: JunctionDisplayMode::Expanded,
        sashimi_scale_mode: SashimiScaleMode::Shared,
        sashimi_fixed_scale_maximum: 250,
        sashimi_count_mode: SashimiCountMode::CountsPerMillion,
        show_junction_labels: false,
        show_modified_bases: false,
        minimum_mapping_quality: Some(5),
        minimum_base_quality: 7,
        auxiliary_predicates: vec!["RG=source".to_owned()],
        include_secondary: false,
        ..SessionFilters::default()
    };
    let mut target = SessionFilters {
        minimum_mapping_quality: Some(30),
        minimum_base_quality: 20,
        auxiliary_predicates: vec!["RG=target".to_owned()],
        include_secondary: true,
        ..SessionFilters::default()
    };

    copy_compatible_alignment_appearance(&mut target, &source);

    assert_eq!(target.alignment_color_mode, AlignmentColorMode::Haplotype);
    assert_eq!(target.alignment_group_mode, AlignmentGroupMode::ReadGroup);
    assert_eq!(
        target.alignment_group_order,
        AlignmentOrderDirection::Reverse
    );
    assert_eq!(target.alignment_sort_mode, AlignmentSortMode::Base);
    assert_eq!(
        target.alignment_sort_order,
        AlignmentOrderDirection::Reverse
    );
    assert_eq!(target.alignment_sort_anchor, Some(4_242));
    assert_eq!(target.read_stacking_mode, ReadStackingMode::Compact);
    assert!((target.alignment_density - 2.5).abs() < f32::EPSILON);
    assert!(!target.show_junctions);
    assert_eq!(target.minimum_junction_flank, 0);
    assert_eq!(target.junction_display_mode, JunctionDisplayMode::Expanded);
    assert_eq!(target.sashimi_scale_mode, SashimiScaleMode::Shared);
    assert_eq!(target.sashimi_fixed_scale_maximum, 250);
    assert_eq!(
        target.sashimi_count_mode,
        SashimiCountMode::CountsPerMillion
    );
    assert!(!target.show_junction_labels);
    assert!(!target.show_modified_bases);
    assert_eq!(target.minimum_mapping_quality, Some(30));
    assert_eq!(target.minimum_base_quality, 20);
    assert_eq!(target.auxiliary_predicates, vec!["RG=target"]);
    assert!(target.include_secondary);
}

#[test]
fn base_sort_position_captures_a_centre_then_keeps_the_fixed_anchor() {
    let viewport = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 100, 200).unwrap(),
        1_000.0,
    )
    .unwrap();

    assert_eq!(
        alignment_sort_position(AlignmentSortMode::Base, None, viewport),
        Some(150)
    );
    let moved = Viewport::new(
        GenomicInterval::new(ContigId::new(0), 500, 900).unwrap(),
        1_000.0,
    )
    .unwrap();
    assert_eq!(
        alignment_sort_position(AlignmentSortMode::Base, Some(150), moved),
        Some(150)
    );
    assert_eq!(
        alignment_sort_position(AlignmentSortMode::MappingQuality, None, viewport),
        None
    );
}
