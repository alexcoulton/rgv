use egui::{Color32, RichText, Ui};

pub fn status_bar(
    ui: &mut Ui,
    track_count: usize,
    locus: &str,
    status: &str,
    selection: Option<&str>,
) -> egui::Response {
    ui.horizontal_centered(|ui| {
        ui.label(RichText::new(format!("{track_count} tracks")).color(Color32::from_gray(70)));
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
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_focusable_and_named_in_the_accessibility_tree() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            let _ = status_bar(ui, 3, "chr1:101-200", "Loading remote index", None);
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
