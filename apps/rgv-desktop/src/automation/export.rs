use crate::inspection::model::InspectorSnapshot;
use crate::inspection::read::{format_cigar, truncate_string_to_bytes};
use crate::inspection::rna::{AutomationAlignmentExportTrack, xml_escape};
use crate::inspection::variant::{
    bounded_variant_allele, bounded_variant_export_field, format_genotype, format_variant_type,
    tsv_cell, variant_genotype_group_label,
};
use crate::ui::alignment_settings::modification_code_label;
use crate::workspace::alignments::AlignmentTrackController;
use crate::workspace::panels::PanelWorkspace;
use crate::{
    MAX_AUTOMATION_EXPORT_BYTES, MAX_AUTOMATION_FULL_PANEL_HEIGHT, SEQUENCE_COPY_BASES, Viewer, ui,
};
use anyhow::Result;
use rgv_alignment_scene::AlleleSummaryDetail;
use rgv_automation::{AutomationSnapshotFormat, AutomationSnapshotScope};
use rgv_core::{GenomicInterval, PanelId, TrackCollection, TrackId, WorkspacePresentation};
use rgv_engine::{TrackSnapshot, TrackSnapshotData};
use rgv_hts::AlignmentRecord;
use rgv_layout::{
    Direction, RectShape, breakend_bracket_dimensions, junction_arc_stroke_width,
    read_arrow_head_width,
};
use rgv_layout::{DisplayList, RectPrimitive};
use rgv_render::{PngExportBackend, export_png_scaled_prefer_gpu};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub(crate) struct SemanticExportCapture {
    generic: DisplayList,
    panels: Vec<rgv_alignment_scene::AlignmentPanelSceneFrame>,
    interactions: HashMap<(PanelId, TrackId), rgv_alignment_scene::AlignmentInteractionState>,
    bounds: egui::Rect,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) scope_label: String,
    ledger: rgv_alignment_scene::AllocationLedger,
    /// The theme active when the frame was captured: the export draws the same palette and
    /// canvas colour as the window it was taken from.
    theme: crate::theme::DesktopTheme,
    _resources: rgv_alignment_scene::TransientResourceReservation,
}

/// Completes an exported display list for `theme`: under a dark theme the canvas colour is laid
/// underneath (the encoders start from white) and generic text authored for a light canvas is
/// recoloured the way the window recolours it. Light themes change nothing.
pub(crate) fn themed_export_display_list(
    mut display: DisplayList,
    alignment_rectangles: &[RectPrimitive],
    width: u32,
    height: u32,
    theme: &crate::theme::DesktopTheme,
) -> DisplayList {
    if theme.is_dark() {
        let adapt = |color: rgv_layout::Rgba| {
            let adapted = theme.adapt(crate::ui::alignment_settings::rgba_to_color32(color));
            let [red, green, blue, alpha] = adapted.to_srgba_unmultiplied();
            rgv_layout::Rgba::new(
                f32::from(red) / 255.0,
                f32::from(green) / 255.0,
                f32::from(blue) / 255.0,
                f32::from(alpha) / 255.0,
            )
        };
        for text in &mut display.texts {
            text.color = adapt(text.color);
        }
        let [red, green, blue, _] = theme.canvas().to_srgba_unmultiplied();
        display.rectangles.insert(
            0,
            RectPrimitive {
                x: 0.0,
                y: 0.0,
                width: width as f32,
                height: height as f32,
                color: rgv_layout::Rgba::new(
                    f32::from(red) / 255.0,
                    f32::from(green) / 255.0,
                    f32::from(blue) / 255.0,
                    1.0,
                ),
                object_id: 0,
                shape: rgv_layout::RectShape::Rectangle,
            },
        );
    }
    // Alignment geometry already carries the theme's palette colours.
    display.rectangles.extend_from_slice(alignment_rectangles);
    display
}

/// Largest pixel density an interactive export uses.
const MAXIMUM_INTERACTIVE_EXPORT_SCALE: f32 = 8.0;

/// The PNG size, in physical pixels, of a `width` x `height` point snapshot at `scale`.
#[cfg(test)]
fn scaled_snapshot_pixels(width: u32, height: u32, scale: f32) -> (u32, u32) {
    let edge = |points: u32| (f64::from(points) * f64::from(scale)).round().max(1.0) as u32;
    (edge(width), edge(height))
}

/// Pixels per point for an export the user started from the window: the display's own density,
/// so the file is as sharp as what they were looking at.
pub(crate) fn interactive_export_scale(window_scale_factor: f64) -> f32 {
    if window_scale_factor.is_finite() && window_scale_factor >= 1.0 {
        (window_scale_factor as f32).min(MAXIMUM_INTERACTIVE_EXPORT_SCALE)
    } else {
        1.0
    }
}

/// The GPU device and queue an export job may render through. Both are cheap reference-counted
/// handles, so an export worker thread owns its own pair.
pub(crate) type ExportGpu = (wgpu::Device, wgpu::Queue);

/// Writes `display` as a PNG, through the GPU when `gpu` is supplied so the pixels match the
/// screen, and through the CPU rasteriser otherwise or when the GPU export fails.
pub(crate) fn write_png_export(
    gpu: Option<&ExportGpu>,
    display: &DisplayList,
    width: u32,
    height: u32,
    scale: f32,
    path: &Path,
) -> std::io::Result<()> {
    let backend = export_png_scaled_prefer_gpu(
        gpu.map(|(device, queue)| (device, queue)),
        display,
        width,
        height,
        scale,
        path,
    )
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    if let PngExportBackend::Cpu {
        gpu_failure: Some(reason),
    } = backend
    {
        tracing::warn!(%reason, "PNG export fell back to the CPU rasteriser");
    }
    Ok(())
}

impl Viewer {
    /// The device and queue for a PNG export, or `None` while the GPU device is lost, in which
    /// case the export uses the CPU rasteriser.
    pub(crate) fn export_gpu(&self) -> Option<ExportGpu> {
        (!self.gpu_device_health.is_lost()).then(|| (self.device.clone(), self.queue.clone()))
    }
}

