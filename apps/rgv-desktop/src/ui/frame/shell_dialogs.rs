//! The unsaved-changes prompt and the notification log window.

use super::*;
use crate::app::shell_state::{Notification, NotificationSeverity, UnsavedChoice};

/// Shell prompt and notification state for one frame.
#[derive(Default)]
pub(super) struct ShellUiDraft {
    /// Question shown by the Save / Discard / Cancel prompt, when it is open.
    pub(super) unsaved_prompt: Option<String>,
    pub(super) unsaved_choice: Option<UnsavedChoice>,
    pub(super) notifications_open: bool,
    pub(super) error_count: usize,
    pub(super) unseen_error_count: usize,
    /// Newest first; populated only while the window is open.
    pub(super) notifications: Vec<Notification>,
    pub(super) dismiss_notification: Option<u64>,
    pub(super) clear_notifications: bool,
}

/// Status-bar badge text, or `None` when there is nothing to report.
pub(crate) fn notification_badge_label(errors: usize, unseen: usize) -> Option<String> {
    match (errors, unseen) {
        (0, _) => None,
        (1, 0) => Some("1 error".to_owned()),
        (errors, 0) => Some(format!("{errors} errors")),
        (1, _) => Some("1 error (new)".to_owned()),
        (errors, unseen) => Some(format!("{errors} errors ({unseen} new)")),
    }
}

const fn severity_label(severity: NotificationSeverity) -> &'static str {
    match severity {
        NotificationSeverity::Warning => "Warning",
        NotificationSeverity::Error => "Error",
    }
}

pub(super) fn show_shell_dialogs(ui: &mut egui::Ui, draft: &mut ShellUiDraft) {
    if draft.notifications_open {
        let context = ui.ctx().clone();
        let mut open = true;
        egui::Window::new("Errors and warnings")
            .id(egui::Id::new("notification-log"))
            .default_pos([320.0, 140.0])
            .default_size([560.0, 320.0])
            .collapsible(false)
            .open(&mut open)
            .show(&context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("{} entries", draft.notifications.len()));
                    if ui.button("Clear all").clicked() {
                        draft.clear_notifications = true;
                    }
                });
                ui.separator();
                let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                    ui,
                    row_height,
                    draft.notifications.len(),
                    |ui, rows| {
                        for entry in &draft.notifications[rows] {
                            ui.horizontal(|ui| {
                                if ui
                                    .small_button("Dismiss")
                                    .on_hover_text("Remove this entry")
                                    .clicked()
                                {
                                    draft.dismiss_notification = Some(entry.sequence);
                                }
                                let colour = match entry.severity {
                                    NotificationSeverity::Error => egui::Color32::from_rgb(170, 30, 30),
                                    NotificationSeverity::Warning => {
                                        egui::Color32::from_rgb(140, 95, 0)
                                    }
                                };
                                ui.colored_label(colour, severity_label(entry.severity));
                                let mut text = String::new();
                                if let Some(track) = entry.track {
                                    let _ = write!(text, "track {} · ", track.get());
                                }
                                text.push_str(&entry.message);
                                if entry.occurrences > 1 {
                                    let _ = write!(text, " (×{})", entry.occurrences);
                                }
                                ui.add(egui::Label::new(text).truncate())
                                    .on_hover_text(&entry.message);
                            });
                        }
                    },
                );
            });
        draft.notifications_open = open;
    }
    // Painted last so it stays above every other window; modal until answered.
    if let Some(question) = draft.unsaved_prompt.clone() {
        let id = egui::Id::new("unsaved-changes-prompt");
        let response = egui::Modal::new(id)
            .area(egui::Modal::default_area(id).fade_in(false))
            .show(ui.ctx(), |ui| {
                ui.heading("Unsaved changes");
                ui.label(question);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        draft.unsaved_choice = Some(UnsavedChoice::Save);
                    }
                    if ui.button("Discard").clicked() {
                        draft.unsaved_choice = Some(UnsavedChoice::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        draft.unsaved_choice = Some(UnsavedChoice::Cancel);
                    }
                });
            });
        if response.should_close() && draft.unsaved_choice.is_none() {
            draft.unsaved_choice = Some(UnsavedChoice::Cancel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_is_absent_without_errors_and_counts_new_ones() {
        assert_eq!(notification_badge_label(0, 0), None);
        assert_eq!(notification_badge_label(1, 0).as_deref(), Some("1 error"));
        assert_eq!(notification_badge_label(3, 0).as_deref(), Some("3 errors"));
        assert_eq!(
            notification_badge_label(3, 2).as_deref(),
            Some("3 errors (2 new)")
        );
    }

    #[test]
    fn unsaved_prompt_offers_save_discard_and_cancel() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut draft = ShellUiDraft {
            unsaved_prompt: Some("Save changes to this project before exiting RGV?".to_owned()),
            ..ShellUiDraft::default()
        };
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            show_shell_dialogs(ui, &mut draft);
        });
        let update = output.platform_output.accesskit_update.unwrap();
        for label in ["Save", "Discard", "Cancel"] {
            assert!(
                update.nodes.iter().any(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button && node.label() == Some(label)
                }),
                "missing {label} button"
            );
        }
        assert_eq!(draft.unsaved_choice, None, "nothing is chosen without input");
    }
}
