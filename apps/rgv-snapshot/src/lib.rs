//! Headless snapshots: open a view with no window and no GPU, drive the [`rgv_session`] session
//! through its headless presenter until every panel settles, and write the image with the CPU
//! projectors (`rgv_render::project_semantic_alignment_export` for alignment lanes, plain
//! rectangles and labels for annotation lanes) to PNG (CPU rasteriser) or SVG.
//!
//! Rendering lives here rather than in `rgv-session` so the session crate's dependency graph
//! stays free of `wgpu` (pulled in by `rgv-render`).

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use rgv_alignment_scene::AlignmentPanelSceneFrame;
use rgv_core::{
    ContigId, DesktopSettings, GenomeAssembly, GenomicInterval, GenomicSourceFormat, PanelId,
    ReferenceDictionary, SessionAlignmentTrack, SessionDocument, TrackConfig, TrackSource,
    Viewport, classify_genomic_source,
};
use rgv_engine::{TrackSnapshotData, TrackSpec, TrackSpecKind};
use rgv_layout::{DisplayList, RectPrimitive, RectShape, Rgba, TextPrimitive};
use rgv_render::palette::AlignmentPalette;
use rgv_session::Session;
use rgv_session::headless::{HeadlessCanvas, HeadlessHost};
use rgv_session::lane_layout::{LaneOwner, PanelLaneLayout};
use rgv_session::panels::PanelState;
use rgv_session::presentation::{PanelPresentation, TrackLanePresentation};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Image formats `rgv-snapshot` writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotFormat {
    Png,
    Svg,
}

/// What to open and how large to draw it.
#[derive(Clone, Debug)]
pub struct SnapshotRequest {
    /// An RGV session file; its reference, tracks and locus are used, and the options below add
    /// to or override them.
    pub session_file: Option<PathBuf>,
    pub reference: Option<PathBuf>,
    /// Alignment (BAM/CRAM) and annotation track files, classified by extension.
    pub tracks: Vec<PathBuf>,
    pub locus: Option<String>,
    /// Canvas size in logical points.
    pub width: u32,
    pub height: u32,
}

/// Width of the track-name gutter left of the panels, in logical points.
pub const SNAPSHOT_GUTTER_WIDTH: f32 = 160.0;

fn local(path: &Path) -> TrackSource {
    TrackSource::Local {
        path: path.to_string_lossy().into_owned(),
    }
}

fn display_name(source: &TrackSource) -> String {
    let label = source.redacted_label();
    Path::new(&label)
        .file_name()
        .map_or(label.clone(), |name| name.to_string_lossy().into_owned())
}

fn track_spec_kind(format: GenomicSourceFormat) -> Option<TrackSpecKind> {
    Some(match format {
        GenomicSourceFormat::Bed => TrackSpecKind::Bed,
        GenomicSourceFormat::Gff3 => TrackSpecKind::Gff3,
        GenomicSourceFormat::Gtf => TrackSpecKind::Gtf,
        GenomicSourceFormat::GenePred => TrackSpecKind::GenePred,
        GenomicSourceFormat::RefFlat => TrackSpecKind::RefFlat,
        GenomicSourceFormat::NarrowPeak => TrackSpecKind::NarrowPeak,
        GenomicSourceFormat::BroadPeak => TrackSpecKind::BroadPeak,
        GenomicSourceFormat::Psl => TrackSpecKind::Psl,
        GenomicSourceFormat::Gwas => TrackSpecKind::Gwas,
        GenomicSourceFormat::Vcf | GenomicSourceFormat::Bcf => TrackSpecKind::VcfOrBcf,
        GenomicSourceFormat::BigWig => TrackSpecKind::BigWig,
        GenomicSourceFormat::Tdf => TrackSpecKind::Tdf,
        GenomicSourceFormat::BedGraph => TrackSpecKind::BedGraph,
        GenomicSourceFormat::Wig => TrackSpecKind::Wig,
        GenomicSourceFormat::BedPe => TrackSpecKind::BedPe,
        GenomicSourceFormat::Interact => TrackSpecKind::Interact,
        GenomicSourceFormat::Seg => TrackSpecKind::Seg,
        GenomicSourceFormat::MutationMaf => TrackSpecKind::MutationMaf,
        GenomicSourceFormat::BigBed => TrackSpecKind::BigBed,
        GenomicSourceFormat::BigGenePred => TrackSpecKind::BigGenePred,
        GenomicSourceFormat::BigNarrowPeak => TrackSpecKind::BigNarrowPeak,
        GenomicSourceFormat::Bam | GenomicSourceFormat::Cram | GenomicSourceFormat::Fasta => {
            return None;
        }
    })
}

