//! A continuous gesture inside the scenes an alignment lane already prepared is a camera-only
//! move: no query is dispatched until the gesture settles, and then exactly one, whose camera is
//! the settled viewport. Leaving the prepared interval needs a dispatch.
//!
//! Driven through the headless presenter over `benchmark-data/mixed.bam`; skips when that local
//! benchmark file is absent, like the other tests that read it.

use rgv_core::{PanelId, Viewport};
use rgv_session::alignment::dispatch::{
    AlignmentGestureStep, alignment_gesture_step, alignment_viewport_change_is_camera_only,
    dispatch_alignment_query,
};
use rgv_session::headless::HeadlessHost;
use rgv_snapshot::{SnapshotRequest, open_host};
use std::path::{Path, PathBuf};
use std::time::Duration;

const PANEL: PanelId = PanelId::new(1);

fn mixed_bam() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipping: {} is absent", path.display());
        None
    }
}

fn settled_host(bam: PathBuf) -> HeadlessHost {
    let mut host = open_host(&SnapshotRequest {
        session_file: None,
        reference: None,
        tracks: vec![bam],
        locus: Some("chr1:905001-907000".to_owned()),
        width: 1200,
        height: 800,
    })
    .unwrap();
    host.settle(Duration::from_secs(120)).unwrap();
    host
}

/// What the desktop does for one gesture step before the alignment decision: move the viewport,
/// advance the panel's and every lane's generation, and queue each lane's transaction.
fn move_viewport(host: &mut HeadlessHost, viewport: Viewport) {
    let session = &mut host.session;
    let panel = session.panels.panel_state_mut(PANEL);
    panel.viewport = viewport;
    panel.generation_clock.advance();
    for track_index in 0..session.alignment_tracks.len() {
        session.queue_alignment_navigation_for_track(track_index, PANEL, true, "gesture step");
    }
    session.mark_alignment_queries_queued(PANEL);
}

fn viewport(host: &HeadlessHost) -> Viewport {
    host.session.panels.panel_state(PANEL).viewport
}

fn contig_length(host: &HeadlessHost) -> u64 {
    host.session.panels.panel_state(PANEL).contig_length
}

fn dispatches(host: &HeadlessHost) -> u64 {
    host.session.alignment_dispatch_telemetry.dispatches
}

#[test]
fn a_gesture_inside_the_prepared_scene_dispatches_once_when_it_settles() {
    let Some(bam) = mixed_bam() else {
        return;
    };
    let mut host = settled_host(bam);
    let prepared = host.session.alignment_tracks[0]
        .projection(PANEL)
        .and_then(|projection| projection.scene_frame.as_ref())
        .and_then(|frame| frame.snapshot.as_ref())
        .map(|snapshot| snapshot.prepared_interval())
        .expect("the settled lane holds a prepared scene");
    let start = viewport(&host);
    assert!(
        prepared.contains_interval(start.interval()) && prepared.len() > start.interval().len(),
        "the source prepares a browsing buffer around the view: {prepared:?} vs {:?}",
        start.interval()
    );
    let before = dispatches(&host);
    let camera_only_before = host.session.alignment_dispatch_telemetry.camera_only_steps;

    // Twenty small pans to the right and back, a few pixels at a time, never leaving the
    // prepared interval with its margin.
    let length = contig_length(&host);
    let mut steps = 0;
    for points in [6.0, 6.0, 6.0, 6.0, 6.0, -6.0, -6.0, -6.0, -6.0, -6.0]
        .into_iter()
        .cycle()
        .take(20)
    {
        let next = viewport(&host).pan_pixels(points, length).unwrap();
        move_viewport(&mut host, next);
        assert_eq!(
            alignment_gesture_step(&mut host, PANEL),
            AlignmentGestureStep::CameraOnly,
            "step {steps} at {:?} stays inside {prepared:?}",
            next.interval()
        );
        steps += 1;
    }
    assert_eq!(
        dispatches(&host),
        before,
        "a camera-only gesture dispatches nothing"
    );
    assert_eq!(
        host.session.alignment_dispatch_telemetry.camera_only_steps - camera_only_before,
        steps
    );
    assert!(
        host.session
            .panels
            .panel_state(PANEL)
            .pending_queries
            .alignment,
        "the settled viewport is still owed its query"
    );

    // The gesture settles: exactly one dispatch, for the final viewport.
    let settled = viewport(&host);
    dispatch_alignment_query(&mut host, PANEL);
    assert_eq!(dispatches(&host), before + 1);
    let telemetry = host.session.alignment_dispatch_telemetry;
    assert!(telemetry.last_micros <= telemetry.total_micros);
    let projection = host.session.alignment_tracks[0].projection(PANEL).unwrap();
    let desired = projection
        .navigation
        .desired_camera()
        .expect("the dispatch bound the settled camera");
    assert_eq!(desired.interval(), settled.interval());
    host.settle(Duration::from_secs(120)).unwrap();
    let frame = host.session.alignment_tracks[0]
        .projection(PANEL)
        .and_then(|projection| projection.scene_frame.as_ref())
        .unwrap();
    assert_eq!(
        frame.camera.interval(),
        settled.interval(),
        "the settled scene answers the final viewport"
    );
}

#[test]
fn leaving_the_prepared_interval_needs_a_dispatch() {
    let Some(bam) = mixed_bam() else {
        return;
    };
    let mut host = settled_host(bam);
    let prepared = host.session.alignment_tracks[0]
        .projection(PANEL)
        .and_then(|projection| projection.scene_frame.as_ref())
        .and_then(|frame| frame.snapshot.as_ref())
        .map(|snapshot| snapshot.prepared_interval())
        .unwrap();
    let length = contig_length(&host);
    let start = viewport(&host);
    // Pan until the view (plus margin) runs past the prepared interval's right edge.
    let pixels_per_base = start.pixel_width() / start.span();
    let beyond = (prepared.end() - start.interval().start()) as f64 * pixels_per_base;
    let next = start.pan_pixels(beyond, length).unwrap();
    assert!(next.interval().end() > prepared.end());
    move_viewport(&mut host, next);
    assert!(!alignment_viewport_change_is_camera_only(&host, PANEL));
    let before = dispatches(&host);
    assert_eq!(
        alignment_gesture_step(&mut host, PANEL),
        AlignmentGestureStep::NeedsDispatch
    );
    assert_eq!(dispatches(&host), before, "the caller owns the dispatch");
    dispatch_alignment_query(&mut host, PANEL);
    assert_eq!(dispatches(&host), before + 1);
    host.settle(Duration::from_secs(120)).unwrap();

    // A zoom across a level of detail changes the scene contract even inside the interval.
    let zoomed = viewport(&host)
        .zoom_at(viewport(&host).pixel_width() / 2.0, 40.0, length)
        .unwrap();
    move_viewport(&mut host, zoomed);
    assert_eq!(
        alignment_gesture_step(&mut host, PANEL),
        AlignmentGestureStep::NeedsDispatch
    );
}