impl SemanticExportCapture {
    /// `gpu` is used for PNG output only; see [`write_png_export`].
    pub(crate) fn write(
        self,
        path: PathBuf,
        format: AutomationSnapshotFormat,
        metadata: &str,
        scale: f32,
        gpu: Option<ExportGpu>,
    ) -> std::io::Result<()> {
        let geometry = rgv_render::project_semantic_alignment_export(
            &self.panels,
            &self.ledger,
            &self.interactions,
            self.theme.palette(),
        )?;
        let display = cropped_display_list(&self.generic, self.bounds);
        let alignment = cropped_display_list(&geometry.display, self.bounds);
        let display = themed_export_display_list(
            display,
            &alignment.rectangles,
            self.width,
            self.height,
            &self.theme,
        );
        match format {
            AutomationSnapshotFormat::Png => write_png_export(
                gpu.as_ref(),
                &display,
                self.width,
                self.height,
                scale,
                &path,
            ),
            AutomationSnapshotFormat::Svg => std::fs::write(
                path,
                display_list_svg_export(&display, self.width, self.height, metadata),
            ),
        }
    }
}

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn capture_semantic_export(
        &self,
        scope: AutomationSnapshotScope,
    ) -> Result<SemanticExportCapture> {
        let active_only = scope != AutomationSnapshotScope::VisibleWorkspace;
        let bounds = if active_only {
            self.active_panel_rect()
        } else {
            self.workspace_layout.workspace
        };
        anyhow::ensure!(
            scope != AutomationSnapshotScope::NativeWindow,
            "native window uses composed capture"
        );
        if scope == AutomationSnapshotScope::FullActivePanel {
            let panel = self.session.panels.active();
            // We may only promise a complete scope if every lane is already
            // resident and unscrolled. Acquiring unseen pages requires a separate
            // bounded source request, never a mutation of the live UI camera.
            anyhow::ensure!(
                self.session.panels.annotation_scroll_points.abs() < f32::EPSILON
                    && panel.read_stack_scroll_points.abs() < f32::EPSILON
                    && panel.read_stack_content_bottom_points <= bounds.bottom()
                    && panel
                        .scroll_display_cache
                        .as_ref()
                        .is_some_and(
                            |cache| cache.annotation_content_bottom <= cache.annotation_clip_bottom
                        ),
                "full active panel is not fully resident in the captured viewport; export refused (no partial file was written)"
            );
        }
        let mut panels = Vec::new();
        let mut interactions = HashMap::new();
        for panel in self
            .session
            .panels
            .iter()
            .filter(|panel| !active_only || panel.id == self.session.panels.active().id)
        {
            anyhow::ensure!(
                panel.navigation_presentation.is_interactive(),
                "target viewport is still being checked"
            );
            if let Some(frame) = self
                .alignment_presented_epoch
                .as_ref()
                .and_then(|epoch| epoch.panel_frames.get(&panel.id))
            {
                for lane in frame.lanes.iter() {
                    if scope == AutomationSnapshotScope::FullActivePanel
                        && let Some(camera) = lane.read_camera
                    {
                        let extent = lane.row_activity.as_ref().ok_or_else(|| {
                            anyhow::anyhow!("full-panel row extent is unavailable")
                        })?;
                        let end = extent.active_row_end();
                        anyhow::ensure!(
                            camera.row_scroll() <= f64::EPSILON
                                && (end as f64 - camera.row_scroll()) * camera.row_height()
                                    <= camera.clip().height
                                && lane.covered_rows.start == 0
                                && lane.covered_rows.end as usize >= end,
                            "full active panel requires off-screen read pages; export refused without changing the live camera"
                        );
                    }
                    if let Some(projection) = self
                        .session
                        .alignment_tracks
                        .iter()
                        .find(|track| track.config.id == lane.track_id)
                        .and_then(|track| track.projection(panel.id))
                    {
                        interactions.insert(
                            (panel.id, lane.track_id),
                            projection.scene_interaction.without_hover(),
                        );
                    }
                    let presented = self
                        .presented_alignment_lane(panel.id, lane.track_id)
                        .ok_or_else(|| {
                            anyhow::anyhow!("alignment presentation is stale or unavailable")
                        })?;
                    anyhow::ensure!(
                        lane.visible_complete()
                            && (lane.read_camera.is_none() || presented.read_authority.is_some()),
                        "complete alignment evidence is unavailable for export"
                    );
                }
                panels.push(frame.clone());
            } else {
                anyhow::ensure!(
                    self.session.alignment_tracks.is_empty(),
                    "committed alignment presentation is unavailable"
                );
            }
        }
        let width = bounds.width().ceil().max(1.0) as u32;
        let height = bounds.height().ceil().max(1.0) as u32;
        anyhow::ensure!(
            height <= MAX_AUTOMATION_FULL_PANEL_HEIGHT,
            "export height exceeds its resource limit"
        );
        let mut generic = semantic_export_display_list(if active_only {
            &self.session.panels.active().display_list
        } else {
            &self.composite_display_list
        });
        // Read-level text is not part of any display list; take it from the presented bundle,
        // the capture the window draws, for the same panels.
        let active_panel_id = self.session.panels.active().id;
        generic
            .texts
            .extend(self.presented_alignment_label_texts(|panel_id| {
                !active_only || panel_id == active_panel_id
            }));
        let bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(16))
            .and_then(|bytes| bytes.checked_add(generic.rectangles.len().saturating_mul(1024)))
            .and_then(|bytes| {
                bytes.checked_add(
                    generic
                        .texts
                        .iter()
                        .map(|text| {
                            std::mem::size_of::<rgv_layout::TextPrimitive>() * 3
                                + text.text.len() * 12
                        })
                        .sum::<usize>(),
                )
            })
            .ok_or_else(|| anyhow::anyhow!("export allocation overflow"))?;
        let resources = self.session.alignment_scene_ledger.reserve_transient(
            rgv_alignment_scene::TransientResourceClass::CpuProjection,
            bytes,
            rgv_alignment_scene::ReservationKind::Normal,
        )?;
        Ok(SemanticExportCapture {
            interactions,
            ledger: self.session.alignment_scene_ledger.clone(),
            theme: crate::theme::active(),
            generic,
            panels,
            bounds,
            width,
            height,
            scope_label: match scope {
                AutomationSnapshotScope::VisibleWorkspace => format!(
                    "visible workspace; committed layout revision {}",
                    self.workspace_layout.revision
                ),
                AutomationSnapshotScope::FullActivePanel => {
                    "full active panel; complete resident scope".into()
                }
                _ => "active panel".into(),
            },
            _resources: resources,
        })
    }
}