fn session_locus_label(document: &SessionDocument) -> String {
    format!(
        "{}:{}-{}",
        document.locus.contig,
        document.locus.start.saturating_add(1),
        document.locus.end
    )
}

/// Opens the request in a headless session. Nothing has settled yet; call
/// [`HeadlessHost::settle`].
#[allow(clippy::too_many_lines)]
pub fn open_host(request: &SnapshotRequest) -> Result<HeadlessHost> {
    ensure!(
        request.width >= 200 && request.height >= 100,
        "the canvas must be at least 200 x 100 points"
    );
    let document = request
        .session_file
        .as_ref()
        .map(|path| {
            // Relative sources resolve against the session file's directory.
            SessionDocument::read(path)
                .map_err(|error| anyhow!("could not open the session {}: {error}", path.display()))
        })
        .transpose()?;
    let locus = request
        .locus
        .clone()
        .or_else(|| document.as_ref().map(session_locus_label));
    let width = request.width as f32;
    let canvas = HeadlessCanvas {
        width,
        height: request.height as f32,
        gutter_width: SNAPSHOT_GUTTER_WIDTH,
    };
    let column_width = f64::from((width - SNAPSHOT_GUTTER_WIDTH).max(1.0));
    let placeholder = Viewport::new(GenomicInterval::new(ContigId::new(0), 0, 1)?, column_width)?;
    let panel = PanelState::new(PanelId::new(1), placeholder, locus, String::new(), 0);
    // Settings are defaults and never written: the headless presenter does not persist.
    let settings_path = std::env::temp_dir().join("rgv-snapshot-unused-settings.json");
    let mut session = Session::new(
        panel,
        GenomeAssembly::from_dictionary("headless", ReferenceDictionary::from_contigs([])),
        DesktopSettings::default(),
        settings_path,
    );

    let reference = request
        .reference
        .as_ref()
        .map(|path| local(path))
        .or_else(|| {
            document
                .as_ref()
                .and_then(|document| document.reference.clone())
        });
    if let Some(source) = reference {
        let path = rgv_session::sources::source_path(&source);
        let options = rgv_session::sources::reference_open_options(
            document
                .as_ref()
                .and_then(|document| document.remote_reference_index_url.clone()),
        );
        let engine = session
            .start_reference_engine(path.clone(), options)
            .with_context(|| format!("could not open the reference {}", path.display()))?;
        session.install_reference_engine(engine, source);
    }

    let mut alignment_configs = document
        .as_ref()
        .map(|document| document.alignment_tracks.clone())
        .unwrap_or_default();
    for config in &alignment_configs {
        session.reserve_track_ids_through(config.id.get());
    }
    let mut annotation_configs = Vec::new();
    if let Some(document) = &document {
        for config in &document.tracks {
            session.reserve_track_ids_through(config.id.get());
            annotation_configs.push(config.clone());
        }
    }
    for path in &request.tracks {
        let text = path.to_string_lossy();
        let classified = classify_genomic_source(&text)
            .ok_or_else(|| anyhow!("cannot tell what kind of file {} is", path.display()))?;
        let id = session.allocate_track_id();
        let source = local(path);
        let name = display_name(&source);
        match classified.format {
            GenomicSourceFormat::Bam | GenomicSourceFormat::Cram => {
                alignment_configs.push(SessionAlignmentTrack::new(id, name, source));
            }
            GenomicSourceFormat::Fasta => {
                bail!(
                    "{} is a reference; pass it with --reference",
                    path.display()
                );
            }
            format => {
                annotation_configs.push(TrackConfig::new(id, name, format.kind(), source));
            }
        }
    }

    if !alignment_configs.is_empty() {
        let controllers = session
            .prepare_alignment_track_controllers_for_workspace(alignment_configs)
            .context("could not open the alignment tracks")?;
        session.swap_alignment_track_controllers(controllers);
        session.reconcile_track_sources();
    }
    // One engine starts with every annotation track, as the desktop does when it opens a
    // session: adding them one by one could overrun the engine's bounded control queue.
    let mut specifications = Vec::with_capacity(annotation_configs.len());
    for config in &annotation_configs {
        let path = rgv_session::sources::source_path(&config.source);
        let format = classify_genomic_source(&path.to_string_lossy())
            .ok_or_else(|| anyhow!("cannot tell what kind of file {} is", path.display()))?
            .format;
        let kind = track_spec_kind(format)
            .ok_or_else(|| anyhow!("{} is not an annotation track", path.display()))?;
        specifications.push((
            TrackSpec {
                id: config.id,
                name: config.name.clone(),
                path,
                kind,
            },
            BTreeMap::new(),
        ));
    }
    if !specifications.is_empty() {
        session.track_engine = Some(rgv_session::sources::spawn_track_engine(specifications));
        for config in annotation_configs {
            session.tracks.push(config);
        }
        session.invalidate_navigation_contract("annotation tracks opened");
        session.reconcile_track_sources();
    }
    ensure!(
        session.reference_engine.is_some() || !session.alignment_tracks.is_empty(),
        "nothing to draw against: pass --reference or an alignment track"
    );
    Ok(HeadlessHost::new(session, canvas))
}

