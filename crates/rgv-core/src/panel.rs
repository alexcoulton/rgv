use serde::{Deserialize, Serialize};

use crate::{TrackId, ViewportGeneration};

/// Stable semantic identity for a view panel.
///
/// A panel ID is independent of its current position in the workspace. Moving,
/// resizing, or activating a panel must never change this value.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PanelId(u64);

impl PanelId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Stable identity of one track projection inside one view panel.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PanelTrackId {
    panel_id: PanelId,
    track_id: TrackId,
}

impl PanelTrackId {
    pub const fn new(panel_id: PanelId, track_id: TrackId) -> Self {
        Self { panel_id, track_id }
    }

    pub const fn panel_id(self) -> PanelId {
        self.panel_id
    }

    pub const fn track_id(self) -> TrackId {
        self.track_id
    }
}

/// Identifies one generation of work for one panel.
///
/// Generations are only meaningful within their owning panel. Carrying the
/// pair as one value prevents equal generation numbers from different panels
/// from being mistaken for the same request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PanelGeneration {
    panel_id: PanelId,
    generation: ViewportGeneration,
}

impl PanelGeneration {
    pub const fn new(panel_id: PanelId, generation: ViewportGeneration) -> Self {
        Self {
            panel_id,
            generation,
        }
    }

    pub const fn panel_id(self) -> PanelId {
        self.panel_id
    }

    pub const fn generation(self) -> ViewportGeneration {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_generations_in_different_panels_are_distinct() {
        let generation = ViewportGeneration::new(7);
        assert_ne!(
            PanelGeneration::new(PanelId::new(1), generation),
            PanelGeneration::new(PanelId::new(2), generation)
        );
    }

    #[test]
    fn panel_identity_round_trips_through_json() {
        let id = PanelId::new(42);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<PanelId>(&json).unwrap(), id);
    }

    #[test]
    fn track_projection_identity_includes_both_dimensions() {
        let baseline = PanelTrackId::new(PanelId::new(1), TrackId::new(1));
        assert_ne!(
            baseline,
            PanelTrackId::new(PanelId::new(2), TrackId::new(1))
        );
        assert_ne!(
            baseline,
            PanelTrackId::new(PanelId::new(1), TrackId::new(2))
        );
    }
}
