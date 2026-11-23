//! The headless half of the action model: navigation and panel management over the panel
//! workspace alone.
//!
//! Nothing here touches engines, layout or the window. A reducer validates first, then edits
//! the workspace, and reports the [`Effect`]s the host owes and the actions that would undo the
//! edit. Tests drive it directly; the host's `apply_action` drives it with the live workspace.

use super::{Action, ActionOutcome, Effect, PanelRestore, ViewportCommit};
use crate::navigation::{
    HistoryPanelContig, LociNavigationPlan, automation_viewport_after_operation,
    loci_navigation_plan, resolve_history_entry, resolve_initial_locus, standalone_locus,
};
use crate::panels::{Panel, PanelState, PanelWorkspace};
use anyhow::{Context as _, Result};
use rgv_automation::AutomationHistoryNavigation;
use rgv_core::{
    ContigAliases, GenomicInterval, MAXIMUM_VIEW_PANELS, NavigationEntry, PanelId,
    ReferenceDictionary, RegionOfInterest, ReviewItem, Viewport,
};

/// Narrowest canvas, in logical points, a panel may be given.
pub const MINIMUM_PANEL_CANVAS_WIDTH: f32 = 140.0;

/// What a panel reducer needs to know about the world outside the panel workspace.
pub struct PanelContext<'a> {
    /// The dictionary the addressed panel's coordinates are expressed in, if one is open.
    pub dictionary: Option<&'a ReferenceDictionary>,
    /// The dictionary a new panel falls back to when no source dictionary is open.
    pub fallback_dictionary: &'a ReferenceDictionary,
    pub aliases: &'a ContigAliases,
    /// Canvas width, in logical points, each panel gets when the workspace holds that many.
    pub canvas_width_for: &'a dyn Fn(usize) -> f32,
    /// Top of the track viewport, where a new panel's content starts.
    pub content_top: f32,
}

/// The result of one reduction.
#[derive(Debug, Default)]
pub struct Reduced {
    pub outcome: ActionOutcome,
    pub effects: Vec<Effect>,
    /// Actions that put the document back, in the order to apply them. Empty for navigation,
    /// which Back/Forward already covers, and for focus changes.
    pub inverse: Vec<Action>,
}

impl Reduced {
    fn unchanged() -> Self {
        Self::default()
    }

    fn merge(&mut self, next: Self) {
        self.outcome.changed |= next.outcome.changed;
        self.outcome.panel_id = next.outcome.panel_id.or(self.outcome.panel_id);
        self.outcome.interval = next.outcome.interval.or(self.outcome.interval);
        self.effects.extend(next.effects);
        // Undo runs newest-first.
        let mut inverse = next.inverse;
        inverse.append(&mut self.inverse);
        self.inverse = inverse;
    }
}

/// Whether [`reduce_panel_action`] implements the action.
pub const fn is_panel_action(action: &Action) -> bool {
    !matches!(
        action,
        Action::ApplyTrackIntents { .. }
            | Action::RestorePresentation { .. }
            | Action::RemoveTracks { .. }
            | Action::RestoreTracks(_)
            | Action::SetWorkspaceLists { .. }
            | Action::LoadTracks { .. }
            | Action::SetAlignmentSettings { .. }
            | Action::SetTrackConfigs { .. }
            | Action::SetAlignmentTrackLabels { .. }
            | Action::MoveTrack { .. }
            | Action::SetReferencePresentation(_)
            | Action::SelectTrack { .. }
            | Action::ClearSelection { .. }
            | Action::SetPreferences(_)
    )
}

fn resolve_locus(ctx: &PanelContext<'_>, locus: &str) -> Result<(GenomicInterval, String, u64)> {
    match ctx.dictionary {
        Some(dictionary) => resolve_initial_locus(dictionary, ctx.aliases, Some(locus)),
        None => standalone_locus(locus),
    }
}

fn ensure_panel<P: Panel>(panels: &PanelWorkspace<P>, panel_id: PanelId) -> Result<()> {
    anyhow::ensure!(
        panels.get(panel_id).is_some(),
        "panel {} was not found",
        panel_id.get()
    );
    Ok(())
}

