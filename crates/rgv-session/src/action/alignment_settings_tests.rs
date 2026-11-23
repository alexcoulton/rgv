//! Alignment settings on the action path, driven on a headless session: the automation
//! protocol and the settings window leave the same document, and every change round-trips
//! through undo and redo.
//!
//! "The same document" is the alignment part of the serialised session document: each track's
//! filters and complete-read limit as `session_document` writes them (the primary track with
//! its committed filter clauses) and the workspace complete-read limit.

use super::alignment_settings::{
    AlignmentSettings, EMPTY_ALIGNMENT_TRACK_ID, merge_alignment_settings_draft,
};
use super::history::{UNDO_COALESCE_WINDOW, UndoEntry, UndoHistory};
use super::track_settings::{track_configs_coalesce_key, track_configs_inverse};
use super::translate::{
    AlignmentOption, alignment_option_actions, automation_alignment_settings_actions,
};
use super::{Action, ActionOutcome};
use crate::Session;
use crate::alignment::track::{
    AlignmentPanelProjection, AlignmentTrackController, AlignmentTrackLifecycle,
    filter_state_from_session,
};
use crate::panels::PanelState;
use crate::session::{
    ActionHost, HistoryStep, PanelCanvas, apply_action, redo_action, undo_action,
};
use anyhow::Result;
use rgv_automation::{AutomationCommand, AutomationSetting};
use rgv_core::{
    AlignmentPredicateExpression, AlignmentPredicateScope, AlignmentViewPredicate, ContigId,
    DesktopSettings, GenomeAssembly, GenomicInterval, PanelId, ReferenceDictionary,
    SessionAlignmentTrack, TrackId, TrackSource, Viewport,
};
use rgv_layout::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode,
};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Instant;

const TUMOUR: TrackId = TrackId::new(1);
const NORMAL: TrackId = TrackId::new(2);

fn controller(id: TrackId, name: &str) -> AlignmentTrackController {
    let config = SessionAlignmentTrack::new(
        id,
        name.to_owned(),
        TrackSource::Local {
            path: format!("{name}.bam"),
        },
    );
    let (filter_editor, active_filters) = filter_state_from_session(&config.filters).unwrap();
    AlignmentTrackController {
        scene_complete_read_limit: config.effective_complete_read_limit(),
        config,
        scene_engine: None,
        coverage_engine: None,
        coverage_publication_sequence: 0,
        scene_metadata: None,
        lifecycle: AlignmentTrackLifecycle::Opening,
        dictionary: None,
        index_overview: None,
        filter_editor,
        active_filters,
        projections: HashMap::from([(PanelId::new(1), AlignmentPanelProjection::default())]),
        scene_budget_bytes: NonZeroUsize::new(1 << 20).unwrap(),
        scene_budget_retry: None,
    }
}

fn session(alignments: &[(TrackId, &str)]) -> Session {
    let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000_000)]);
    let interval = GenomicInterval::new(ContigId::new(0), 10_000, 20_000).unwrap();
    let panel = PanelState::new(
        PanelId::new(1),
        Viewport::new(interval, 600.0).unwrap(),
        None,
        "chr1".to_owned(),
        1_000_000,
    );
    let mut session = Session::new(
        panel,
        GenomeAssembly::from_dictionary("test", dictionary),
        DesktopSettings::default(),
        PathBuf::from("settings.json"),
    );
    session.alignment_tracks = alignments
        .iter()
        .map(|(id, name)| controller(*id, name))
        .collect();
    session
}