const fn is_transient_sashimi_hover(shape: RectShape) -> bool {
    matches!(
        shape,
        RectShape::JunctionArcHoverUp | RectShape::JunctionArcHoverDown
    )
}

pub(crate) fn semantic_export_display_list(display: &DisplayList) -> DisplayList {
    DisplayList {
        rectangles: display
            .rectangles
            .iter()
            .filter(|rectangle| !is_transient_sashimi_hover(rectangle.shape))
            .copied()
            .collect(),
        texts: display.texts.clone(),
    }
}

pub(crate) fn cropped_display_list(display: &DisplayList, bounds: egui::Rect) -> DisplayList {
    let mut cropped = DisplayList::default();
    for rectangle in &display.rectangles {
        if is_transient_sashimi_hover(rectangle.shape) {
            continue;
        }
        let left = rectangle.x.max(bounds.left());
        let top = rectangle.y.max(bounds.top());
        let right = (rectangle.x + rectangle.width).min(bounds.right());
        let bottom = (rectangle.y + rectangle.height).min(bounds.bottom());
        if right > left && bottom > top {
            let clipped = top > rectangle.y
                || bottom < rectangle.y + rectangle.height
                || left > rectangle.x
                || right < rectangle.x + rectangle.width;
            if clipped && rectangle.shape.is_junction_arc() {
                continue;
            }
            cropped.rectangles.push(RectPrimitive {
                x: left - bounds.left(),
                y: top - bounds.top(),
                width: right - left,
                height: bottom - top,
                color: rectangle.color,
                object_id: rectangle.object_id,
                shape: if top > rectangle.y
                    || bottom < rectangle.y + rectangle.height
                    || matches!(rectangle.shape.direction(), Some(Direction::Left))
                        && left > rectangle.x
                    || matches!(rectangle.shape.direction(), Some(Direction::Right))
                        && right < rectangle.x + rectangle.width
                {
                    RectShape::Rectangle
                } else {
                    rectangle.shape
                },
            });
        }
    }
    cropped.texts.extend(
        display
            .texts
            .iter()
            .filter(|text| {
                text.x >= bounds.left()
                    && text.x < bounds.right()
                    && text.y >= bounds.top()
                    && text.y < bounds.bottom()
            })
            .cloned()
            .map(|mut text| {
                text.x -= bounds.left();
                text.y -= bounds.top();
                text
            }),
    );
    cropped
}

pub(crate) fn automation_snapshot_metadata(
    assembly_name: &str,
    panels: &PanelWorkspace,
    alignment_tracks: &[AlignmentTrackController],
    tracks: &TrackCollection,
    presentation: &WorkspacePresentation,
    scope: &str,
) -> String {
    let mut metadata = format!(
        "RGV {} snapshot; scope {scope}; genome {}; ",
        env!("CARGO_PKG_VERSION"),
        sanitized_single_line(assembly_name, 4_096),
    );
    metadata.push_str("loci ");
    for (index, panel) in panels.iter().enumerate() {
        if index > 0 {
            metadata.push_str(", ");
        }
        let interval = panel.viewport.interval();
        let _ = write!(
            metadata,
            "{}:{}-{}",
            sanitized_single_line(&panel.contig_name, 4_096),
            interval.start().saturating_add(1),
            interval.end()
        );
    }
    metadata.push_str("; visible tracks ");
    let mut first = true;
    for (id, name) in alignment_tracks
        .iter()
        .filter(|track| {
            presentation
                .track_content_exposed(ui::TrackPane::Annotation, track.config.id)
                .unwrap_or(false)
                || presentation
                    .track_content_exposed(ui::TrackPane::Reads, track.config.id)
                    .unwrap_or(false)
        })
        .map(|track| (track.config.id, track.config.name.as_str()))
        .chain(
            tracks
                .tracks()
                .iter()
                .filter(|track| {
                    presentation
                        .track_content_exposed(ui::TrackPane::Annotation, track.id)
                        .unwrap_or(false)
                })
                .map(|track| (track.id, track.name.as_str())),
        )
    {
        if !first {
            metadata.push_str(", ");
        }
        first = false;
        let _ = write!(
            metadata,
            "{}:{}",
            id.get(),
            sanitized_single_line(name, 4_096)
        );
    }
    metadata
}

