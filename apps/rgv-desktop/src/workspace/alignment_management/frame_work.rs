//! Main-thread alignment work owed to the next frame.
//!
//! A trackpad or wheel gesture delivers several input events per displayed frame. Each used to
//! dispatch the alignment query and capture the complete scene epoch (label projection and GPU
//! camera capture) synchronously, although only the last capture of a frame is ever drawn. While
//! an input event is being handled, scene-frame preparation and out-of-interval dispatches are
//! therefore owed to the frame instead: the frame runs each owed dispatch once and one capture,
//! before it rebuilds the display list. Outside input handling (render, worker polling,
//! automation) every request still runs at once, exactly as before.

use rgv_core::{PanelId, TrackId, Viewport};
use rgv_session::lane_layout::PanelLaneLayout;
use rgv_session::panels::ReadPanelScrollState;
use std::collections::BTreeMap;
use std::time::Duration;

/// Everything a scene-frame capture reads that a display-list rebuild can change: the committed
/// workspace and surface geometry, the alignment budget, and per panel its viewport, lane layout
/// and read-panel scroll states. Equal inputs within one frame capture the same epoch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AlignmentScenePrepareInputs {
    pub(crate) workspace_layout_revision: u64,
    pub(crate) surface: [u32; 2],
    pub(crate) scale_factor_bits: u64,
    pub(crate) budget_bytes: usize,
    pub(crate) panels: Vec<AlignmentScenePanelPrepareInputs>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AlignmentScenePanelPrepareInputs {
    pub(crate) panel_id: PanelId,
    pub(crate) viewport: Viewport,
    pub(crate) lane_layout: Option<PanelLaneLayout>,
    pub(crate) read_panel_scrolls: BTreeMap<TrackId, ReadPanelScrollState>,
}

/// Counters for the live workspace snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AlignmentFrameWorkTelemetry {
    /// Scene-frame preparations that ran.
    pub(crate) prepares: u64,
    /// Preparation requests absorbed by one that ran later in the same frame.
    pub(crate) prepares_coalesced: u64,
    /// Main-thread time spent preparing, in microseconds.
    pub(crate) prepare_micros_total: u64,
    pub(crate) prepare_micros_last: u64,
    /// Gesture steps whose dispatch was merged into a dispatch the frame already owed.
    pub(crate) dispatches_coalesced: u64,
    /// Display-list rebuilds whose layout refresh reused the capture this frame already made.
    pub(crate) layout_refresh_prepares_reused: u64,
    /// Full display-list rebuilds (a rebuild with nothing dirty returns early and is not counted).
    pub(crate) display_list_rebuilds: u64,
}

#[derive(Debug, Default)]
pub(crate) struct AlignmentFrameWork {
    /// An input event is being handled: preparation and dispatch are owed to the frame.
    deferring: bool,
    /// A preparation was requested while deferring and has not run since.
    prepare_owed: bool,
    /// Panels whose gesture left the prepared scene: one dispatch each at the next frame.
    dispatches_owed: Vec<PanelId>,
    /// Inputs of the preparation that ran in the current frame's reuse window, if any.
    frame_prepare_inputs: Option<AlignmentScenePrepareInputs>,
    /// Between the frame's flush and its shell pass: a layout refresh with unchanged inputs may
    /// reuse the capture the flush made.
    reuse_window_open: bool,
    pub(crate) telemetry: AlignmentFrameWorkTelemetry,
}

impl AlignmentFrameWork {
    /// An input event begins. Nested input handling is not expected; the flag is idempotent.
    pub(crate) fn begin_input(&mut self) {
        self.deferring = true;
    }

    /// The input event ends. Returns whether the frame now owes work, so the caller can ask for
    /// a redraw.
    pub(crate) fn end_input(&mut self) -> bool {
        self.deferring = false;
        self.owes_work()
    }

    pub(crate) const fn deferring(&self) -> bool {
        self.deferring
    }

    pub(crate) fn owes_work(&self) -> bool {
        self.prepare_owed || !self.dispatches_owed.is_empty()
    }

