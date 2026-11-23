//! Read-panel notices painted above the GPU reads: when reads could not be loaded (or cannot be
//! shown) the panel gets an opaque banner with a short message elided to fit, the full
//! explanation on hover and, for a failure, Retry. Stale reads under a failure are dimmed rather
//! than printed over.

use crate::Viewer;
use crate::ui::frame::ShellFrame;
use crate::workspace::panels::{ReadPanelNotice, ReadPanelNoticeKind};

/// Widest a banner grows; a wider panel centres it.
const BANNER_MAX_WIDTH: f32 = 560.0;
const BANNER_HEIGHT: f32 = 30.0;
/// Space kept between the banner and the edges of the panel's rows.
const BANNER_MARGIN: f32 = 8.0;
/// Narrowest banner worth drawing: the icon, a few letters and Retry.
const BANNER_MIN_WIDTH: f32 = 96.0;

/// Where the banner of a read panel goes in `rows`, the visible part of the panel below its
/// header: centred, inset by a margin, at most [`BANNER_MAX_WIDTH`] wide, at the top. `None`
/// when the panel is too small to hold one.
pub(crate) fn read_notice_banner_rect(rows: egui::Rect) -> Option<egui::Rect> {
    let width = (rows.width() - 2.0 * BANNER_MARGIN).min(BANNER_MAX_WIDTH);
    if width < BANNER_MIN_WIDTH || rows.height() < BANNER_HEIGHT + BANNER_MARGIN {
        return None;
    }
    Some(egui::Rect::from_min_size(
        egui::pos2(rows.center().x - width / 2.0, rows.top() + BANNER_MARGIN),
        egui::vec2(width, BANNER_HEIGHT),
    ))
}

/// The banner's hover text: the summary, the full explanation, and where else it is kept.
pub(crate) fn read_notice_tooltip(notice: &ReadPanelNotice) -> String {
    let mut text = notice.summary.clone();
    if let Some(detail) = notice
        .detail
        .as_deref()
        .filter(|detail| *detail != notice.summary)
    {
        text.push_str("\n\n");
        text.push_str(detail);
    }
    if matches!(notice.kind, ReadPanelNoticeKind::Failure { .. }) {
        text.push_str(
            "\n\nThe error is also in the error log (the error count in the status bar).",
        );
    }
    text
}

/// Fill, border and text colours of a banner. The fill is opaque: nothing drawn below shows
/// through the text.
fn banner_colors(kind: ReadPanelNoticeKind) -> (egui::Color32, egui::Color32, egui::Color32) {
    let adapt = crate::theme::adapt;
    match kind {
        ReadPanelNoticeKind::Failure { .. } => (
            adapt(egui::Color32::from_rgb(253, 236, 234)),
            adapt(egui::Color32::from_rgb(214, 130, 120)),
            adapt(egui::Color32::from_rgb(120, 28, 20)),
        ),
        ReadPanelNoticeKind::Unavailable | ReadPanelNoticeKind::Status => (
            adapt(egui::Color32::from_rgb(241, 243, 247)),
            adapt(egui::Color32::from_rgb(160, 168, 182)),
            adapt(egui::Color32::from_rgb(60, 66, 78)),
        ),
    }
}

/// Lays out one banner filling `ui`'s width: an icon, the summary elided to the room left, and
/// Retry when the notice offers it. Returns whether Retry was clicked.
pub(crate) fn show_read_notice_banner(ui: &mut egui::Ui, notice: &ReadPanelNotice) -> bool {
    let (fill, border, text) = banner_colors(notice.kind);
    let tooltip = read_notice_tooltip(notice);
    let retry = matches!(notice.kind, ReadPanelNoticeKind::Failure { retry: true });
    let mut clicked = false;
    let width = ui.available_width();
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width((width - 18.0).max(0.0));
            ui.horizontal_centered(|ui| {
                let icon = if matches!(notice.kind, ReadPanelNoticeKind::Failure { .. }) {
                    "⚠"
                } else {
                    "ℹ"
                };
                ui.label(egui::RichText::new(icon).color(text));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if retry {
                        clicked = ui
                            .small_button("Retry")
                            .on_hover_text("Load the reads of this view again")
                            .clicked();
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&notice.summary).color(text))
                                .truncate()
                                .sense(egui::Sense::hover()),
                        )
                        .on_hover_text(&tooltip);
                    });
                });
            });
        })
        .response
        .on_hover_text(&tooltip);
    clicked
}