pub(crate) fn display_list_svg_export(
    display: &DisplayList,
    width: u32,
    height: u32,
    metadata: &str,
) -> String {
    let mut output = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n<metadata>{}</metadata>\n<rect width=\"100%\" height=\"100%\" fill=\"white\"/>\n",
        xml_escape(metadata),
    );
    let mut truncated = false;
    for rectangle in &display.rectangles {
        if is_transient_sashimi_hover(rectangle.shape) {
            continue;
        }
        let red = (rectangle.color.red.clamp(0.0, 1.0) * 255.0).round() as u8;
        let green = (rectangle.color.green.clamp(0.0, 1.0) * 255.0).round() as u8;
        let blue = (rectangle.color.blue.clamp(0.0, 1.0) * 255.0).round() as u8;
        let opacity = rectangle.color.alpha.clamp(0.0, 1.0);
        let element = if rectangle.shape.is_junction_arc() {
            let left = rectangle.x;
            let right = rectangle.x + rectangle.width;
            let top = rectangle.y;
            let bottom = rectangle.y + rectangle.height;
            let shoulder = rectangle.width * 0.18;
            let stroke_width = junction_arc_stroke_width(rectangle.height);
            let (start_y, control_y, end_y) = if rectangle.shape.is_junction_arc_up() {
                (
                    bottom - stroke_width * 0.5,
                    top + stroke_width * 0.5,
                    bottom - stroke_width * 0.5,
                )
            } else {
                (
                    top + stroke_width * 0.5,
                    bottom - stroke_width * 0.5,
                    top + stroke_width * 0.5,
                )
            };
            format!(
                "<path d=\"M {left:.2} {start_y:.2} C {:.2} {control_y:.2}, {:.2} {control_y:.2}, {right:.2} {end_y:.2}\" fill=\"none\" stroke=\"rgb({red},{green},{blue})\" stroke-opacity=\"{opacity:.3}\" stroke-width=\"{stroke_width:.2}\" stroke-linecap=\"round\"/>\n",
                left + shoulder,
                right - shoulder,
            )
        } else if matches!(
            rectangle.shape,
            RectShape::BreakendJoinBefore | RectShape::BreakendJoinAfter
        ) {
            let (bar_width, stem_half_height) =
                breakend_bracket_dimensions(rectangle.width, rectangle.height);
            let bar_x = if rectangle.shape == RectShape::BreakendJoinBefore {
                rectangle.x
            } else {
                rectangle.x + rectangle.width - bar_width
            };
            let stem_y = rectangle.y + rectangle.height * 0.5 - stem_half_height;
            format!(
                "<rect x=\"{bar_x:.2}\" y=\"{:.2}\" width=\"{bar_width:.2}\" height=\"{:.2}\" fill=\"rgb({red},{green},{blue})\" fill-opacity=\"{opacity:.3}\"/>\n<rect x=\"{:.2}\" y=\"{stem_y:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"rgb({red},{green},{blue})\" fill-opacity=\"{opacity:.3}\"/>\n",
                rectangle.y,
                rectangle.height,
                rectangle.x,
                rectangle.width,
                stem_half_height * 2.0,
            )
        } else if let Some(direction) = rectangle.shape.direction() {
            let left = rectangle.x;
            let right = rectangle.x + rectangle.width;
            let top = rectangle.y;
            let bottom = rectangle.y + rectangle.height;
            let middle = rectangle.y + rectangle.height * 0.5;
            let head_width = if matches!(
                rectangle.shape,
                RectShape::DirectionalTerminalLeft | RectShape::DirectionalTerminalRight
            ) {
                read_arrow_head_width(rectangle.width, rectangle.height)
            } else {
                rectangle.width
            };
            let points = match direction {
                Direction::Left => format!(
                    "{left:.2},{middle:.2} {:.2},{top:.2} {right:.2},{top:.2} {right:.2},{bottom:.2} {:.2},{bottom:.2}",
                    left + head_width,
                    left + head_width,
                ),
                Direction::Right => format!(
                    "{left:.2},{top:.2} {:.2},{top:.2} {right:.2},{middle:.2} {:.2},{bottom:.2} {left:.2},{bottom:.2}",
                    right - head_width,
                    right - head_width,
                ),
            };
            format!(
                "<polygon points=\"{points}\" fill=\"rgb({red},{green},{blue})\" fill-opacity=\"{opacity:.3}\"/>\n"
            )
        } else {
            format!(
                "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"rgb({red},{green},{blue})\" fill-opacity=\"{opacity:.3}\"/>\n",
                rectangle.x, rectangle.y, rectangle.width, rectangle.height,
            )
        };
        if output
            .len()
            .saturating_add(element.len())
            .saturating_add(64)
            > MAX_AUTOMATION_EXPORT_BYTES
        {
            truncated = true;
            break;
        }
        output.push_str(&element);
    }
    if !truncated {
        for text in &display.texts {
            let red = (text.color.red.clamp(0.0, 1.0) * 255.0).round() as u8;
            let green = (text.color.green.clamp(0.0, 1.0) * 255.0).round() as u8;
            let blue = (text.color.blue.clamp(0.0, 1.0) * 255.0).round() as u8;
            let element = format!(
                "<text x=\"{:.2}\" y=\"{:.2}\" text-anchor=\"middle\" dominant-baseline=\"middle\" font-family=\"monospace\" font-size=\"{:.2}\" fill=\"rgb({red},{green},{blue})\">{}</text>\n",
                text.x,
                text.y,
                text.font_size,
                xml_escape(&text.text),
            );
            if output
                .len()
                .saturating_add(element.len())
                .saturating_add(64)
                > MAX_AUTOMATION_EXPORT_BYTES
            {
                truncated = true;
                break;
            }
            output.push_str(&element);
        }
    }
    if truncated {
        output.push_str("<!-- export truncated at the declared byte limit -->\n");
    }
    output.push_str("</svg>\n");
    output
}

pub(crate) fn automation_track_names_export(
    assembly_name: &str,
    panels: &PanelWorkspace,
    alignment_tracks: &[AlignmentTrackController],
    tracks: &TrackCollection,
    presentation: &WorkspacePresentation,
) -> String {
    let mut output = format!(
        "# RGV {} track-name export\n# genome\t{}\n# panel_count\t{}\ntrack_id\tkind\tname\tsample_key\tvisible\n",
        env!("CARGO_PKG_VERSION"),
        sanitized_tsv_field(assembly_name, 4_096),
        panels.len(),
    );
    for track in alignment_tracks {
        let _ = writeln!(
            output,
            "{}\talignment\t{}\t{}\t{}",
            track.config.id.get(),
            sanitized_tsv_field(&track.config.name, 4_096),
            track
                .config
                .sample_key
                .as_deref()
                .map_or_else(String::new, |value| sanitized_tsv_field(value, 4_096)),
            presentation
                .track_content_exposed(ui::TrackPane::Annotation, track.config.id)
                .unwrap_or(false),
        );
    }
    for track in tracks.tracks() {
        let _ = writeln!(
            output,
            "{}\t{}\t{}\t{}\t{}",
            track.id.get(),
            format!("{:?}", track.kind).to_ascii_lowercase(),
            sanitized_tsv_field(&track.name, 4_096),
            track
                .sample_key
                .as_deref()
                .map_or_else(String::new, |value| sanitized_tsv_field(value, 4_096)),
            presentation
                .track_content_exposed(ui::TrackPane::Annotation, track.id)
                .unwrap_or(false),
        );
    }
    output
}

