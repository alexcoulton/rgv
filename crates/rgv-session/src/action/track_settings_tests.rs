//! Annotation, signal and alignment-label edits on the action path: the automation protocol
//! and the window leave the same document, and the edits round-trip through undo and redo.
//!
//! "The same document" is the track part of the serialised session document: the
//! `tracks` list (annotation and signal configurations, in order) and each alignment track's
//! name and sample key.

use super::Action;
use super::alignment_settings_tests::Headless;
use super::track_settings::continuous_edit_key;
use super::translate::{TrackLabelEdit, track_label_actions};
use crate::session::{HistoryStep, redo_action, undo_action};
use rgv_core::{FeatureDisplayMode, TrackConfig, TrackId, TrackKind, TrackSource};

const TUMOUR: TrackId = TrackId::new(1);
const GENES: TrackId = TrackId::new(3);
const PEAKS: TrackId = TrackId::new(4);

fn feature_track(id: TrackId, name: &str, kind: TrackKind) -> TrackConfig {
    TrackConfig::new(
        id,
        name.to_owned(),
        kind,
        TrackSource::Local {
            path: format!("{name}.bed.gz"),
        },
    )
}

fn workspace() -> Headless {
    let mut headless = Headless::new(&[(TUMOUR, "tumour")]);
    headless
        .session
        .tracks
        .push(feature_track(GENES, "genes", TrackKind::GeneModel));
    headless
        .session
        .tracks
        .push(feature_track(PEAKS, "peaks", TrackKind::Feature));
    headless
}

fn document(headless: &Headless) -> String {
    let session = &headless.session;
    serde_json::to_string(&serde_json::json!({
        "tracks": session.tracks.tracks(),
        "alignment_tracks": session
            .alignment_tracks
            .iter()
            .map(|track| (track.config.name.clone(), track.config.sample_key.clone()))
            .collect::<Vec<_>>(),
    }))
    .unwrap()
}

/// What the track settings window sends: the track's configuration with the edited fields.
fn window_config_edit(
    headless: &Headless,
    id: TrackId,
    edit: impl FnOnce(&mut TrackConfig),
) -> Action {
    let mut config = headless
        .session
        .tracks
        .tracks()
        .iter()
        .find(|track| track.id == id)
        .unwrap()
        .clone();
    edit(&mut config);
    Action::SetTrackConfigs {
        tracks: vec![config],
    }
}

#[test]
fn rename_commands_and_the_window_leave_the_same_document() {
    // An annotation track renamed by `set_option` and by its name field.
    let mut by_automation = workspace();
    let mut by_window = workspace();
    let before = document(&by_automation);
    let actions = track_label_actions(
        &by_automation.session,
        PEAKS,
        TrackLabelEdit::Name("ChIP peaks".to_owned()),
    )
    .unwrap();
    by_automation.apply(actions);
    let request = window_config_edit(&by_window, PEAKS, |track| {
        track.name = "ChIP peaks".to_owned();
    });
    by_window.apply(vec![request]);
    assert_eq!(document(&by_automation), document(&by_window));
    assert_ne!(document(&by_automation), before);

    // An alignment track's sample association set by `set_option` and by the window.
    let mut by_automation = workspace();
    let mut by_window = workspace();
    let actions = track_label_actions(
        &by_automation.session,
        TUMOUR,
        TrackLabelEdit::SampleKey(Some("patient-7".to_owned())),
    )
    .unwrap();
    by_automation.apply(actions);
    let mut label = by_window.session.alignment_track_label(TUMOUR).unwrap();
    label.sample_key = Some("patient-7".to_owned());
    by_window.apply(vec![Action::SetAlignmentTrackLabels {
        labels: vec![label],
    }]);
    assert_eq!(document(&by_automation), document(&by_window));

    assert!(
        track_label_actions(
            &by_automation.session,
            TrackId::new(9),
            TrackLabelEdit::Name("x".to_owned())
        )
        .is_err()
    );
}

#[test]
fn track_settings_edits_round_trip_through_undo_and_redo() {
    let edits: [fn(&Headless) -> Action; 4] = [
        |headless| {
            window_config_edit(headless, GENES, |track| {
                track.feature.display_mode = FeatureDisplayMode::Squished;
            })
        },
        |headless| {
            window_config_edit(headless, PEAKS, |track| {
                track.sample_key = Some("patient-7".to_owned());
            })
        },
        |_| Action::MoveTrack {
            track_id: PEAKS,
            index: 0,
        },
        |headless| {
            let mut label = headless.session.alignment_track_label(TUMOUR).unwrap();
            label.name = "Tumour (WGS)".to_owned();
            Action::SetAlignmentTrackLabels {
                labels: vec![label],
            }
        },
    ];
    for edit in edits {
        let mut headless = workspace();
        let before = document(&headless);
        let action = edit(&headless);
        headless.apply(vec![action.clone()]);
        let after = document(&headless);
        assert_ne!(after, before, "{action:?}");
        assert!(matches!(
            undo_action(&mut headless),
            HistoryStep::Done { .. }
        ));
        assert_eq!(document(&headless), before, "undo of {action:?}");
        assert!(matches!(
            redo_action(&mut headless),
            HistoryStep::Done { .. }
        ));
        assert_eq!(document(&headless), after, "redo of {action:?}");
        assert!(matches!(
            undo_action(&mut headless),
            HistoryStep::Done { .. }
        ));
        assert!(matches!(undo_action(&mut headless), HistoryStep::Nothing));
    }
}

