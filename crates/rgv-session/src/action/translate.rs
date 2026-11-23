//! How the two front ends phrase an intent as an [`Action`].
//!
//! These are the only places a keyboard step or an automation command is turned into an action,
//! so the tests can hold the window and the protocol to the same result.

use super::alignment_settings::{
    AlignmentSettings, MAXIMUM_ALIGNMENT_DENSITY, MINIMUM_ALIGNMENT_DENSITY,
};
use super::{Action, Preferences, SelectionExtent, TrackLoad, ViewportCommit};
use crate::Session;
use crate::alignment::track::alignment_sort_position;
use crate::navigation::split_locus_expression;
use crate::panels::Panel;
use anyhow::Result;
use rgv_automation::{
    AutomationCommand, AutomationHistoryNavigation, AutomationSelection, AutomationSetting,
    AutomationViewportOperation,
};
use rgv_core::{
    AlignmentPredicateExpression, AlignmentViewPredicate, CoverageScaleTransform, PanelId,
    PresentationNodeId, SessionFilters, TrackChannel, TrackId, TrackIntent, TrackKind, TrackSource,
};
use rgv_hts::AuxPredicate;
use rgv_layout::{AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode};

/// `change_viewport` from the automation protocol: one complete, recorded navigation.
pub const fn change_viewport(panel_id: PanelId, operation: AutomationViewportOperation) -> Action {
    Action::ChangeViewport {
        panel_id,
        operation,
        commit: ViewportCommit::Discrete,
    }
}

/// `navigate_history` from the automation protocol.
pub const fn navigate_history(panel_id: PanelId, step: AutomationHistoryNavigation) -> Action {
    Action::NavigateHistory { panel_id, step }
}

/// An arrow-key or overview pan step: a fraction of the panel width, positive to the right.
pub const fn keyboard_pan(panel_id: PanelId, fraction: f64) -> Action {
    Action::ChangeViewport {
        panel_id,
        operation: AutomationViewportOperation::PanFraction { fraction },
        commit: ViewportCommit::Step,
    }
}

/// A `+`/`-` or toolbar zoom step about the panel centre. The window speaks in span
/// multipliers (0.8 zooms in); the protocol's zoom factor is the reciprocal magnification.
pub fn keyboard_zoom(panel_id: PanelId, span_factor: f64) -> Action {
    Action::ChangeViewport {
        panel_id,
        operation: AutomationViewportOperation::Zoom {
            factor: span_factor.recip(),
            anchor_position: None,
        },
        commit: ViewportCommit::Step,
    }
}

/// The locus box: one locus retargets `focused`; several lay the workspace out as those loci.
pub fn locus_box(focused: PanelId, expression: &str) -> Result<Action> {
    let loci = split_locus_expression(expression)?
        .into_iter()
        .map(str::to_owned)
        .collect();
    Ok(Action::GoToLoci {
        panel_id: focused,
        loci,
    })
}

/// The panel strip's deferred requests for one frame, in the order they are applied: a move,
/// then a close, else an activation.
pub fn panel_strip_requests(
    panel_to_move: Option<(PanelId, usize)>,
    panel_to_close: Option<PanelId>,
    panel_to_activate: Option<PanelId>,
) -> Vec<Action> {
    let mut actions = Vec::new();
    if let Some((panel_id, index)) = panel_to_move {
        actions.push(Action::MovePanel { panel_id, index });
    }
    if let Some(panel_id) = panel_to_close {
        actions.push(Action::ClosePanel { panel_id });
    } else if let Some(panel_id) = panel_to_activate {
        actions.push(Action::ActivatePanel { panel_id });
    }
    actions
}