pub(crate) fn automation_export_header(
    kind: &str,
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
) -> String {
    format!(
        "# RGV {} {kind} export v1\n# genome\t{}\n# locus\t{}:{}-{}\n# coordinates\tstart is one-based; end is one-based inclusive; internal intervals are zero-based half-open\n# byte_limit\t{MAX_AUTOMATION_EXPORT_BYTES}\n",
        env!("CARGO_PKG_VERSION"),
        sanitized_tsv_field(assembly, 4_096),
        sanitized_tsv_field(contig, 4_096),
        interval.start().saturating_add(1),
        interval.end(),
    )
}

pub(crate) fn push_automation_export_line(output: &mut String, line: &str) -> bool {
    let required = line.len().saturating_add(1);
    if output.len().saturating_add(required) <= MAX_AUTOMATION_EXPORT_BYTES {
        output.push_str(line);
        output.push('\n');
        return true;
    }
    let marker = "# export_truncated\tbyte_limit_reached\n";
    if output.len().saturating_add(marker.len()) <= MAX_AUTOMATION_EXPORT_BYTES {
        output.push_str(marker);
    }
    false
}

pub(crate) fn automation_alignments_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    tracks: &[AutomationAlignmentExportTrack],
    eligible: bool,
) -> Result<String> {
    let export_scope = if eligible { "eligible" } else { "filtered" };
    let mut output = automation_export_header(
        &format!("alignment ({export_scope} view)"),
        assembly,
        contig,
        interval,
    );
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tread_name\tflags\tmapq\tcigar\tmate_contig_id\tmate_start\ttemplate_length",
    ) {
        return Ok(output);
    }
    for track in tracks {
        let snapshot = &track.authority.snapshot;
        let exported_records = if eligible {
            snapshot.reads().evidence().select_eligible(interval)?.len()
        } else {
            snapshot.reads().evidence().select(interval)?.len()
        };
        let prepared = snapshot.prepared_interval();
        let provenance = if eligible {
            snapshot
                .reads()
                .evidence()
                .eligible_coverage()
                .exact_provenance(prepared)
        } else {
            snapshot.summaries().filter_provenance(prepared).ok()
        };
        let view_filter = snapshot.reads().evidence().view_filter();
        let predicate_matches = view_filter
            .predicate_matches()
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let unavailable = || ".".to_owned();
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\tauthority=committed_scene\texport_scope={}\tread_presentation=complete\tcomplete_read_limit={}\tprepared_start={}\tprepared_end={}\texported_records={}\tfilter_fingerprint={}\tfilter_matching_records={}\tfilter_eligible_records={}\tpredicate_matches={}\tfilter_provenance_exact={}\trecords_seen={}\trecords_eligible={}\tunmapped_ignored={}\tsecondary_filtered={}\tsupplementary_filtered={}\tuser_filtered={}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                export_scope,
                track.complete_read_limit,
                prepared.start().saturating_add(1),
                prepared.end(),
                exported_records,
                view_filter.fingerprint().get(),
                view_filter.matching_records(),
                view_filter.eligible_records(),
                predicate_matches,
                provenance.is_some(),
                provenance.map_or_else(unavailable, |value| value.records_seen.to_string()),
                provenance.map_or_else(
                    || snapshot.reads().evidence().record_count().to_string(),
                    |value| value.eligible_records.to_string(),
                ),
                provenance.map_or_else(unavailable, |value| value.unmapped.to_string()),
                provenance.map_or_else(unavailable, |value| value.secondary.to_string()),
                provenance.map_or_else(unavailable, |value| value.supplementary.to_string()),
                provenance.map_or_else(unavailable, |value| value.user_filter.to_string()),
            ),
        );
        let mut output_complete = true;
        let mut visit = |exported: rgv_alignment_scene::AlignmentExportRecord| {
            output_complete = push_automation_export_line(
                &mut output,
                &automation_alignment_record_line(track, contig, &exported.record),
            );
            output_complete
        };
        if eligible {
            snapshot
                .reads()
                .for_each_eligible_full(interval, &mut visit)?;
        } else {
            snapshot.reads().for_each_full(interval, &mut visit)?;
        }
        if !output_complete {
            return Ok(output);
        }
    }
    Ok(output)
}

fn automation_alignment_record_line(
    track: &AutomationAlignmentExportTrack,
    contig: &str,
    record: &AlignmentRecord,
) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t0x{:04x}\t{}\t{}\t{}\t{}\t{}",
        track.id.get(),
        sanitized_tsv_field(&track.name, 4_096),
        sanitized_tsv_field(contig, 4_096),
        record.start.saturating_add(1),
        record.end,
        sanitized_tsv_field(&String::from_utf8_lossy(&record.read_name), 1_024),
        record.flags.bits(),
        record
            .mapping_quality
            .map_or_else(|| ".".to_owned(), |value| value.to_string()),
        format_cigar(&record.cigar),
        record
            .mate_contig
            .map_or_else(|| ".".to_owned(), |value| value.get().to_string()),
        record.mate_start.map_or_else(
            || ".".to_owned(),
            |value| value.saturating_add(1).to_string()
        ),
        record.template_length,
    )
}