    /// Whether the presented alignment epoch may lag the live camera: a preparation is owed.
    /// Hit tests against it would answer for the previous camera.
    pub(crate) const fn prepare_owed(&self) -> bool {
        self.prepare_owed
    }

    /// A caller asks for a scene-frame preparation. Returns `true` when it must run now; while
    /// deferring it is owed to the frame instead and `false` is returned.
    pub(crate) fn request_prepare(&mut self) -> bool {
        if !self.deferring {
            return true;
        }
        if self.prepare_owed {
            self.telemetry.prepares_coalesced = self.telemetry.prepares_coalesced.saturating_add(1);
        }
        self.prepare_owed = true;
        false
    }

    /// A preparation ran: it satisfies any owed one.
    pub(crate) fn record_prepare(
        &mut self,
        elapsed: Duration,
        inputs: Option<AlignmentScenePrepareInputs>,
    ) {
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.prepare_owed = false;
        self.telemetry.prepares = self.telemetry.prepares.saturating_add(1);
        self.telemetry.prepare_micros_total =
            self.telemetry.prepare_micros_total.saturating_add(micros);
        self.telemetry.prepare_micros_last = micros;
        self.frame_prepare_inputs = if self.reuse_window_open { inputs } else { None };
    }

    /// Whether the frame needs the inputs of the next preparation recorded.
    pub(crate) const fn records_prepare_inputs(&self) -> bool {
        self.reuse_window_open
    }

    /// A gesture step left the prepared scene while deferring: dispatch once at the next frame.
    /// The camera moved, so a capture is owed too (the dispatch normally makes it).
    pub(crate) fn owe_dispatch(&mut self, panel_id: PanelId) {
        self.prepare_owed = true;
        if self.dispatches_owed.contains(&panel_id) {
            self.telemetry.dispatches_coalesced =
                self.telemetry.dispatches_coalesced.saturating_add(1);
        } else {
            self.dispatches_owed.push(panel_id);
        }
    }

    /// The frame begins. Opens the window, closed by [`Self::close_reuse_window`], in which an
    /// unchanged layout refresh may reuse a capture this frame already made.
    pub(crate) fn open_frame(&mut self) {
        self.frame_prepare_inputs = None;
        self.reuse_window_open = true;
    }

    /// Ends deferral and takes the owed dispatches: the frame runs each once, then the owed
    /// preparation unless a dispatch already prepared. Input that must hit-test the live camera
    /// (a click) takes them early.
    pub(crate) fn take_owed_dispatches(&mut self) -> Vec<PanelId> {
        self.deferring = false;
        std::mem::take(&mut self.dispatches_owed)
    }

    /// Takes the owed preparation, if no dispatch has already run one.
    pub(crate) fn take_owed_prepare(&mut self) -> bool {
        std::mem::take(&mut self.prepare_owed)
    }

    /// The frame's shell pass begins: whatever it changes is not covered by the recorded inputs.
    pub(crate) fn close_reuse_window(&mut self) {
        self.reuse_window_open = false;
        self.frame_prepare_inputs = None;
    }

    pub(crate) fn record_display_list_rebuild(&mut self) {
        self.telemetry.display_list_rebuilds =
            self.telemetry.display_list_rebuilds.saturating_add(1);
    }