/// The arrangement settings of the automation protocol as track intents. `None` for settings
/// that are not track-arrangement edits.
pub fn setting_track_intents(setting: &AutomationSetting) -> Option<Vec<TrackIntent>> {
    Some(vec![match setting {
        AutomationSetting::PresentationSectionExpanded { channel, value } => {
            TrackIntent::SetSectionExpanded {
                channel: *channel,
                expanded: *value,
            }
        }
        AutomationSetting::PresentationNodeVisible { target, value } => {
            TrackIntent::SetNodeVisibility {
                target: *target,
                visible: *value,
            }
        }
        AutomationSetting::PresentationNodeHeight { target, value } => TrackIntent::ResizeNode {
            target: *target,
            preferred_height: *value,
        },
        AutomationSetting::PresentationGroupExpanded { target, value } => {
            TrackIntent::SetGroupExpanded {
                target: *target,
                expanded: *value,
            }
        }
        AutomationSetting::PresentationGroupVisible { target, value } => {
            TrackIntent::SetGroupVisibility {
                target: *target,
                visible: *value,
            }
        }
        AutomationSetting::PresentationCompositeVisible {
            channel,
            composite_id,
            value,
        } => TrackIntent::SetCompositeVisibility {
            channel: *channel,
            composite_id: *composite_id,
            visible: *value,
        },
        AutomationSetting::PresentationGroup { channel, track_ids } => TrackIntent::GroupNodes {
            channel: *channel,
            members: track_ids.clone(),
        },
        AutomationSetting::PresentationUngroup { target } => {
            TrackIntent::Ungroup { target: *target }
        }
        AutomationSetting::ReadPaneHeight { value } => {
            TrackIntent::ResizeReadPane { height: *value }
        }
        _ => return None,
    }])
}

/// The automation commands that are pure action translations, as the actions they apply.
/// `focused` is the panel with keyboard focus when the command arrives; commands that name a
/// panel never read it. `None` for commands that are queries, waits, file work, or not yet on
/// the action path.
///
/// `change_viewport` and `navigate_history` document that their panel becomes the focused
/// panel, so they translate to a focus change followed by the navigation.
pub fn automation_command_actions(
    command: &AutomationCommand,
    focused: PanelId,
) -> Option<Result<Vec<Action>>> {
    Some(match command {
        AutomationCommand::Goto { loci } => {
            locus_box(focused, &loci.join(" ")).map(|action| vec![action])
        }
        AutomationCommand::ChangeViewport {
            panel_id,
            operation,
        } => Ok(vec![
            Action::ActivatePanel {
                panel_id: *panel_id,
            },
            change_viewport(*panel_id, *operation),
        ]),
        AutomationCommand::NavigateHistory { panel_id, action } => Ok(vec![
            Action::ActivatePanel {
                panel_id: *panel_id,
            },
            navigate_history(*panel_id, *action),
        ]),
        AutomationCommand::AddPanel { locus } => Ok(vec![Action::AddPanel {
            locus: locus.clone(),
        }]),
        AutomationCommand::ClosePanel { panel_id } => Ok(vec![Action::ClosePanel {
            panel_id: *panel_id,
        }]),
        AutomationCommand::RemoveTrack { track_id } => Ok(vec![Action::RemoveTracks {
            track_ids: vec![*track_id],
        }]),
        AutomationCommand::Select {
            selection: AutomationSelection::Panel { panel_id },
        } => Ok(vec![Action::ActivatePanel {
            panel_id: *panel_id,
        }]),
        AutomationCommand::Configure { setting } => Ok(vec![Action::ApplyTrackIntents {
            intents: setting_track_intents(setting)?,
        }]),
        _ => return None,
    })
}

// ----- Alignment track settings ------------------------------------------------------------

/// The automation commands that change alignment track settings (the alignment `configure`
/// settings, `group`, `sort`, `filter`), as the actions they apply. The untyped `set_option`
/// workspace keys reach [`alignment_option_actions`] once their JSON value is read.
/// Each action carries the complete settings a track should have afterwards, so the current
/// settings are read from `session`. `None` for every other command; `Some(Err)` for a command
/// that names a missing track or an invalid value, before anything changed.
pub fn automation_alignment_settings_actions<P: Panel>(
    session: &Session<P>,
    command: &AutomationCommand,
) -> Option<Result<Vec<Action>>> {
    Some(match command {
        AutomationCommand::Configure { setting } => configure_alignment_setting(session, setting)?,
        AutomationCommand::Group {
            track_ids,
            mode,
            direction,
            tag,
            anchor_position,
        } => group_alignments(
            session,
            track_ids,
            *mode,
            *direction,
            tag.as_deref(),
            *anchor_position,
        ),
        AutomationCommand::Sort {
            track_ids,
            mode,
            direction,
            tag,
            anchor_position,
        } => sort_alignments(
            session,
            track_ids,
            *mode,
            *direction,
            tag.as_deref(),
            *anchor_position,
        ),
        AutomationCommand::Filter {
            track_ids,
            expression,
        } => filter_alignments(session, track_ids, expression),
        _ => return None,
    })
}