/// The primitive actions a multi-locus `GoToLoci` stands for. Every locus is validated before
/// anything is returned, so a bad expression replaces no panel.
pub fn expand_go_to_loci<P: Panel>(
    panels: &PanelWorkspace<P>,
    ctx: &PanelContext<'_>,
    panel_id: PanelId,
    loci: &[String],
) -> Result<Vec<Action>> {
    ensure_panel(panels, panel_id)?;
    let first = loci.first().context("a locus is required")?;
    if loci_navigation_plan(loci.len()) == LociNavigationPlan::RetargetActivePanel {
        return Ok(vec![Action::GoToLocus {
            panel_id,
            locus: first.clone(),
        }]);
    }
    anyhow::ensure!(
        (ctx.canvas_width_for)(loci.len()) >= MINIMUM_PANEL_CANVAS_WIDTH,
        "the window is too narrow to display {} readable panels",
        loci.len()
    );
    for locus in loci {
        resolve_locus(ctx, locus)?;
    }
    let mut actions = vec![Action::GoToLocus {
        panel_id,
        locus: first.clone(),
    }];
    actions.extend(
        panels
            .iter()
            .map(|panel| panel.state().id)
            .filter(|id| *id != panel_id)
            .map(|panel_id| Action::ClosePanel { panel_id }),
    );
    actions.extend(loci.iter().skip(1).map(|locus| Action::AddPanel {
        locus: locus.clone(),
    }));
    actions.push(Action::ActivatePanel { panel_id });
    Ok(actions)
}

/// Records where `panel_id` is as one history entry and returns the locus string for the recent
/// list. This is what committing a settled gesture does.
pub fn record_panel_visit<P: Panel>(
    panels: &mut PanelWorkspace<P>,
    panel_id: PanelId,
) -> Option<String> {
    let panel = panels.get_mut(panel_id)?.state_mut();
    let interval = panel.viewport.interval();
    panel.visit_history(interval);
    Some(format!(
        "{}:{}-{}",
        panel.contig_name,
        interval.start() + 1,
        interval.end()
    ))
}

fn retarget<P: Panel>(
    panels: &mut PanelWorkspace<P>,
    panel_id: PanelId,
    interval: GenomicInterval,
    contig_name: String,
    contig_length: u64,
    viewport: Viewport,
    record: bool,
) -> Reduced {
    // Every locus a panel is sent to spans at least `MIN_VISIBLE_BASES`.
    let clamped = viewport
        .clamped_to_minimum_span(contig_length)
        .unwrap_or(viewport);
    let interval = if clamped == viewport {
        interval
    } else {
        clamped.interval()
    };
    let viewport = clamped;
    let panel = panels.panel_mut(panel_id).state_mut();
    panel.viewport = viewport;
    panel.contig_name = contig_name;
    panel.contig_length = contig_length;
    if record {
        panel.visit_history(interval);
    }
    Reduced {
        outcome: ActionOutcome {
            changed: true,
            panel_id: Some(panel_id),
            interval: Some(interval),
            ..ActionOutcome::default()
        },
        effects: vec![Effect::ViewportChanged {
            panel_id,
            commit: ViewportCommit::Discrete,
        }],
        inverse: Vec::new(),
    }
}

fn navigate_to_entry<P: Panel>(
    panels: &mut PanelWorkspace<P>,
    ctx: &PanelContext<'_>,
    panel_id: PanelId,
    entry: &NavigationEntry,
    record: bool,
) -> Result<Reduced> {
    let panel = panels.panel(panel_id).state();
    let (interval, contig_name, contig_length) = resolve_history_entry(
        entry,
        ctx.dictionary,
        ctx.aliases,
        HistoryPanelContig {
            id: panel.viewport.interval().contig(),
            name: &panel.contig_name,
            length: panel.contig_length,
        },
    )?;
    let viewport = Viewport::new(interval, panel.viewport.pixel_width())?;
    Ok(retarget(
        panels,
        panel_id,
        interval,
        contig_name,
        contig_length,
        viewport,
        record,
    ))
}