    /// A layout refresh whose coordinator demand is unchanged asks whether it must prepare
    /// again. It may reuse the capture this frame already made when nothing that capture read
    /// has changed since.
    pub(crate) fn layout_refresh_reuses_frame_prepare(
        &mut self,
        inputs: &AlignmentScenePrepareInputs,
    ) -> bool {
        let reuse = self.reuse_window_open
            && !self.prepare_owed
            && self.frame_prepare_inputs.as_ref() == Some(inputs);
        if reuse {
            self.telemetry.layout_refresh_prepares_reused = self
                .telemetry
                .layout_refresh_prepares_reused
                .saturating_add(1);
        }
        reuse
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::{ContigId, GenomicInterval};

    fn inputs(start: u64) -> AlignmentScenePrepareInputs {
        let interval = GenomicInterval::new(ContigId::new(0), start, start + 1_000).unwrap();
        AlignmentScenePrepareInputs {
            workspace_layout_revision: 3,
            surface: [1_200, 800],
            scale_factor_bits: 2.0_f64.to_bits(),
            budget_bytes: 1 << 30,
            panels: vec![AlignmentScenePanelPrepareInputs {
                panel_id: PanelId::new(1),
                viewport: Viewport::new(interval, 1_000.0).unwrap(),
                lane_layout: None,
                read_panel_scrolls: BTreeMap::new(),
            }],
        }
    }

    #[test]
    fn outside_input_every_preparation_runs_at_once() {
        let mut work = AlignmentFrameWork::default();
        assert!(work.request_prepare());
        assert!(work.request_prepare());
        assert!(!work.owes_work());
    }

    #[test]
    fn a_burst_of_input_events_owes_one_preparation_to_the_frame() {
        let mut work = AlignmentFrameWork::default();
        for _ in 0..12 {
            work.begin_input();
            assert!(!work.request_prepare(), "deferred while handling input");
            assert!(work.prepare_owed(), "hit tests must not trust the epoch");
            assert!(work.end_input(), "the caller must request a redraw");
        }
        assert_eq!(work.telemetry.prepares, 0);
        assert_eq!(work.telemetry.prepares_coalesced, 11);

        work.open_frame();
        assert!(work.take_owed_dispatches().is_empty());
        assert!(work.take_owed_prepare());
        work.record_prepare(Duration::from_micros(40), None);
        assert!(!work.take_owed_prepare(), "one preparation per frame");
        assert_eq!(work.telemetry.prepares, 1);
        assert_eq!(work.telemetry.prepare_micros_last, 40);
        assert!(!work.owes_work());
    }

    #[test]
    fn a_dispatch_that_prepared_satisfies_the_owed_preparation() {
        let mut work = AlignmentFrameWork::default();
        work.begin_input();
        work.owe_dispatch(PanelId::new(1));
        assert!(work.prepare_owed(), "the moved camera owes a capture");
        assert!(!work.request_prepare());
        work.owe_dispatch(PanelId::new(1));
        work.end_input();
        assert_eq!(work.telemetry.dispatches_coalesced, 1);

        work.open_frame();
        assert_eq!(work.take_owed_dispatches(), vec![PanelId::new(1)]);
        // The frame's dispatch ends by preparing, outside input handling.
        assert!(work.request_prepare());
        work.record_prepare(Duration::ZERO, None);
        assert!(!work.take_owed_prepare());
        assert!(
            work.take_owed_dispatches().is_empty(),
            "each owed dispatch runs once"
        );

        // A dispatch that found nothing queued (the schedule got there first) did not prepare:
        // the owed capture still runs.
        work.begin_input();
        work.owe_dispatch(PanelId::new(1));
        work.end_input();
        assert_eq!(work.take_owed_dispatches(), vec![PanelId::new(1)]);
        assert!(work.take_owed_prepare());
    }

    #[test]
    fn an_unchanged_layout_refresh_reuses_the_frame_capture_only_inside_the_window() {
        let mut work = AlignmentFrameWork::default();
        assert!(!work.records_prepare_inputs());
        work.open_frame();
        assert!(work.records_prepare_inputs());
        work.record_prepare(Duration::ZERO, Some(inputs(100)));
        assert!(work.layout_refresh_reuses_frame_prepare(&inputs(100)));
        assert!(
            !work.layout_refresh_reuses_frame_prepare(&inputs(200)),
            "a changed viewport or layout captures again"
        );
        assert_eq!(work.telemetry.layout_refresh_prepares_reused, 1);

        work.close_reuse_window();
        assert!(
            !work.layout_refresh_reuses_frame_prepare(&inputs(100)),
            "the shell pass may change state the inputs do not cover"
        );
        work.open_frame();
        assert!(
            !work.layout_refresh_reuses_frame_prepare(&inputs(100)),
            "a capture from an earlier frame is never reused"
        );
    }
}