const fn rgba(red: f32, green: f32, blue: f32) -> Rgba {
    Rgba::new(red, green, blue, 1.0)
}

const LABEL: Rgba = rgba(0.15, 0.17, 0.2);
const RULE: Rgba = rgba(0.82, 0.84, 0.87);
const FEATURE: Rgba = rgba(0.16, 0.35, 0.65);
const VARIANT: Rgba = rgba(0.75, 0.25, 0.2);

fn rect(x: f32, y: f32, width: f32, height: f32, color: Rgba) -> RectPrimitive {
    RectPrimitive {
        x,
        y,
        width,
        height,
        color,
        object_id: 0,
        shape: RectShape::Rectangle,
    }
}

fn text(x: f32, y: f32, value: impl Into<String>, font_size: f32) -> TextPrimitive {
    TextPrimitive {
        x,
        y,
        text: value.into(),
        font_size,
        color: LABEL,
    }
}

/// Screen x of genomic position `position` in `viewport`, drawn in a column starting at `left`.
fn screen_x(viewport: Viewport, left: f32, position: u64) -> f32 {
    let start = viewport.interval().start();
    let offset = position as f64 - start as f64;
    left + (offset / viewport.bases_per_pixel()) as f32
}

fn draw_ruler(
    display: &mut DisplayList,
    presentation: &PanelPresentation,
    layout: &PanelLaneLayout,
) {
    let interval = presentation.viewport.interval();
    let label = format!(
        "{}:{}-{}",
        presentation.contig_name,
        interval.start().saturating_add(1),
        interval.end()
    );
    let centre = f32::midpoint(layout.canvas.left, layout.canvas.right);
    display.texts.push(text(centre, 12.0, label, 12.0));
    display.rectangles.push(rect(
        layout.canvas.left,
        rgv_session::headless::HEADLESS_RULER_HEIGHT - 1.0,
        layout.canvas.width(),
        1.0,
        RULE,
    ));
}