fn ensure_alignment_tracks<P: Panel>(session: &Session<P>, track_ids: &[TrackId]) -> Result<()> {
    for id in track_ids {
        anyhow::ensure!(
            session
                .alignment_tracks
                .iter()
                .any(|track| track.config.id == *id),
            "alignment track {} was not found",
            id.get()
        );
    }
    Ok(())
}

/// One action giving each named track `edit` applied to its current settings.
fn edit_alignment_tracks<P: Panel>(
    session: &Session<P>,
    track_ids: &[TrackId],
    mut edit: impl FnMut(&mut AlignmentSettings) -> Result<()>,
) -> Result<Vec<Action>> {
    let mut updates = Vec::with_capacity(track_ids.len());
    for track_id in track_ids {
        let mut settings = session
            .alignment_settings(*track_id)
            .ok_or_else(|| anyhow::anyhow!("alignment track {} was not found", track_id.get()))?;
        edit(&mut settings)?;
        updates.push((*track_id, settings));
    }
    Ok(vec![Action::SetAlignmentSettings {
        updates,
        workspace_limit: None,
    }])
}

/// The open alignment tracks among `track_ids`, in workspace order, once each.
fn open_alignment_tracks<P: Panel>(session: &Session<P>, track_ids: &[TrackId]) -> Vec<TrackId> {
    session
        .alignment_tracks
        .iter()
        .map(|track| track.config.id)
        .filter(|id| track_ids.contains(id))
        .collect()
}

/// One primary-alignment setting of the untyped `set_option` workspace keys (every key but
/// `show_reads`, which is a track-arrangement edit), with its value already read from JSON.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlignmentOption {
    ShowReference(bool),
    ShowCoverage(bool),
    ShowJunctions(bool),
    ShowJunctionLabels(bool),
    ShowCoverageCenterLine(bool),
    ViewAsPairs(bool),
    QualityWeightedAlleles(bool),
    ShadeReadBasesByQuality(bool),
    ShowModifiedBases(bool),
    ShowModificationFrequency(bool),
    MinimumMappingQuality(Option<u8>),
    IncludeSecondary(bool),
    IncludeSupplementary(bool),
    MinimumBaseQuality(u8),
    MinimumAlleleFraction(f32),
    /// `vertical_density`: the read-row density of one track (the primary by default).
    VerticalDensity(f32),
    CoverageScaleMaximum(Option<u32>),
    CoverageScaleTransform(CoverageScaleTransform),
    MinimumIndelSize(u32),
    MinimumEventLabelSize(u32),
}