pub(crate) fn automation_pileup_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    tracks: &[AutomationAlignmentExportTrack],
) -> Result<String> {
    let mut output = automation_export_header("pileup", assembly, contig, interval);
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tdepth\tforward_depth\treverse_depth\tA\tC\tG\tT\tN\tinsertion\tdeletion\tmean_base_quality\tmean_mapping_quality",
    ) {
        return Ok(output);
    }
    for track in tracks {
        let snapshot = &track.authority.snapshot;
        snapshot.summaries().project_depth_bins(interval, 1)?;
        let contract = snapshot.summaries().contract();
        let allele_available = contract.allele_detail != AlleleSummaryDetail::None;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\tauthority=committed_scene\tresolution=single_base\tzero_depth_sites=omitted\teligible_records={}\tall_eligible_records=true\talleles_available={}\toverlap_collapse_complete=true\toverlapping_mate_policy={:?}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                snapshot.reads().evidence().record_count(),
                allele_available,
                contract.overlapping_mate_policy,
            ),
        );
        for position in interval.start()..interval.end() {
            let Some(evidence) = snapshot.summaries().pileup_site_evidence_at(position) else {
                continue;
            };
            if evidence.bin.depth == 0 && evidence.bin.alleles == rgv_hts::AlleleSupport::default()
            {
                continue;
            }
            let site = GenomicInterval::new(interval.contig(), position, position + 1)?;
            if !push_automation_export_line(
                &mut output,
                &automation_pileup_line(track, contig, site, evidence.bin),
            ) {
                return Ok(output);
            }
        }
    }
    Ok(output)
}

fn automation_pileup_line(
    track: &AutomationAlignmentExportTrack,
    contig: &str,
    interval: GenomicInterval,
    bin: rgv_hts::PileupBin,
) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        track.id.get(),
        sanitized_tsv_field(&track.name, 4_096),
        sanitized_tsv_field(contig, 4_096),
        interval.start().saturating_add(1),
        interval.end(),
        bin.depth,
        bin.forward_depth,
        bin.reverse_depth,
        bin.alleles.a,
        bin.alleles.c,
        bin.alleles.g,
        bin.alleles.t,
        bin.alleles.n,
        bin.alleles.insertion,
        bin.alleles.deletion,
        bin.mean_base_quality()
            .map_or_else(|| ".".to_owned(), |value| format!("{value:.4}")),
        bin.mean_mapping_quality()
            .map_or_else(|| ".".to_owned(), |value| format!("{value:.4}")),
    )
}

pub(crate) fn automation_modifications_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    tracks: &[AutomationAlignmentExportTrack],
) -> Result<String> {
    let mut output = automation_export_header("modified-base", assembly, contig, interval);
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tcanonical_base\tmodification_code\tmodification_strand\timplicit_mode\thaplotype\tmodified_calls\tcanonical_observations\tforward_modified_calls\treverse_modified_calls\traw_fraction\tweighted_fraction\tmean_probability\tmissing_probability_calls",
    ) {
        return Ok(output);
    }
    for track in tracks {
        let snapshot = &track.authority.snapshot;
        anyhow::ensure!(
            snapshot.summaries().contract().collect_modifications,
            "modified-base summaries are unavailable in the committed scene; enable modified-base frequency and allow the scene to settle before exporting"
        );
        let summaries = snapshot.summaries().modification_summaries(interval)?;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\tauthority=committed_scene\tresolution=single_base\ttile_count={}\tall_eligible_records=true\tmodifications_enabled={}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                summaries.len(),
                true,
            ),
        );
        for (owner, summary) in summaries {
            let provenance = summary.provenance;
            let _ = push_automation_export_line(
                &mut output,
                &format!(
                    "# tile_provenance\t{}\t{}\t{}\t{}\teligible_records={}\ttagged_records={}\tdecoded_calls={}\tmalformed_records={}\tunavailable_records={}\tdropped_series={}\tall_eligible_records={}",
                    track.id.get(),
                    sanitized_tsv_field(&track.name, 4_096),
                    owner.start().saturating_add(1),
                    owner.end(),
                    provenance.eligible_records,
                    provenance.records_with_modification_tags,
                    provenance.decoded_calls,
                    provenance.malformed_records,
                    provenance.unavailable_records,
                    provenance.dropped_series,
                    provenance.all_eligible_records,
                ),
            );
            if !append_automation_modification_summary(
                &mut output,
                track,
                contig,
                interval,
                owner,
                summary,
            ) {
                return Ok(output);
            }
        }
    }
    Ok(output)
}

fn append_automation_modification_summary(
    output: &mut String,
    track: &AutomationAlignmentExportTrack,
    contig: &str,
    requested: GenomicInterval,
    owner: GenomicInterval,
    summary: &rgv_hts::ModificationSummary,
) -> bool {
    for series in &summary.series {
        for (index, bin) in series.bins.iter().copied().enumerate() {
            if bin.canonical_observations == 0
                && bin.modified_calls == 0
                && bin.missing_probability_calls == 0
            {
                continue;
            }
            let Some(bin_interval) = summary.bin_interval(owner, index) else {
                continue;
            };
            if !bin_interval.overlaps(requested) {
                continue;
            }
            let line = format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                sanitized_tsv_field(contig, 4_096),
                bin_interval.start().saturating_add(1),
                bin_interval.end(),
                char::from(series.key.canonical_base),
                modification_code_label(series.key.code),
                series.key.modification_strand,
                series.key.implicit_mode,
                series
                    .key
                    .haplotype
                    .map_or_else(|| ".".to_owned(), |value| value.to_string()),
                bin.modified_calls,
                bin.canonical_observations,
                bin.forward_modified_calls,
                bin.reverse_modified_calls,
                bin.raw_fraction()
                    .map_or_else(|| ".".to_owned(), |value| format!("{value:.6}")),
                bin.weighted_fraction()
                    .map_or_else(|| ".".to_owned(), |value| format!("{value:.6}")),
                bin.mean_probability()
                    .map_or_else(|| ".".to_owned(), |value| format!("{value:.4}")),
                bin.missing_probability_calls,
            );
            if !push_automation_export_line(output, &line) {
                return false;
            }
        }
    }
    true
}