fn draw_track_lane(
    display: &mut DisplayList,
    presentation: &PanelPresentation,
    layout: &PanelLaneLayout,
    lane: &TrackLanePresentation,
) {
    let Some(geometry) = layout
        .annotation
        .lanes
        .iter()
        .find(|candidate| candidate.owner == LaneOwner::Auxiliary(lane.track_id))
    else {
        return;
    };
    let Some(content) = geometry.content_rect else {
        return;
    };
    let viewport = presentation.viewport;
    let interval = viewport.interval();
    let left = layout.canvas.left;
    let right = layout.canvas.right;
    let clip = |start: u64, end: u64| {
        let x0 = screen_x(viewport, left, start).max(left);
        let x1 = screen_x(viewport, left, end).min(right);
        (x1 > x0).then_some((x0, (x1 - x0).max(1.0)))
    };
    let body_top = content.top + 4.0;
    let body_height = 10.0;
    let label_y = body_top + body_height + 9.0;
    let mut draw = |start: u64, end: u64, label: Option<&str>, color: Rgba| {
        if end <= interval.start() || start >= interval.end() {
            return;
        }
        let Some((x, width)) = clip(start, end.max(start.saturating_add(1))) else {
            return;
        };
        display
            .rectangles
            .push(rect(x, body_top, width, body_height, color));
        if let Some(label) = label {
            display
                .texts
                .push(text(x + width / 2.0, label_y, label, 10.0));
        }
    };
    match lane.snapshot.as_ref().map(|snapshot| &snapshot.data) {
        Some(TrackSnapshotData::Features(features)) => {
            for feature in features
                .iter()
                .filter(|feature| feature.contig == presentation.contig_name)
            {
                draw(feature.start, feature.end, feature.name.as_deref(), FEATURE);
            }
        }
        Some(TrackSnapshotData::Variants(query)) => {
            for record in query
                .records
                .iter()
                .filter(|record| record.contig == presentation.contig_name)
            {
                draw(record.start, record.end, record.id.as_deref(), VARIANT);
            }
        }
        _ => {}
    }
}

fn draw_gutter(display: &mut DisplayList, layout: &PanelLaneLayout) {
    for lane in layout
        .annotation
        .lanes
        .iter()
        .chain(layout.reads.lanes.iter())
    {
        if lane.header_rect.height() <= 0.0 {
            continue;
        }
        let y = f32::midpoint(lane.header_rect.top, lane.header_rect.bottom);
        // About 6.5 points per character at 11 points: keep the name inside the gutter.
        let name = if lane.name.chars().count() > 22 {
            format!("{}…", lane.name.chars().take(21).collect::<String>())
        } else {
            lane.name.clone()
        };
        display
            .texts
            .push(text(SNAPSHOT_GUTTER_WIDTH / 2.0, y, name, 11.0));
        display
            .rectangles
            .push(rect(0.0, lane.row_rect.top, layout.canvas.right, 1.0, RULE));
    }
}

/// The committed alignment frames of a panel whose every lane is complete.
fn complete_frames(host: &HeadlessHost, panel_id: PanelId) -> Vec<AlignmentPanelSceneFrame> {
    host.presented_frames(panel_id)
        .into_iter()
        .filter(|frame| {
            frame
                .lanes
                .iter()
                .all(rgv_alignment_scene::AlignmentPanelLaneFrame::visible_complete)
        })
        .cloned()
        .collect()
}