/// The alignment part of the serialised session document.
fn alignment_document(session: &Session) -> String {
    let ids = if session.alignment_tracks.is_empty() {
        vec![EMPTY_ALIGNMENT_TRACK_ID]
    } else {
        session
            .alignment_tracks
            .iter()
            .map(|track| track.config.id)
            .collect()
    };
    let tracks = ids
        .into_iter()
        .map(|id| {
            let settings = session.alignment_settings(id).unwrap();
            serde_json::json!({
                "id": id.get(),
                "filters": settings.filters,
                "maximum_alignments": settings.maximum_alignments,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&serde_json::json!({
        "alignment_tracks": tracks,
        "maximum_alignments": session.workspace_alignment_limit,
    }))
    .unwrap()
}

/// A window-less host: the session carries out alignment settings and panel actions alone,
/// through the same `apply_action` / `undo_action` / `redo_action` the desktop uses.
pub(super) struct Headless {
    pub(super) session: Session,
}

impl ActionHost for Headless {
    fn undo_history(&mut self) -> &mut UndoHistory {
        &mut self.session.undo
    }

    fn perform_action(&mut self, action: Action) -> Result<(ActionOutcome, Option<UndoEntry>)> {
        let (outcome, inverse, coalesce) = match &action {
            Action::SetAlignmentSettings {
                updates,
                workspace_limit,
            } => {
                let applied = self
                    .session
                    .apply_alignment_settings(updates, *workspace_limit)?;
                (
                    ActionOutcome {
                        changed: applied.changed(),
                        ..ActionOutcome::default()
                    },
                    applied.inverse().into_iter().collect(),
                    applied.coalesce_key(),
                )
            }
            Action::SetTrackConfigs { tracks } => {
                let changes = self.session.apply_track_configs(tracks)?;
                (
                    ActionOutcome {
                        changed: !changes.is_empty(),
                        ..ActionOutcome::default()
                    },
                    track_configs_inverse(&changes).into_iter().collect(),
                    track_configs_coalesce_key(&changes),
                )
            }
            Action::SetAlignmentTrackLabels { labels } => {
                let previous = self.session.apply_alignment_track_labels(labels)?;
                let changed = !previous.is_empty();
                (
                    ActionOutcome {
                        changed,
                        ..ActionOutcome::default()
                    },
                    if changed {
                        vec![Action::SetAlignmentTrackLabels { labels: previous }]
                    } else {
                        Vec::new()
                    },
                    None,
                )
            }
            Action::MoveTrack { track_id, index } => {
                let previous = self.session.move_auxiliary_track(*track_id, *index);
                (
                    ActionOutcome {
                        changed: previous.is_some(),
                        ..ActionOutcome::default()
                    },
                    previous
                        .filter(|previous| previous != index)
                        .map(|previous| Action::MoveTrack {
                            track_id: *track_id,
                            index: previous,
                        })
                        .into_iter()
                        .collect(),
                    None,
                )
            }
            _ => {
                let width = |_| 600.0;
                let canvas = PanelCanvas {
                    width_for: &width,
                    content_top: 0.0,
                };
                let reduction = self.session.reduce(&action, &canvas, None)?;
                (reduction.outcome, reduction.inverse, reduction.coalesce)
            }
        };
        Ok((
            outcome,
            Session::<PanelState>::undo_entry(action, inverse, coalesce),
        ))
    }
}

impl Headless {
    pub(super) fn new(alignments: &[(TrackId, &str)]) -> Self {
        Self {
            session: session(alignments),
        }
    }

    pub(super) fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            apply_action(self, action).unwrap();
        }
    }

    fn automation(&mut self, command: &AutomationCommand) {
        let actions = automation_alignment_settings_actions(&self.session, command)
            .expect("the command is on the alignment settings path")
            .unwrap();
        self.apply(actions);
    }

    /// The settings window: the controls start from the track's settings, the user edits some
    /// of them, and the commit takes only the edited fields.
    fn window(&mut self, track_id: TrackId, edit: impl FnOnce(&mut AlignmentSettings)) {
        let current = self.session.alignment_settings(track_id).unwrap();
        let mut drafted = current.clone();
        edit(&mut drafted);
        let next = merge_alignment_settings_draft(&current, &current, &drafted);
        if next != current {
            self.apply(vec![Action::SetAlignmentSettings {
                updates: vec![(track_id, next)],
                workspace_limit: None,
            }]);
        }
    }

    fn document(&self) -> String {
        alignment_document(&self.session)
    }
}

fn assert_same_document(
    alignments: &[(TrackId, &str)],
    command: &AutomationCommand,
    window: &dyn Fn(&mut Headless),
) {
    let mut by_automation = Headless::new(alignments);
    let mut by_window = Headless::new(alignments);
    let before = by_automation.document();
    by_automation.automation(command);
    window(&mut by_window);
    assert_eq!(
        by_automation.document(),
        by_window.document(),
        "{command:?}"
    );
    assert_ne!(
        by_automation.document(),
        before,
        "{command:?} is not a no-op"
    );
}

fn configure(setting: AutomationSetting) -> AutomationCommand {
    AutomationCommand::Configure { setting }
}

#[test]
fn alignment_settings_commands_and_the_settings_window_leave_the_same_document() {
    let both = [(TUMOUR, "tumour"), (NORMAL, "normal")];
    // A primary-track toggle with no source open, and with sources open.
    for alignments in [&[][..], &both[..]] {
        let primary = if alignments.is_empty() {
            EMPTY_ALIGNMENT_TRACK_ID
        } else {
            TUMOUR
        };
        assert_same_document(
            alignments,
            &configure(AutomationSetting::ShowCoverage { value: false }),
            &|window| window.window(primary, |settings| settings.filters.show_coverage = false),
        );
        assert_same_document(
            alignments,
            &configure(AutomationSetting::IncludeSecondary { value: true }),
            &|window| {
                window.window(primary, |settings| {
                    settings.filters.include_secondary = true;
                });
            },
        );
    }
    // A density slider on the second track.
    assert_same_document(
        &both,
        &configure(AutomationSetting::AlignmentDensity {
            value: 2.5,
            track_id: Some(NORMAL),
        }),
        &|window| window.window(NORMAL, |settings| settings.filters.alignment_density = 2.5),
    );
    // Colour by strand on both tracks: one command, two window edits.
    assert_same_document(
        &both,
        &configure(AutomationSetting::AlignmentColorMode {
            track_ids: vec![TUMOUR, NORMAL],
            value: AlignmentColorMode::Strand,
        }),
        &|window| {
            for track in [TUMOUR, NORMAL] {
                window.window(track, |settings| {
                    settings.filters.alignment_color_mode = AlignmentColorMode::Strand;
                });
            }
        },
    );
    // A complete-read limit above the workspace limit widens the workspace limit.
    assert_same_document(
        &both,
        &configure(AutomationSetting::CompleteReadLimit {
            track_ids: vec![NORMAL],
            value: 400_000,
        }),
        &|window| window.window(NORMAL, |settings| settings.maximum_alignments = 400_000),
    );
    // `group`, `sort` and `filter` against the same edits in the window.
    assert_same_document(
        &both,
        &AutomationCommand::Group {
            track_ids: vec![NORMAL],
            mode: AlignmentGroupMode::Strand,
            direction: AlignmentOrderDirection::Reverse,
            tag: None,
            anchor_position: None,
        },
        &|window| {
            window.window(NORMAL, |settings| {
                settings.filters.alignment_group_mode = AlignmentGroupMode::Strand;
                settings.filters.alignment_group_order = AlignmentOrderDirection::Reverse;
            });
        },
    );
    assert_same_document(
        &both,
        &AutomationCommand::Sort {
            track_ids: vec![TUMOUR],
            mode: AlignmentSortMode::Base,
            direction: AlignmentOrderDirection::Natural,
            tag: None,
            anchor_position: Some(15_000),
        },
        &|window| {
            window.window(TUMOUR, |settings| {
                settings.filters.alignment_sort_mode = AlignmentSortMode::Base;
                settings.filters.alignment_sort_anchor = Some(15_000);
            });
        },
    );
    assert_same_document(
        &both,
        &AutomationCommand::Filter {
            track_ids: vec![TUMOUR],
            expression: " HP=1 ; NM<=2 ".to_owned(),
        },
        &|window| {
            window.window(TUMOUR, |settings| {
                settings.filters.auxiliary_predicates = vec!["HP=1".to_owned(), "NM<=2".to_owned()];
            });
        },
    );
}

#[test]
fn the_untyped_options_reach_the_same_settings_as_their_typed_settings() {
    let mut by_option = Headless::new(&[(TUMOUR, "tumour")]);
    let mut by_setting = Headless::new(&[(TUMOUR, "tumour")]);
    let actions = alignment_option_actions(
        &by_option.session,
        AlignmentOption::ShowJunctions(false),
        None,
    )
    .unwrap();
    by_option.apply(actions);
    by_setting.automation(&configure(AutomationSetting::ShowJunctions {
        value: false,
    }));
    assert_eq!(by_option.document(), by_setting.document());
}

#[test]
fn invalid_alignment_commands_change_nothing() {
    let headless = Headless::new(&[(TUMOUR, "tumour")]);
    let missing = TrackId::new(9);
    for command in [
        configure(AutomationSetting::AlignmentColorMode {
            track_ids: vec![TUMOUR, missing],
            value: AlignmentColorMode::Strand,
        }),
        configure(AutomationSetting::AlignmentDensity {
            value: 50.0,
            track_id: None,
        }),
        configure(AutomationSetting::AlignmentDensity {
            value: 1.0,
            track_id: Some(missing),
        }),
        AutomationCommand::Filter {
            track_ids: vec![TUMOUR],
            expression: "not a predicate!".to_owned(),
        },
    ] {
        let result = automation_alignment_settings_actions(&headless.session, &command)
            .expect("an alignment settings command");
        assert!(result.is_err(), "{command:?}");
    }
    assert!(
        alignment_option_actions(
            &headless.session,
            AlignmentOption::MinimumAlleleFraction(1.5),
            None
        )
        .is_err()
    );
    // A reference-backed view filter needs a reference.
    let predicate = AlignmentPredicateExpression {
        all: vec![AlignmentViewPredicate::Substitution {
            scope: AlignmentPredicateScope::default(),
            observed_base: Some(b'A'),
            minimum_count: None,
            maximum_count: None,
        }],
        ..AlignmentPredicateExpression::default()
    };
    let command = configure(AutomationSetting::AlignmentViewFilter {
        track_ids: vec![TUMOUR],
        value: predicate,
    });
    assert!(
        automation_alignment_settings_actions(&headless.session, &command)
            .unwrap()
            .is_err()
    );
}

#[test]
fn alignment_settings_round_trip_through_undo_and_redo() {
    let edits = [
        configure(AutomationSetting::ShowCoverage { value: false }),
        configure(AutomationSetting::AlignmentColorMode {
            track_ids: vec![TUMOUR, NORMAL],
            value: AlignmentColorMode::Strand,
        }),
        configure(AutomationSetting::CompleteReadLimit {
            track_ids: vec![NORMAL],
            value: 400_000,
        }),
        AutomationCommand::Filter {
            track_ids: vec![TUMOUR],
            expression: "HP=1".to_owned(),
        },
        configure(AutomationSetting::IncludeSupplementary { value: false }),
    ];
    for edit in edits {
        let mut headless = Headless::new(&[(TUMOUR, "tumour"), (NORMAL, "normal")]);
        let before = headless.document();
        headless.automation(&edit);
        let after = headless.document();
        assert_ne!(after, before, "{edit:?}");

        assert!(matches!(
            undo_action(&mut headless),
            HistoryStep::Done { .. }
        ));
        assert_eq!(headless.document(), before, "undo of {edit:?}");
        assert!(matches!(
            redo_action(&mut headless),
            HistoryStep::Done { .. }
        ));
        assert_eq!(headless.document(), after, "redo of {edit:?}");
        assert!(matches!(
            undo_action(&mut headless),
            HistoryStep::Done { .. }
        ));
        assert_eq!(headless.document(), before, "second undo of {edit:?}");
        assert!(matches!(undo_action(&mut headless), HistoryStep::Nothing));
    }
}

#[test]
fn an_undone_filter_puts_the_filter_editor_back() {
    let mut headless = Headless::new(&[(TUMOUR, "tumour")]);
    headless.automation(&AutomationCommand::Filter {
        track_ids: vec![TUMOUR],
        expression: "HP=1;NM<=2".to_owned(),
    });
    let track = &headless.session.alignment_tracks[0];
    assert_eq!(track.filter_editor.expression, "HP=1;NM<=2");
    assert_eq!(track.active_filters.alignment.auxiliary.len(), 2);
    undo_action(&mut headless);
    let track = &headless.session.alignment_tracks[0];
    assert!(track.filter_editor.expression.is_empty());
    assert!(track.active_filters.alignment.auxiliary.is_empty());
}

#[test]
fn a_slider_drag_is_one_undo_step_but_toggles_are_not_merged() {
    let mut headless = Headless::new(&[(TUMOUR, "tumour")]);
    let before = headless.document();
    for density in [1.2, 1.4, 1.6, 1.8] {
        headless.window(TUMOUR, |settings| {
            settings.filters.alignment_density = density;
        });
    }
    let dragged = headless.document();
    assert!(matches!(
        undo_action(&mut headless),
        HistoryStep::Done { .. }
    ));
    assert_eq!(
        headless.document(),
        before,
        "one undo returns to before the drag"
    );
    assert!(matches!(undo_action(&mut headless), HistoryStep::Nothing));
    assert!(matches!(
        redo_action(&mut headless),
        HistoryStep::Done { .. }
    ));
    assert_eq!(
        headless.document(),
        dragged,
        "redo lands where the drag ended"
    );

    // Two toggles in quick succession are two steps.
    headless.window(TUMOUR, |settings| settings.filters.show_coverage = false);
    headless.window(TUMOUR, |settings| settings.filters.show_junctions = false);
    assert!(matches!(
        undo_action(&mut headless),
        HistoryStep::Done { .. }
    ));
    assert!(
        !headless
            .session
            .alignment_settings(TUMOUR)
            .unwrap()
            .filters
            .show_coverage
    );
    assert!(matches!(
        undo_action(&mut headless),
        HistoryStep::Done { .. }
    ));
    assert!(
        headless
            .session
            .alignment_settings(TUMOUR)
            .unwrap()
            .filters
            .show_coverage
    );
}

#[test]
fn a_drag_paused_longer_than_the_window_is_two_steps() {
    let mut history = UndoHistory::default();
    let mut headless = Headless::new(&[(TUMOUR, "tumour")]);
    let start = Instant::now();
    for (density, at) in [(1.2, start), (1.4, start + UNDO_COALESCE_WINDOW * 2)] {
        let current = headless.session.alignment_settings(TUMOUR).unwrap();
        let mut next = current.clone();
        next.filters.alignment_density = density;
        let (_, entry) = headless
            .perform_action(Action::SetAlignmentSettings {
                updates: vec![(TUMOUR, next)],
                workspace_limit: None,
            })
            .unwrap();
        history.record(entry.unwrap(), at);
    }
    assert!(history.take_undo().is_some());
    assert!(history.take_undo().is_some());
}

#[test]
fn an_untouched_window_writes_nothing_even_over_settings_it_cannot_show() {
    let mut headless = Headless::new(&[(TUMOUR, "tumour")]);
    // A modification code the controls cannot parse, and a one-sided pair bound.
    headless.session.alignment_tracks[0]
        .config
        .filters
        .hidden_modification_codes = vec!["not-a-code".to_owned()];
    headless.session.alignment_tracks[0]
        .config
        .filters
        .minimum_template_length = Some(50);
    let current = headless.session.alignment_settings(TUMOUR).unwrap();
    // What the controls show for those settings, and the same draft after an edit.
    let mut baseline = current.clone();
    baseline.filters.hidden_modification_codes.clear();
    baseline.filters.maximum_template_length = Some(1_000);
    assert_eq!(
        merge_alignment_settings_draft(&current, &baseline, &baseline),
        current,
        "an untouched draft is not an edit"
    );
    let mut edited = baseline.clone();
    edited.filters.show_coverage = false;
    let merged = merge_alignment_settings_draft(&current, &baseline, &edited);
    assert!(!merged.filters.show_coverage);
    assert_eq!(
        merged.filters.hidden_modification_codes,
        current.filters.hidden_modification_codes
    );
    assert_eq!(merged.filters.maximum_template_length, None);
}

#[test]
fn query_and_presentation_fields_are_told_apart() {
    let mut headless = Headless::new(&[(TUMOUR, "tumour"), (NORMAL, "normal")]);
    let apply = |headless: &mut Headless, track: TrackId, edit: fn(&mut AlignmentSettings)| {
        let mut next = headless.session.alignment_settings(track).unwrap();
        edit(&mut next);
        headless
            .session
            .apply_alignment_settings(&[(track, next)], None)
            .unwrap()
    };
    let applied = apply(&mut headless, NORMAL, |settings| {
        settings.filters.minimum_base_quality = 20;
    });
    let change = &applied.changes[0];
    assert!(change.query_changed() && !change.primary);
    let applied = apply(&mut headless, TUMOUR, |settings| {
        settings.filters.show_coverage = false;
    });
    let change = &applied.changes[0];
    assert!(change.primary && !change.query_changed() && !change.style_changed());
    assert!(change.presentation_changed() && change.turned_off("show_coverage"));
    let applied = apply(&mut headless, TUMOUR, |settings| {
        settings.filters.alignment_group_mode = AlignmentGroupMode::Strand;
    });
    assert!(applied.changes[0].style_changed() && !applied.changes[0].query_changed());
    assert!(applied.coalesce_key().is_none());
}

#[test]
fn sashimi_presentation_fields_ask_for_a_new_presentation_not_a_query() {
    let before = AlignmentSettings {
        filters: rgv_core::SessionFilters::default(),
        maximum_alignments: 1_000,
    };
    let mut after = before.clone();
    after.filters.junction_relative_threshold_percent = 5.0;
    after.filters.junction_library_strandedness =
        rgv_core::LibraryStrandedness::FirstOfPairAntisense;
    let change = super::alignment_settings::AlignmentSettingsChange {
        track_id: rgv_core::TrackId::new(1),
        primary: true,
        fields: super::alignment_settings::changed_alignment_fields(&before, &after),
        before,
        after,
    };
    assert!(change.sashimi_presentation_changed());
    assert!(!change.query_changed());
}

#[test]
fn the_modification_display_and_lane_code_are_typed_configure_settings() {
    let both = [(TUMOUR, "tumour"), (NORMAL, "normal")];
    assert!(
        !Headless::new(&both)
            .session
            .alignment_settings(TUMOUR)
            .unwrap()
            .filters
            .draws_unmodified_calls(),
        "modified-only is the default"
    );
    assert_same_document(
        &both,
        &configure(AutomationSetting::ShowUnmodifiedModificationCalls { value: true }),
        &|window| {
            window.window(TUMOUR, |settings| {
                settings.filters.set_draws_unmodified_calls(true);
            });
        },
    );
    assert_same_document(
        &both,
        &configure(AutomationSetting::ModificationLaneCode {
            value: Some("h".to_owned()),
        }),
        &|window| {
            window.window(TUMOUR, |settings| {
                settings.filters.modification_lane_code = Some("h".to_owned());
            });
        },
    );
    // The setting reaches the lane: a chosen code, and the fraction of called reads.
    let mut headless = Headless::new(&both);
    headless.automation(&configure(AutomationSetting::ModificationLaneCode {
        value: Some("CHEBI:76792".to_owned()),
    }));
    let options = crate::alignment::track::alignment_scene_modification_lane_options(
        &headless.session.alignment_settings(TUMOUR).unwrap().filters,
        None,
    );
    assert_eq!(
        options.projection.shown_code,
        Some(rgv_model::ModificationCode::Chebi(76_792))
    );
    assert!(!options.projection.probability_weighted);
    // Not a modification code.
    assert!(
        automation_alignment_settings_actions(
            &headless.session,
            &configure(AutomationSetting::ModificationLaneCode {
                value: Some("5mC".to_owned()),
            }),
        )
        .expect("the command is on the alignment settings path")
        .is_err()
    );
    // The 2-colour switch, and back.
    headless.automation(&configure(
        AutomationSetting::ShowUnmodifiedModificationCalls { value: true },
    ));
    let filters = headless.session.alignment_settings(TUMOUR).unwrap().filters;
    assert!(filters.show_unmodified_calls && filters.draws_unmodified_calls());
    headless.automation(&configure(
        AutomationSetting::ShowUnmodifiedModificationCalls { value: false },
    ));
    assert!(
        !headless
            .session
            .alignment_settings(TUMOUR)
            .unwrap()
            .filters
            .draws_unmodified_calls()
    );
}