/// A workspace alignment option as the action it applies. `alignment_track` names the track
/// `vertical_density` addresses (it must be an open alignment track); the other options and an
/// unnamed track address the primary alignment.
pub fn alignment_option_actions<P: Panel>(
    session: &Session<P>,
    option: AlignmentOption,
    alignment_track: Option<TrackId>,
) -> Result<Vec<Action>> {
    let primary = session.primary_alignment_settings_id();
    let target = match alignment_track {
        None => primary,
        Some(track_id) => {
            ensure_alignment_tracks(session, &[track_id])?;
            track_id
        }
    };
    let track = if matches!(option, AlignmentOption::VerticalDensity(_)) {
        target
    } else {
        primary
    };
    edit_alignment_tracks(session, &[track], |settings| {
        let filters = &mut settings.filters;
        match option {
            AlignmentOption::ShowReference(value) => filters.show_reference = value,
            AlignmentOption::ShowCoverage(value) => filters.show_coverage = value,
            AlignmentOption::ShowJunctions(value) => filters.show_junctions = value,
            AlignmentOption::ShowJunctionLabels(value) => filters.show_junction_labels = value,
            AlignmentOption::ShowCoverageCenterLine(value) => {
                filters.show_coverage_center_line = value;
            }
            AlignmentOption::ViewAsPairs(value) => filters.view_as_pairs = value,
            AlignmentOption::QualityWeightedAlleles(value) => {
                filters.quality_weighted_alleles = value;
            }
            AlignmentOption::ShadeReadBasesByQuality(value) => {
                filters.shade_read_bases_by_quality = value;
            }
            AlignmentOption::ShowModifiedBases(value) => filters.show_modified_bases = value,
            AlignmentOption::ShowModificationFrequency(value) => {
                filters.show_modification_frequency = value;
            }
            AlignmentOption::MinimumMappingQuality(value) => {
                filters.minimum_mapping_quality = value;
            }
            AlignmentOption::IncludeSecondary(value) => filters.include_secondary = value,
            AlignmentOption::IncludeSupplementary(value) => filters.include_supplementary = value,
            AlignmentOption::MinimumBaseQuality(value) => filters.minimum_base_quality = value,
            AlignmentOption::MinimumAlleleFraction(fraction) => {
                anyhow::ensure!(
                    (0.0..=1.0).contains(&fraction),
                    "minimum_allele_fraction must be between 0 and 1"
                );
                filters.minimum_allele_fraction = fraction;
            }
            AlignmentOption::VerticalDensity(density) => {
                anyhow::ensure!(
                    (MINIMUM_ALIGNMENT_DENSITY..=MAXIMUM_ALIGNMENT_DENSITY).contains(&density),
                    "vertical_density must be between {MINIMUM_ALIGNMENT_DENSITY} and \
                     {MAXIMUM_ALIGNMENT_DENSITY}"
                );
                filters.alignment_density = density;
            }
            AlignmentOption::CoverageScaleMaximum(maximum) => {
                anyhow::ensure!(
                    maximum != Some(0),
                    "coverage_scale_maximum must be positive or null"
                );
                filters.coverage_scale_maximum = maximum;
            }
            AlignmentOption::CoverageScaleTransform(transform) => {
                filters.coverage_scale_transform = transform;
            }
            AlignmentOption::MinimumIndelSize(minimum) => {
                anyhow::ensure!(minimum > 0, "minimum_indel_size must be positive");
                filters.minimum_indel_size = minimum;
            }
            AlignmentOption::MinimumEventLabelSize(minimum) => {
                anyhow::ensure!(minimum > 0, "minimum_event_label_size must be positive");
                filters.minimum_event_label_size = minimum;
            }
        }
        Ok(())
    })
}