/// Draws every settled panel: ruler, gutter labels, annotation lanes, then the alignment lanes
/// through the same CPU projection the desktop's figure export uses.
pub fn render_display_list(host: &HeadlessHost) -> Result<DisplayList> {
    let mut display = DisplayList::default();
    let panel_ids = host
        .session
        .panels
        .states()
        .map(|panel| panel.id)
        .collect::<Vec<_>>();
    for (index, panel_id) in panel_ids.into_iter().enumerate() {
        let presentation = host
            .presentation(panel_id)
            .ok_or_else(|| anyhow!("panel {} vanished", panel_id.get()))?;
        let layout = presentation
            .lanes
            .clone()
            .ok_or_else(|| anyhow!("panel {} has no canvas", panel_id.get()))?;
        if index == 0 {
            draw_gutter(&mut display, &layout);
        }
        draw_ruler(&mut display, &presentation, &layout);
        for lane in &presentation.tracks {
            draw_track_lane(&mut display, &presentation, &layout, lane);
        }
        let frames = complete_frames(host, panel_id);
        if !frames.is_empty() {
            let alignments = rgv_render::project_semantic_alignment_export(
                &frames,
                &host.session.alignment_scene_ledger,
                &HashMap::new(),
                &AlignmentPalette::default(),
            )
            .context("could not project the alignment lanes")?;
            display
                .rectangles
                .extend(alignments.display.rectangles.iter().copied());
            display
                .texts
                .extend(alignments.display.texts.iter().cloned());
        }
    }
    Ok(display)
}

fn svg_color(color: Rgba) -> String {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "rgb({},{},{})\" fill-opacity=\"{:.3}",
        channel(color.red),
        channel(color.green),
        channel(color.blue),
        color.alpha.clamp(0.0, 1.0)
    )
}

fn svg_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The display list as an SVG document of `width` x `height` points.
pub fn display_list_svg(display: &DisplayList, width: u32, height: u32) -> String {
    let mut output = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" \
         width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n\
         <rect width=\"100%\" height=\"100%\" fill=\"white\"/>\n"
    );
    for rectangle in &display.rectangles {
        let _ = writeln!(
            output,
            "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"{}\"/>",
            rectangle.x,
            rectangle.y,
            rectangle.width,
            rectangle.height,
            svg_color(rectangle.color)
        );
    }
    for label in &display.texts {
        let _ = writeln!(
            output,
            "<text x=\"{:.2}\" y=\"{:.2}\" font-family=\"sans-serif\" font-size=\"{:.1}\" \
             text-anchor=\"middle\" dominant-baseline=\"central\" fill=\"{}\">{}</text>",
            label.x,
            label.y,
            label.font_size,
            svg_color(label.color),
            svg_escape(&label.text)
        );
    }
    output.push_str("</svg>\n");
    output
}

/// Writes `display` to `out`: PNG through the CPU rasteriser at `scale` pixels per point, or SVG.
pub fn write_image(
    display: &DisplayList,
    width: u32,
    height: u32,
    scale: f32,
    format: SnapshotFormat,
    out: &Path,
) -> Result<()> {
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    match format {
        SnapshotFormat::Png => {
            rgv_render::export_png_scaled_prefer_gpu(None, display, width, height, scale, out)
                .with_context(|| format!("could not write {}", out.display()))?;
        }
        SnapshotFormat::Svg => {
            std::fs::write(out, display_list_svg(display, width, height))
                .with_context(|| format!("could not write {}", out.display()))?;
        }
    }
    Ok(())
}

/// Opens, settles and writes one snapshot.
pub fn snapshot(
    request: &SnapshotRequest,
    format: SnapshotFormat,
    scale: f32,
    out: &Path,
    timeout: Duration,
) -> Result<HeadlessHost> {
    let mut host = open_host(request)?;
    host.settle(timeout)?;
    let placed = host
        .session
        .panels
        .states()
        .all(|panel| !panel.contig_name.is_empty());
    ensure!(
        placed,
        "the view could not be placed on a contig: {}",
        host.session.data_status
    );
    let display = render_display_list(&host)?;
    write_image(&display, request.width, request.height, scale, format, out)?;
    Ok(host)
}