pub(crate) fn automation_junctions_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    tracks: &[AutomationAlignmentExportTrack],
) -> Result<String> {
    let mut output = automation_export_header("junction", assembly, contig, interval);
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tdepth\tminimum_left_flank\tminimum_right_flank\tmaximum_left_flank\tmaximum_right_flank\tpositive_depth\tnegative_depth\tunknown_depth\ttagged_positive_depth\ttagged_negative_depth\tfallback_positive_depth\tfallback_negative_depth\tinvalid_strand_depth",
    ) {
        return Ok(output);
    }
    for track in tracks {
        let snapshot = &track.authority.snapshot;
        let junctions = snapshot.summaries().junctions(interval)?;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\tauthority=committed_scene\texact_events=true\tjunction_count={}\tminimum_flank={}\tall_eligible_records=true",
                track.id.get(),
                sanitized_tsv_field(&track.name, 4_096),
                junctions.len(),
                snapshot.summaries().contract().minimum_junction_flank,
            ),
        );
        for junction in junctions {
            if !push_automation_export_line(
                &mut output,
                &automation_junction_line(track, contig, &junction),
            ) {
                return Ok(output);
            }
        }
    }
    Ok(output)
}

fn automation_junction_line(
    track: &AutomationAlignmentExportTrack,
    contig: &str,
    junction: &rgv_hts::JunctionEvidence,
) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        track.id.get(),
        sanitized_tsv_field(&track.name, 4_096),
        sanitized_tsv_field(contig, 4_096),
        junction.start.saturating_add(1),
        junction.end,
        junction.depth,
        junction.minimum_left_flank,
        junction.minimum_right_flank,
        junction.maximum_left_flank,
        junction.maximum_right_flank,
        junction.positive_depth,
        junction.negative_depth,
        junction.unknown_depth,
        junction.tagged_positive_depth,
        junction.tagged_negative_depth,
        junction.fallback_positive_depth,
        junction.fallback_negative_depth,
        junction.invalid_strand_depth,
    )
}

#[allow(clippy::too_many_lines)]
pub(crate) fn automation_variants_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    snapshots: &[TrackSnapshot],
) -> String {
    let mut output = automation_export_header("variant", assembly, contig, interval);
    for line in [
        "record\ttrack_id\ttrack_name\tcontig\tstart\tend\tid\tref\talt\tqual\tfilters\ttypes",
        "info\ttrack_id\trecord_ordinal\tkey\tvalue\tfield_truncated",
        "genotype\ttrack_id\trecord_ordinal\tsample\tgenotype\tgroup\tformat_fields",
    ] {
        if !push_automation_export_line(&mut output, line) {
            return output;
        }
    }
    for snapshot in snapshots {
        let TrackSnapshotData::Variants(query) = &snapshot.data else {
            continue;
        };
        let provenance = query.provenance;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\trecords_examined={}\trecords_filtered={}\trecords_truncated={}\ttotal_samples={}\tselected_samples={}\tgenotype_cells_omitted={}",
                snapshot.id.get(),
                sanitized_tsv_field(&snapshot.name, 4_096),
                provenance.records_examined,
                provenance.records_filtered,
                provenance.records_truncated,
                provenance.total_samples,
                provenance.selected_samples,
                provenance.genotype_cells_omitted,
            ),
        );
        for (ordinal, record) in query.records.iter().enumerate() {
            if record.start >= interval.end() || record.end <= interval.start() {
                continue;
            }
            let filters = if record.filters.is_empty() {
                "PASS".to_owned()
            } else {
                record.filters.join(";")
            };
            let line = format!(
                "record\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                snapshot.id.get(),
                sanitized_tsv_field(&snapshot.name, 4_096),
                sanitized_tsv_field(&record.contig, 4_096),
                record.start.saturating_add(1),
                record.end,
                tsv_cell(record.id.as_deref().unwrap_or(".")),
                tsv_cell(&bounded_variant_allele(&record.reference)),
                tsv_cell(
                    &record
                        .alternates
                        .iter()
                        .map(|allele| bounded_variant_allele(allele))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                record
                    .quality
                    .map_or_else(|| ".".to_owned(), |value| value.to_string()),
                tsv_cell(&filters),
                tsv_cell(
                    &record
                        .variant_types
                        .iter()
                        .map(format_variant_type)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            );
            if !push_automation_export_line(&mut output, &line) {
                return output;
            }
            for field in &record.info {
                let value = bounded_variant_export_field(field);
                let line = format!(
                    "info\t{}\t{}\t{}\t{}\t{}",
                    snapshot.id.get(),
                    ordinal,
                    tsv_cell(&field.key),
                    tsv_cell(&value),
                    field.truncated || value.ends_with('…'),
                );
                if !push_automation_export_line(&mut output, &line) {
                    return output;
                }
            }
            for genotype in &record.genotypes {
                let sample = query
                    .header
                    .samples
                    .get(genotype.sample_index)
                    .map_or("?", String::as_str);
                let format_fields = genotype
                    .fields
                    .iter()
                    .map(|field| format!("{}={}", field.key, bounded_variant_export_field(field)))
                    .collect::<Vec<_>>()
                    .join(";");
                let line = format!(
                    "genotype\t{}\t{}\t{}\t{}\t{}\t{}",
                    snapshot.id.get(),
                    ordinal,
                    tsv_cell(sample),
                    tsv_cell(&format_genotype(genotype)),
                    variant_genotype_group_label(genotype),
                    tsv_cell(&format_fields),
                );
                if !push_automation_export_line(&mut output, &line) {
                    return output;
                }
            }
        }
    }
    output
}

