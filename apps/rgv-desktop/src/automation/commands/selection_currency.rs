use crate::workspace::panels::ViewPanelController;
use rgv_core::TrackId;

pub(crate) fn auxiliary_position_snapshot_is_current(
    panel: &ViewPanelController,
    track_id: TrackId,
    position: u64,
) -> bool {
    let interval = panel.viewport.interval();
    panel
        .track_snapshot_intervals
        .get(&track_id)
        .copied()
        .is_some_and(|loaded| loaded.contains_interval(interval))
        && panel.track_settled_generation == Some(panel.generation_clock.current().get())
        && interval.contains(position)
        && panel
            .track_snapshots
            .iter()
            .any(|snapshot| snapshot.id == track_id)
}