#[test]
fn a_configuration_cannot_replace_the_track_it_names() {
    let mut headless = workspace();
    let mut config = headless.session.tracks.tracks()[0].clone();
    config.source = TrackSource::Local {
        path: "other.bed.gz".to_owned(),
    };
    assert!(headless.session.apply_track_configs(&[config]).is_err());
    let missing = feature_track(TrackId::new(9), "missing", TrackKind::Feature);
    assert!(headless.session.apply_track_configs(&[missing]).is_err());
}

#[test]
fn typing_and_dragging_coalesce_but_toggles_do_not() {
    let key = |before: serde_json::Value, after: serde_json::Value| {
        continuous_edit_key("t", [(1, before, after)])
    };
    let typing = key(
        serde_json::json!({"name": "Pe", "height": 40.0}),
        serde_json::json!({"name": "Pea", "height": 40.0}),
    );
    assert!(typing.is_some());
    assert_eq!(
        typing,
        key(
            serde_json::json!({"name": "Pea", "height": 40.0}),
            serde_json::json!({"name": "Peak", "height": 40.0}),
        ),
        "the same field is the same gesture"
    );
    assert!(
        key(
            serde_json::json!({"quantitative": {"minimum": 0.0}}),
            serde_json::json!({"quantitative": {"minimum": 0.5}}),
        )
        .is_some()
    );
    assert_ne!(
        typing,
        key(
            serde_json::json!({"name": "Peak", "height": 40.0}),
            serde_json::json!({"name": "Peak", "height": 48.0}),
        )
    );
    assert!(
        key(
            serde_json::json!({"visible": true}),
            serde_json::json!({"visible": false}),
        )
        .is_none(),
        "a toggle is a step of its own"
    );
    assert!(
        key(
            serde_json::json!({"samples": ["a"]}),
            serde_json::json!({"samples": ["a", "b"]}),
        )
        .is_none(),
        "adding an entry is a step of its own"
    );
}

#[test]
fn selecting_a_track_by_protocol_is_a_plain_click_on_a_cleared_panel() {
    use super::SelectionExtent;
    use super::translate::automation_session_actions;
    use rgv_automation::{AutomationCommand, AutomationSelection};
    use rgv_core::{PanelId, PresentationNodeId, TrackChannel};

    let headless = workspace();
    let panel_id = PanelId::new(1);
    let select = |track_id| AutomationCommand::Select {
        selection: AutomationSelection::Track { track_id },
    };
    let actions = automation_session_actions(&headless.session, &select(PEAKS))
        .unwrap()
        .unwrap();
    // The window's plain click on the same row.
    let click = Action::SelectTrack {
        panel_id,
        target: PresentationNodeId::Track {
            channel: TrackChannel::Annotation,
            track_id: PEAKS,
        },
        extent: SelectionExtent::from_modifiers(false, false),
    };
    assert_eq!(actions, vec![Action::ClearSelection { panel_id }, click]);
    assert_eq!(
        automation_session_actions(&headless.session, &AutomationCommand::ClearSelection)
            .unwrap()
            .unwrap(),
        vec![Action::ClearSelection { panel_id }]
    );
    assert!(
        automation_session_actions(&headless.session, &select(TrackId::new(9)))
            .unwrap()
            .is_err()
    );
    assert_eq!(
        SelectionExtent::from_modifiers(true, true),
        SelectionExtent::Range
    );
    assert_eq!(
        SelectionExtent::from_modifiers(false, true),
        SelectionExtent::Toggle
    );
}

#[test]
fn preference_settings_and_the_settings_window_leave_the_same_settings_document() {
    use super::Preferences;
    use super::translate::preference_setting_action;
    use rgv_automation::AutomationSetting;
    use rgv_core::TrackPipeline;

    let window_edits: [(AutomationSetting, fn(&mut Preferences)); 3] = [
        (
            AutomationSetting::TotalCacheBudgetMib { value: 2_048 },
            |preferences| preferences.cache_budget_mib = 2_048,
        ),
        (
            AutomationSetting::ReadBrowsingBufferPercent { value: 75 },
            |preferences| preferences.read_browsing_buffer_percent = 75,
        ),
        (
            AutomationSetting::TrackPipeline {
                value: TrackPipeline::Tiles,
            },
            |preferences| preferences.track_pipeline = TrackPipeline::Tiles,
        ),
    ];
    for (setting, window) in window_edits {
        let mut by_automation = workspace();
        let mut by_window = workspace();
        let before = serde_json::to_string(&by_automation.session.settings).unwrap();
        let Some(Action::SetPreferences(requested)) =
            preference_setting_action(&by_automation.session, &setting)
        else {
            panic!("{setting:?} is a preference");
        };
        requested.write_to(&mut by_automation.session.settings);
        let mut controls = Preferences::of(&by_window.session.settings);
        window(&mut controls);
        controls.write_to(&mut by_window.session.settings);
        let document =
            |headless: &Headless| serde_json::to_string(&headless.session.settings).unwrap();
        assert_eq!(
            document(&by_automation),
            document(&by_window),
            "{setting:?}"
        );
        assert_ne!(document(&by_automation), before, "{setting:?}");
    }
    assert!(
        preference_setting_action(
            &workspace().session,
            &AutomationSetting::ShowCoverage { value: true }
        )
        .is_none()
    );
}