fn insert_panel<P: Panel>(
    panels: &mut PanelWorkspace<P>,
    panel: P,
    index: Option<usize>,
    focus: bool,
) -> Result<Reduced> {
    let panel_id = panel.state().id;
    let previous_focus = panels.active().state().id;
    anyhow::ensure!(panels.push(panel), "view panel identity collision");
    if let Some(index) = index {
        panels.move_panel(panel_id, index);
    }
    let mut inverse = vec![Action::ClosePanel { panel_id }];
    if focus {
        panels.activate(panel_id);
        // Closing the focused panel hands focus to a neighbour; undo returns it to where it was.
        inverse.push(Action::ActivatePanel {
            panel_id: previous_focus,
        });
    }
    Ok(Reduced {
        outcome: ActionOutcome {
            changed: true,
            panel_id: Some(panel_id),
            ..ActionOutcome::default()
        },
        effects: vec![Effect::PanelAdded {
            panel_id,
            previous_focus,
        }],
        inverse,
    })
}

/// Applies one navigation or panel-management action to the panel workspace.
///
/// `before_commit` runs once after validation and before the first edit of a discrete
/// navigation; the host uses it to turn a still-pending gesture into its own history entry so
/// the gesture's end point is not lost.
#[allow(clippy::too_many_lines)]
pub fn reduce_panel_action<P: Panel>(
    panels: &mut PanelWorkspace<P>,
    ctx: &PanelContext<'_>,
    action: &Action,
    before_commit: &mut dyn FnMut(&mut PanelWorkspace<P>),
) -> Result<Reduced> {
    match action {
        Action::GoToLocus { panel_id, locus } => {
            ensure_panel(panels, *panel_id)?;
            let (interval, contig_name, contig_length) = resolve_locus(ctx, locus)?;
            let viewport = Viewport::new(
                interval,
                panels.panel(*panel_id).state().viewport.pixel_width(),
            )?;
            before_commit(panels);
            Ok(retarget(
                panels,
                *panel_id,
                interval,
                contig_name,
                contig_length,
                viewport,
                true,
            ))
        }
        Action::GoToLoci { panel_id, loci } => {
            let mut reduced = Reduced::unchanged();
            for primitive in expand_go_to_loci(panels, ctx, *panel_id, loci)? {
                reduced.merge(reduce_panel_action(panels, ctx, &primitive, before_commit)?);
            }
            reduced.outcome.panel_id = Some(*panel_id);
            Ok(reduced)
        }
        Action::ChangeViewport {
            panel_id,
            operation,
            commit,
        } => {
            ensure_panel(panels, *panel_id)?;
            let panel = panels.panel(*panel_id).state();
            let next = automation_viewport_after_operation(
                panel.viewport,
                panel.contig_length,
                *operation,
            )?
            .clamped_to_minimum_span(panel.contig_length)?;
            let panel = panels.panel_mut(*panel_id).state_mut();
            panel.viewport = next;
            if *commit == ViewportCommit::Discrete {
                panel.visit_history(next.interval());
            }
            Ok(Reduced {
                outcome: ActionOutcome {
                    changed: true,
                    panel_id: Some(*panel_id),
                    interval: Some(next.interval()),
                    ..ActionOutcome::default()
                },
                effects: vec![Effect::ViewportChanged {
                    panel_id: *panel_id,
                    commit: *commit,
                }],
                inverse: Vec::new(),
            })
        }
        Action::SetViewport {
            panel_id,
            viewport,
            commit,
        } => {
            ensure_panel(panels, *panel_id)?;
            // Window-computed cameras (slider, overview drag, pointer gestures) obey the same
            // minimum span as typed loci.
            let viewport =
                viewport.clamped_to_minimum_span(panels.panel(*panel_id).state().contig_length)?;
            let interval = viewport.interval();
            if *commit == ViewportCommit::Discrete {
                before_commit(panels);
            }
            let panel = panels.panel_mut(*panel_id).state_mut();
            panel.viewport = viewport;
            if *commit == ViewportCommit::Discrete {
                panel.visit_history(interval);
            }
            Ok(Reduced {
                outcome: ActionOutcome {
                    changed: true,
                    panel_id: Some(*panel_id),
                    interval: Some(interval),
                    ..ActionOutcome::default()
                },
                effects: vec![Effect::ViewportChanged {
                    panel_id: *panel_id,
                    commit: *commit,
                }],
                inverse: Vec::new(),
            })
        }
        Action::RetargetPanel {
            panel_id,
            viewport,
            contig_name,
            contig_length,
        } => {
            ensure_panel(panels, *panel_id)?;
            before_commit(panels);
            Ok(retarget(
                panels,
                *panel_id,
                viewport.interval(),
                contig_name.clone(),
                *contig_length,
                *viewport,
                true,
            ))
        }
        Action::NavigateHistory { panel_id, step } => {
            ensure_panel(panels, *panel_id)?;
            // An unsettled gesture is recorded first, so Back returns to where it started.
            before_commit(panels);
            let history = &mut panels.panel_mut(*panel_id).state_mut().navigation_history;
            let entry = match step {
                AutomationHistoryNavigation::Back => history.back(),
                AutomationHistoryNavigation::Forward => history.forward(),
                AutomationHistoryNavigation::Index { index } => history.select(*index),
            }
            .cloned();
            let Some(entry) = entry else {
                // The history boundary: nothing to move to.
                return Ok(Reduced::unchanged());
            };
            navigate_to_entry(panels, ctx, *panel_id, &entry, false)
        }
        Action::NavigateHome { panel_id } => {
            ensure_panel(panels, *panel_id)?;
            before_commit(panels);
            let panel = panels.panel_mut(*panel_id).state_mut();
            let home = panel.home.clone();
            panel.navigation_history.visit(home.clone());
            navigate_to_entry(panels, ctx, *panel_id, &home, false)
        }
        Action::NavigateToEntry {
            panel_id,
            entry,
            record,
        } => {
            ensure_panel(panels, *panel_id)?;
            before_commit(panels);
            navigate_to_entry(panels, ctx, *panel_id, entry, *record)
        }
        Action::AddPanel { locus } => {
            anyhow::ensure!(
                panels.len() < MAXIMUM_VIEW_PANELS,
                "a workspace supports at most {MAXIMUM_VIEW_PANELS} view panels"
            );
            let width = (ctx.canvas_width_for)(panels.len() + 1);
            anyhow::ensure!(
                width >= MINIMUM_PANEL_CANVAS_WIDTH,
                "the window is too narrow to add another readable panel"
            );
            let dictionary = ctx.dictionary.unwrap_or(ctx.fallback_dictionary);
            let (interval, contig_name, contig_length) =
                resolve_initial_locus(dictionary, ctx.aliases, Some(locus))?;
            let viewport = Viewport::new(interval, f64::from(width.max(1.0)))?;
            let mut panel = PanelState::new(
                panels.next_id(),
                viewport,
                Some(locus.clone()),
                contig_name,
                contig_length,
            );
            panel.content_bottom_points = ctx.content_top;
            insert_panel(panels, P::from_state(panel), None, true)
        }
        Action::RestorePanel(restore) => {
            anyhow::ensure!(
                panels.len() < MAXIMUM_VIEW_PANELS,
                "a workspace supports at most {MAXIMUM_VIEW_PANELS} view panels"
            );
            anyhow::ensure!(
                panels.get(restore.panel_id).is_none(),
                "panel {} is already open",
                restore.panel_id.get()
            );
            let width = (ctx.canvas_width_for)(panels.len() + 1);
            anyhow::ensure!(
                width >= MINIMUM_PANEL_CANVAS_WIDTH,
                "the window is too narrow to add another readable panel"
            );
            let focused = panels.active().state();
            let (interval, contig_name, contig_length) = resolve_history_entry(
                &restore.locus,
                ctx.dictionary,
                ctx.aliases,
                HistoryPanelContig {
                    id: focused.viewport.interval().contig(),
                    name: &restore.locus.contig_name,
                    length: restore.contig_length,
                },
            )?;
            let viewport = Viewport::new(interval, f64::from(width.max(1.0)))?;
            let mut panel = PanelState::new(
                restore.panel_id,
                viewport,
                restore.requested_locus.clone(),
                contig_name,
                contig_length,
            );
            panel.content_bottom_points = ctx.content_top;
            insert_panel(
                panels,
                P::from_state(panel),
                Some(restore.index),
                restore.was_focused,
            )
        }
        Action::ClosePanel { panel_id } => {
            let Some(index) = panels
                .iter()
                .position(|panel| panel.state().id == *panel_id)
            else {
                return Ok(Reduced::unchanged());
            };
            if panels.len() == 1 {
                return Ok(Reduced::unchanged());
            }
            let was_focused = panels.is_active(*panel_id);
            let Some(panel) = panels.remove(*panel_id) else {
                return Ok(Reduced::unchanged());
            };
            let panel = panel.state();
            let restore = PanelRestore {
                panel_id: *panel_id,
                index,
                locus: NavigationEntry::new(panel.contig_name.clone(), panel.viewport.interval()),
                contig_length: panel.contig_length,
                requested_locus: panel.requested_locus.clone(),
                was_focused,
            };
            Ok(Reduced {
                outcome: ActionOutcome {
                    changed: true,
                    panel_id: Some(*panel_id),
                    ..ActionOutcome::default()
                },
                effects: vec![Effect::PanelClosed {
                    panel_id: *panel_id,
                }],
                inverse: vec![Action::RestorePanel(restore)],
            })
        }
        Action::MovePanel { panel_id, index } => {
            let Some(previous) = panels
                .iter()
                .position(|panel| panel.state().id == *panel_id)
            else {
                return Ok(Reduced::unchanged());
            };
            panels.move_panel(*panel_id, *index);
            let moved = panels
                .iter()
                .position(|panel| panel.state().id == *panel_id)
                != Some(previous);
            Ok(Reduced {
                outcome: ActionOutcome {
                    changed: true,
                    panel_id: Some(*panel_id),
                    ..ActionOutcome::default()
                },
                effects: vec![Effect::PanelsReordered],
                inverse: if moved {
                    vec![Action::MovePanel {
                        panel_id: *panel_id,
                        index: previous,
                    }]
                } else {
                    Vec::new()
                },
            })
        }
        Action::ActivatePanel { panel_id } => {
            let previous = panels.active().state().id;
            if !panels.activate(*panel_id) {
                return Ok(Reduced::unchanged());
            }
            Ok(Reduced {
                outcome: ActionOutcome {
                    changed: previous != *panel_id,
                    panel_id: Some(*panel_id),
                    ..ActionOutcome::default()
                },
                effects: vec![Effect::PanelActivated {
                    previous,
                    current: *panel_id,
                }],
                inverse: Vec::new(),
            })
        }
        Action::ApplyTrackIntents { .. }
        | Action::RestorePresentation { .. }
        | Action::RemoveTracks { .. }
        | Action::RestoreTracks(_)
        | Action::SetWorkspaceLists { .. }
        | Action::LoadTracks { .. }
        | Action::SetAlignmentSettings { .. }
        | Action::SetTrackConfigs { .. }
        | Action::SetAlignmentTrackLabels { .. }
        | Action::MoveTrack { .. }
        | Action::SetReferencePresentation(_)
        | Action::SelectTrack { .. }
        | Action::ClearSelection { .. }
        | Action::SetPreferences(_) => {
            anyhow::bail!("{} is not a panel action", action.label())
        }
    }
}