/// The alignment settings of `configure`. `None` for settings of another domain.
fn configure_alignment_setting<P: Panel>(
    session: &Session<P>,
    setting: &AutomationSetting,
) -> Option<Result<Vec<Action>>> {
    let option = |option| alignment_option_actions(session, option, None);
    let primary = session.primary_alignment_settings_id();
    Some(match setting {
        AutomationSetting::ShowReference { value } => {
            option(AlignmentOption::ShowReference(*value))
        }
        AutomationSetting::ShowCoverage { value } => option(AlignmentOption::ShowCoverage(*value)),
        AutomationSetting::ShowJunctions { value } => {
            option(AlignmentOption::ShowJunctions(*value))
        }
        AutomationSetting::SashimiJunctions {
            minimum_depth,
            relative_threshold_percent,
            show_all,
            library_strandedness,
            label_mode,
            track_id,
        } => {
            let target = track_id.unwrap_or(primary);
            ensure_alignment_tracks(session, &[target]).and_then(|()| {
                edit_alignment_tracks(session, &[target], |settings| {
                    let filters = &mut settings.filters;
                    if let Some(depth) = minimum_depth {
                        anyhow::ensure!(*depth > 0, "minimum_depth must be positive");
                        filters.minimum_junction_depth = *depth;
                    }
                    if let Some(percent) = relative_threshold_percent {
                        filters.junction_relative_threshold_percent = *percent;
                    }
                    if let Some(show_all) = show_all {
                        filters.show_all_junctions = *show_all;
                    }
                    if let Some(library) = library_strandedness {
                        filters.junction_library_strandedness = *library;
                    }
                    if let Some(mode) = label_mode {
                        filters.sashimi_label_mode = *mode;
                        filters.show_junction_labels = *mode != rgv_core::SashimiLabelMode::None;
                    }
                    Ok(())
                })
            })
        }
        AutomationSetting::ViewAsPairs { value } => option(AlignmentOption::ViewAsPairs(*value)),
        AutomationSetting::IncludeSecondary { value } => {
            option(AlignmentOption::IncludeSecondary(*value))
        }
        AutomationSetting::IncludeSupplementary { value } => {
            option(AlignmentOption::IncludeSupplementary(*value))
        }
        AutomationSetting::ShadeReadBasesByQuality { value } => {
            option(AlignmentOption::ShadeReadBasesByQuality(*value))
        }
        AutomationSetting::ShowModifiedBases { value } => {
            option(AlignmentOption::ShowModifiedBases(*value))
        }
        AutomationSetting::CoverageScaleTransform { value } => {
            option(AlignmentOption::CoverageScaleTransform(*value))
        }
        AutomationSetting::AlignmentDensity { value, track_id } => {
            alignment_option_actions(session, AlignmentOption::VerticalDensity(*value), *track_id)
        }
        AutomationSetting::ReadBaseDisplayMode { value } => {
            edit_alignment_tracks(session, &[primary], |settings| {
                settings.filters.read_base_display_mode = *value;
                Ok(())
            })
        }
        AutomationSetting::OverlappingMatePolicy { value } => {
            edit_alignment_tracks(session, &[primary], |settings| {
                settings.filters.overlapping_mate_policy = *value;
                Ok(())
            })
        }
        AutomationSetting::ModifiedBaseColorMode { value } => {
            edit_alignment_tracks(session, &[primary], |settings| {
                settings.filters.modified_base_color_mode = *value;
                Ok(())
            })
        }
        AutomationSetting::AlignmentColorMode { track_ids, value } => {
            ensure_alignment_tracks(session, track_ids).and_then(|()| {
                edit_alignment_tracks(
                    session,
                    &open_alignment_tracks(session, track_ids),
                    |settings| {
                        settings.filters.alignment_color_mode = *value;
                        Ok(())
                    },
                )
            })
        }
        AutomationSetting::AlignmentViewFilter { track_ids, value } => {
            predicate_setting(session, track_ids, value, "filters", |filters, value| {
                filters.alignment_view_filter = value;
            })
        }
        AutomationSetting::AlignmentHighlightFilter { track_ids, value } => {
            predicate_setting(session, track_ids, value, "highlights", |filters, value| {
                filters.alignment_highlight_filter = value;
            })
        }
        AutomationSetting::ReadStackingMode { track_ids, value } => {
            ensure_alignment_tracks(session, track_ids).and_then(|()| {
                edit_alignment_tracks(
                    session,
                    &open_alignment_tracks(session, track_ids),
                    |settings| {
                        settings.filters.read_stacking_mode = *value;
                        Ok(())
                    },
                )
            })
        }
        AutomationSetting::CompleteReadLimit { track_ids, value } => {
            ensure_alignment_tracks(session, track_ids).and_then(|()| {
                edit_alignment_tracks(
                    session,
                    &open_alignment_tracks(session, track_ids),
                    |settings| {
                        settings.maximum_alignments = *value;
                        Ok(())
                    },
                )
            })
        }
        _ => return None,
    })
}