impl Viewer {
    /// Paints the banner of every read panel whose notice is one, dimming the stale reads under
    /// a failure. A click on Retry asks for that panel's view again (applied after the frame).
    pub(super) fn paint_read_notice_banners(
        &self,
        ui: &mut egui::Ui,
        st: &mut ShellFrame,
        genomic: egui::Rect,
    ) {
        for panel in self.session.panels.iter() {
            let Some(cache) = panel.scroll_display_cache.as_ref() else {
                continue;
            };
            let column = self.panel_rect(panel.id);
            for read_panel in &cache.read_panels {
                let Some(notice) = read_panel
                    .notice
                    .as_ref()
                    .filter(|notice| notice.is_banner())
                else {
                    continue;
                };
                let rows = egui::Rect::from_min_max(
                    egui::pos2(
                        column.left(),
                        read_panel.content_top.max(read_panel.clip_top),
                    ),
                    egui::pos2(column.right(), read_panel.clip_bottom),
                )
                .intersect(genomic);
                if !rows.is_positive() {
                    continue;
                }
                // Reads drawn before the failure are not this view's: dim them.
                if read_panel.density_camera
                    && matches!(notice.kind, ReadPanelNoticeKind::Failure { .. })
                {
                    ui.painter().with_clip_rect(rows).rect_filled(
                        rows,
                        0.0,
                        crate::theme::adapt(egui::Color32::from_rgba_unmultiplied(
                            255, 255, 255, 190,
                        )),
                    );
                }
                let Some(banner) = read_notice_banner_rect(rows) else {
                    continue;
                };
                let retry = egui::Area::new(egui::Id::new((
                    "read-notice-banner",
                    panel.id.get(),
                    read_panel.track_id.get(),
                )))
                .order(egui::Order::Foreground)
                .fixed_pos(banner.min)
                .constrain(false)
                .show(ui.ctx(), |ui| {
                    ui.set_width(banner.width());
                    ui.set_max_width(banner.width());
                    show_read_notice_banner(ui, notice)
                })
                .inner;
                if retry {
                    st.read_notice_retry = Some((panel.id, read_panel.track_id));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure() -> ReadPanelNotice {
        ReadPanelNotice {
            kind: ReadPanelNoticeKind::Failure { retry: true },
            summary: "Couldn't load reads here".to_owned(),
            detail: Some(
                "alignment source work failed: alignment source backend failed: record 1 \
                 repeats an existing physical source position"
                    .to_owned(),
            ),
        }
    }

    #[test]
    fn a_banner_is_centred_inset_and_bounded_by_its_panel() {
        let wide = egui::Rect::from_min_size(egui::pos2(100.0, 200.0), egui::vec2(1_600.0, 400.0));
        let close = |left: f32, right: f32| (left - right).abs() < 1e-3;
        let banner = read_notice_banner_rect(wide).unwrap();
        assert!(close(banner.width(), BANNER_MAX_WIDTH));
        assert!(close(banner.center().x, wide.center().x));
        assert!(close(banner.top(), wide.top() + BANNER_MARGIN));
        assert!(wide.contains_rect(banner));

        // A narrow panel keeps the margins: the banner never runs past its edges.
        let narrow = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(300.0, 60.0));
        let banner = read_notice_banner_rect(narrow).unwrap();
        assert!(close(banner.left(), BANNER_MARGIN));
        assert!(close(banner.right(), 300.0 - BANNER_MARGIN));
        assert!(narrow.contains_rect(banner));

        // Too small to hold a banner at all.
        assert!(
            read_notice_banner_rect(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(100.0, 400.0)
            ))
            .is_none()
        );
        assert!(
            read_notice_banner_rect(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 20.0)
            ))
            .is_none()
        );
    }

    #[test]
    fn a_failure_banner_is_short_and_keeps_the_error_chain_on_hover() {
        let notice = failure();
        let tooltip = read_notice_tooltip(&notice);
        assert!(tooltip.starts_with("Couldn't load reads here\n\n"));
        assert!(tooltip.contains("record 1 repeats an existing physical source position"));
        assert!(tooltip.contains("error log"));

        // Unavailable notices say why directly and do not repeat it.
        let unavailable = ReadPanelNotice {
            kind: ReadPanelNoticeKind::Unavailable,
            summary: "chrM is not available in this alignment".to_owned(),
            detail: Some("chrM is not available in this alignment".to_owned()),
        };
        assert_eq!(
            read_notice_tooltip(&unavailable),
            "chrM is not available in this alignment"
        );
    }

    #[test]
    fn a_failure_banner_is_opaque_elided_and_offers_retry() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut banner_width = 0.0;
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            ui.allocate_ui(egui::vec2(220.0, BANNER_HEIGHT), |ui| {
                ui.set_max_width(220.0);
                let mut notice = failure();
                // Longer than the banner: the text is elided, not drawn past its edge.
                notice.summary = notice.detail.clone().unwrap();
                let response = ui.scope(|ui| show_read_notice_banner(ui, &notice));
                banner_width = response.response.rect.width();
                assert!(!response.inner);
            });
        });
        assert!(banner_width <= 220.0 + 0.5, "{banner_width}");
        let (fill, _, _) = banner_colors(ReadPanelNoticeKind::Failure { retry: true });
        assert_eq!(fill.a(), 255, "the banner background must be opaque");

        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button && node.label() == Some("Retry")
        }));
        // The full chain is never laid out unelided inside the banner.
        let texts = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!texts.is_empty());
        for galley in texts {
            assert!(
                galley.size().x <= 220.0,
                "text overflows: {}",
                galley.text()
            );
        }
    }
}
