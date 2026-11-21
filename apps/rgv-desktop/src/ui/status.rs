use egui::{Color32, RichText, Ui};

/// The status bar with an optional error badge. Returns the status label's response and whether
/// the badge was clicked. The badge stays visible after the status text moves on, so a failure
/// is not lost to a later progress message.
pub fn status_bar_with_badge(
    ui: &mut Ui,
    track_count: usize,
    locus: &str,
    status: &str,
    selection: Option<&str>,
    badge: Option<&str>,
) -> (egui::Response, bool) {
    let mut badge_clicked = false;
    let response = ui
        .horizontal_centered(|ui| {
            if let Some(badge) = badge {
                badge_clicked = ui
                    .add(egui::Button::new(
                        RichText::new(badge)
                            .color(crate::theme::adapt(Color32::from_rgb(170, 30, 30))),
                    ))
                    .on_hover_text("Show errors and warnings")
                    .clicked();
                ui.separator();
            }
            ui.label(
                RichText::new(format!("{track_count} tracks"))
                    .color(crate::theme::adapt(Color32::from_gray(70))),
            );
            ui.separator();
            ui.monospace(locus);
            ui.separator();
            let response =
                ui.add(egui::Label::new(status).sense(egui::Sense::focusable_noninteractive()));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Label,
                    true,
                    format!("Application status: {status}"),
                )
            });
            if let Some(selection) = selection {
                ui.separator();
                ui.label(selection);
            }
            response
        })
        .inner;
    (response, badge_clicked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_badge_is_a_named_button_next_to_the_status() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            let (_, clicked) = status_bar_with_badge(
                ui,
                3,
                "chr1:101-200",
                "Loading remote index",
                None,
                Some("2 errors (1 new)"),
            );
            assert!(!clicked);
        });
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button && node.label() == Some("2 errors (1 new)")
        }));
    }

    #[test]
    fn status_is_focusable_and_named_in_the_accessibility_tree() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            let _ =
                status_bar_with_badge(ui, 3, "chr1:101-200", "Loading remote index", None, None);
        });
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Label
                && node
                    .value()
                    .is_some_and(|value| value == "Application status: Loading remote index")
        }));
    }
}