fn predicate_setting<P: Panel>(
    session: &Session<P>,
    track_ids: &[TrackId],
    value: &AlignmentPredicateExpression,
    noun: &str,
    set: impl Fn(&mut SessionFilters, AlignmentPredicateExpression),
) -> Result<Vec<Action>> {
    ensure_alignment_tracks(session, track_ids)?;
    anyhow::ensure!(
        session.reference_source.is_some()
            || !value
                .all
                .iter()
                .any(AlignmentViewPredicate::requires_reference),
        "reference-backed alignment {noun} require a matching reference FASTA"
    );
    edit_alignment_tracks(
        session,
        &open_alignment_tracks(session, track_ids),
        |settings| {
            set(&mut settings.filters, value.clone());
            Ok(())
        },
    )
}

/// The centre base of the focused panel, where `group` and `sort` anchor a position mode.
fn focused_centre<P: Panel>(session: &Session<P>) -> u64 {
    let interval = session.panels.active().state().viewport.interval();
    interval.start().saturating_add(interval.len() / 2)
}

fn group_alignments<P: Panel>(
    session: &Session<P>,
    track_ids: &[TrackId],
    mode: AlignmentGroupMode,
    direction: AlignmentOrderDirection,
    tag: Option<&str>,
    anchor_position: Option<u64>,
) -> Result<Vec<Action>> {
    ensure_alignment_tracks(session, track_ids)?;
    let mode = tag.map_or(Ok(mode), |tag| sam_tag(tag).map(AlignmentGroupMode::Tag))?;
    let captured_anchor = matches!(
        mode,
        AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
    )
    .then(|| anchor_position.unwrap_or_else(|| focused_centre(session)));
    edit_alignment_tracks(
        session,
        &open_alignment_tracks(session, track_ids),
        |settings| {
            settings.filters.alignment_group_mode = mode;
            settings.filters.alignment_group_order = direction;
            if captured_anchor.is_some() {
                settings.filters.alignment_sort_anchor = captured_anchor;
            }
            Ok(())
        },
    )
}

fn sort_alignments<P: Panel>(
    session: &Session<P>,
    track_ids: &[TrackId],
    mode: AlignmentSortMode,
    direction: AlignmentOrderDirection,
    tag: Option<&str>,
    anchor_position: Option<u64>,
) -> Result<Vec<Action>> {
    ensure_alignment_tracks(session, track_ids)?;
    let mode = tag.map_or(Ok(mode), |tag| sam_tag(tag).map(AlignmentSortMode::Tag))?;
    let captured_anchor = if mode == AlignmentSortMode::Base {
        anchor_position.or_else(|| {
            alignment_sort_position(mode, None, session.panels.active().state().viewport)
        })
    } else {
        None
    };
    edit_alignment_tracks(
        session,
        &open_alignment_tracks(session, track_ids),
        |settings| {
            let filters = &mut settings.filters;
            filters.alignment_sort_mode = mode;
            filters.alignment_sort_order = direction;
            if captured_anchor.is_some() {
                filters.alignment_sort_anchor = captured_anchor;
            } else if !matches!(
                filters.alignment_group_mode,
                AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
            ) {
                filters.alignment_sort_anchor = None;
            }
            Ok(())
        },
    )
}

fn filter_alignments<P: Panel>(
    session: &Session<P>,
    track_ids: &[TrackId],
    expression: &str,
) -> Result<Vec<Action>> {
    ensure_alignment_tracks(session, track_ids)?;
    let predicates = expression
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<AuxPredicate>().map(|_| part.to_owned()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    edit_alignment_tracks(
        session,
        &open_alignment_tracks(session, track_ids),
        |settings| {
            settings
                .filters
                .auxiliary_predicates
                .clone_from(&predicates);
            Ok(())
        },
    )
}

fn sam_tag(tag: &str) -> Result<[u8; 2]> {
    let bytes = tag.as_bytes();
    anyhow::ensure!(
        bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1].is_ascii_alphanumeric(),
        "tag must be a valid two-character SAM tag"
    );
    Ok([bytes[0], bytes[1]])
}

// ----- Track names and sample keys ---------------------------------------------------------

/// A `set_option` track key that renames a track or sets its sample association, with its
/// value already read from JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackLabelEdit {
    Name(String),
    SampleKey(Option<String>),
}