pub(crate) fn automation_quantitative_export(
    assembly: &str,
    contig: &str,
    interval: GenomicInterval,
    snapshots: &[TrackSnapshot],
) -> String {
    let mut output = automation_export_header("quantitative-window", assembly, contig, interval);
    if !push_automation_export_line(
        &mut output,
        "track_id\ttrack_name\tcontig\tstart\tend\tvalue",
    ) {
        return output;
    }
    for snapshot in snapshots {
        let TrackSnapshotData::Quantitative(query) = &snapshot.data else {
            continue;
        };
        let provenance = query.provenance;
        let _ = push_automation_export_line(
            &mut output,
            &format!(
                "# track_provenance\t{}\t{}\trecords_examined={}\trecords_retained={}\tnon_finite_records={}\trecords_truncated={}",
                snapshot.id.get(),
                sanitized_tsv_field(&snapshot.name, 4_096),
                provenance.records_examined,
                provenance.records_retained,
                provenance.non_finite_records,
                provenance.records_truncated,
            ),
        );
        if let Some(rgv_hts::QuantitativeSourceMetadata::Tdf(metadata)) = &query.source_metadata {
            let _ = push_automation_export_line(
                &mut output,
                &format!(
                    "# tdf_source\t{}\tchannel={}\tchannel_index={}\tchannel_count={}\tdataset={}\tversion={}\tgenome={}\tcompressed={}",
                    snapshot.id.get(),
                    tsv_cell(&metadata.selected_track_name),
                    metadata.selected_track_index,
                    metadata.track_count,
                    tsv_cell(&metadata.dataset),
                    metadata.version,
                    tsv_cell(metadata.genome_id.as_deref().unwrap_or(".")),
                    metadata.compressed,
                ),
            );
        }
        for record in &query.records {
            if record.start >= interval.end() || record.end <= interval.start() {
                continue;
            }
            let line = format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                snapshot.id.get(),
                sanitized_tsv_field(&snapshot.name, 4_096),
                sanitized_tsv_field(contig, 4_096),
                record.start.saturating_add(1),
                record.end,
                record.value,
            );
            if !push_automation_export_line(&mut output, &line) {
                return output;
            }
        }
    }
    output
}

pub(crate) fn automation_inspector_export(inspector: &InspectorSnapshot) -> String {
    let mut output = format!(
        "# RGV {} selected evidence export v1\n# byte_limit\t{MAX_AUTOMATION_EXPORT_BYTES}\nsection\tkey\tvalue\n",
        env!("CARGO_PKG_VERSION")
    );
    for (key, value) in &inspector.fields {
        if !push_automation_export_line(
            &mut output,
            &format!(
                "detail\t{}\t{}",
                sanitized_tsv_field(key, 4_096),
                sanitized_tsv_field(value, 64 * 1024)
            ),
        ) {
            return output;
        }
    }
    for (key, value) in &inspector.auxiliary {
        if !push_automation_export_line(
            &mut output,
            &format!(
                "tag\t{}\t{}",
                sanitized_tsv_field(key, 4_096),
                sanitized_tsv_field(value, 64 * 1024)
            ),
        ) {
            return output;
        }
    }
    for (key, value) in [
        ("cigar", inspector.cigar.as_deref()),
        ("event_sequence", inspector.event_sequence.as_deref()),
        (
            "read_sequence",
            inspector
                .read_sequence
                .as_ref()
                .map(|sequence| sequence.text.as_str()),
        ),
    ] {
        if let Some(value) = value
            && !push_automation_export_line(
                &mut output,
                &format!(
                    "evidence\t{key}\t{}",
                    sanitized_tsv_field(value, SEQUENCE_COPY_BASES)
                ),
            )
        {
            return output;
        }
    }
    output
}

pub(crate) fn sanitized_single_line(value: &str, maximum_bytes: usize) -> String {
    let mut sanitized = value.replace(['\r', '\n'], " ");
    truncate_string_to_bytes(&mut sanitized, maximum_bytes);
    sanitized
}

pub(crate) fn sanitized_tsv_field(value: &str, maximum_bytes: usize) -> String {
    let mut sanitized = value.replace(['\t', '\r', '\n'], " ");
    truncate_string_to_bytes(&mut sanitized, maximum_bytes);
    sanitized
}

pub(crate) fn create_output_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub(crate) fn atomic_write_text(path: &Path, contents: &str) -> std::result::Result<(), String> {
    create_output_parent(path).map_err(|error| error.to_string())?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(contents.as_bytes())
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temporary
        .persist(path)
        .map_err(|error| error.error.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod scale_tests {
    use super::*;
    use rgv_layout::Rgba;

    #[test]
    fn interactive_exports_use_the_display_density_and_automation_reports_pixels() {
        let scale = |factor: f64| interactive_export_scale(factor).to_bits();
        assert_eq!(scale(2.0), 2.0_f32.to_bits());
        assert_eq!(scale(1.5), 1.5_f32.to_bits());
        // A nonsensical factor never shrinks or breaks the export.
        assert_eq!(scale(0.5), 1.0_f32.to_bits());
        assert_eq!(scale(f64::NAN), 1.0_f32.to_bits());
        assert_eq!(scale(64.0), 8.0_f32.to_bits());
        // Automation snapshots stay one PNG pixel per logical point unless the command asks.
        assert_eq!(
            rgv_automation::default_snapshot_scale().to_bits(),
            1.0_f32.to_bits()
        );

        assert_eq!(scaled_snapshot_pixels(1_280, 720, 1.0), (1_280, 720));
        assert_eq!(scaled_snapshot_pixels(1_280, 720, 2.0), (2_560, 1_440));
        assert_eq!(scaled_snapshot_pixels(3, 3, 1.5), (5, 5));
    }

    #[test]
    fn a_scaled_png_has_the_reported_pixel_size() {
        let display = DisplayList {
            rectangles: vec![RectPrimitive {
                x: 10.0,
                y: 10.0,
                width: 20.0,
                height: 10.0,
                color: Rgba::new(0.1, 0.2, 0.3, 1.0),
                object_id: 0,
                shape: rgv_layout::RectShape::default(),
            }],
            texts: Vec::new(),
        };
        let directory = tempfile::tempdir().unwrap();
        for scale in [1.0_f32, 2.0] {
            let path = directory.path().join(format!("view-{scale}.png"));
            write_png_export(None, &display, 120, 60, scale, &path).unwrap();
            assert_eq!(
                image::image_dimensions(&path).unwrap(),
                scaled_snapshot_pixels(120, 60, scale)
            );
        }
    }
}