/// What replacing the regions of interest and review queue changed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceListChange {
    pub regions_changed: bool,
    pub review_changed: bool,
    /// The edit kept every item and its place: a rename, note, status or colour change. Such
    /// edits arrive once per keystroke and are one undo step while they keep coming.
    pub in_place: bool,
}

/// Replaces both lists and returns what changed and the action that puts the old lists back
/// (`None` when nothing changed).
pub fn reduce_workspace_lists(
    current_regions: &mut Vec<RegionOfInterest>,
    current_review: &mut Vec<ReviewItem>,
    regions: Vec<RegionOfInterest>,
    review_items: Vec<ReviewItem>,
) -> (WorkspaceListChange, Option<Action>) {
    let change = WorkspaceListChange {
        regions_changed: regions != *current_regions,
        review_changed: review_items != *current_review,
        in_place: regions
            .iter()
            .map(|region| (region.id, &region.locus))
            .eq(current_regions
                .iter()
                .map(|region| (region.id, &region.locus)))
            && review_items
                .iter()
                .map(|item| (item.id, &item.locus))
                .eq(current_review.iter().map(|item| (item.id, &item.locus))),
    };
    if !change.regions_changed && !change.review_changed {
        return (change, None);
    }
    let inverse = Action::SetWorkspaceLists {
        regions: std::mem::replace(current_regions, regions),
        review_items: std::mem::replace(current_review, review_items),
    };
    (change, Some(inverse))
}