/// A track rename or sample association as the action it applies: an alignment track's label,
/// or an annotation or signal track's configuration.
pub fn track_label_actions<P: Panel>(
    session: &Session<P>,
    track_id: TrackId,
    edit: TrackLabelEdit,
) -> Result<Vec<Action>> {
    if let Some(mut label) = session.alignment_track_label(track_id) {
        match edit {
            TrackLabelEdit::Name(name) => label.name = name,
            TrackLabelEdit::SampleKey(sample_key) => label.sample_key = sample_key,
        }
        return Ok(vec![Action::SetAlignmentTrackLabels {
            labels: vec![label],
        }]);
    }
    let mut config = session
        .tracks
        .tracks()
        .iter()
        .find(|track| track.id == track_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("track {} was not found", track_id.get()))?;
    match edit {
        TrackLabelEdit::Name(name) => config.name = name,
        TrackLabelEdit::SampleKey(sample_key) => config.sample_key = sample_key,
    }
    Ok(vec![Action::SetTrackConfigs {
        tracks: vec![config],
    }])
}

// ----- Track load --------------------------------------------------------------------------

/// `load_track` as the action it applies, once the protocol has checked the source exists and
/// resolved its kind (the named kind, else the one its file name implies).
pub fn load_track_action(source: TrackSource, kind: TrackKind, name: Option<String>) -> Action {
    Action::LoadTracks {
        loads: vec![TrackLoad::of_kind(source, kind, name)],
    }
}

// ----- Selection ---------------------------------------------------------------------------

/// `select` of a track and `clear_selection`, as the actions they apply to the focused panel.
/// Selecting a track by protocol replaces the whole selection, as a plain click on its row
/// replaces the row selection. `None` for every other command.
pub fn automation_selection_actions<P: Panel>(
    session: &Session<P>,
    command: &AutomationCommand,
) -> Option<Result<Vec<Action>>> {
    let panel_id = session.panels.active().state().id;
    Some(match command {
        AutomationCommand::ClearSelection => Ok(vec![Action::ClearSelection { panel_id }]),
        AutomationCommand::Select {
            selection: AutomationSelection::Track { track_id },
        } => {
            let open = session
                .alignment_tracks
                .iter()
                .any(|track| track.config.id == *track_id)
                || session
                    .tracks
                    .tracks()
                    .iter()
                    .any(|track| track.id == *track_id);
            if open {
                Ok(vec![
                    Action::ClearSelection { panel_id },
                    Action::SelectTrack {
                        panel_id,
                        target: PresentationNodeId::Track {
                            channel: TrackChannel::Annotation,
                            track_id: *track_id,
                        },
                        extent: SelectionExtent::Replace,
                    },
                ])
            } else {
                Err(anyhow::anyhow!("track {} was not found", track_id.get()))
            }
        }
        _ => return None,
    })
}

/// Every automation command that is a pure action translation and needs the session to phrase
/// it: alignment settings and selection. The commands that need no session are
/// [`automation_command_actions`].
pub fn automation_session_actions<P: Panel>(
    session: &Session<P>,
    command: &AutomationCommand,
) -> Option<Result<Vec<Action>>> {
    automation_alignment_settings_actions(session, command)
        .or_else(|| automation_selection_actions(session, command))
}

// ----- Preferences -------------------------------------------------------------------------

/// The preference settings of `configure`, as the preferences they leave. `None` for every
/// other setting.
pub fn preference_setting_action<P: Panel>(
    session: &Session<P>,
    setting: &AutomationSetting,
) -> Option<Action> {
    let mut preferences = Preferences::of(&session.settings);
    match setting {
        AutomationSetting::ReadBrowsingBufferPercent { value } => {
            preferences.read_browsing_buffer_percent = *value;
        }
        AutomationSetting::TotalCacheBudgetMib { value } => preferences.cache_budget_mib = *value,
        AutomationSetting::TrackPipeline { value } => preferences.track_pipeline = *value,
        _ => return None,
    }
    Some(Action::SetPreferences(preferences))
}
