//! Track identifiers that are never reused within one run (APP-14).

use rgv_core::TrackId;

/// Allocates track ids that are never reused within one run, so a late worker event for a
/// removed track cannot be attributed to its replacement.
#[derive(Debug, Default)]
pub struct TrackIdAllocator {
    high_water: u64,
}

impl TrackIdAllocator {
    /// `largest_live` is the largest id currently in the workspace (ids restored from a session
    /// are not issued by this allocator and must still be skipped).
    pub fn allocate(&mut self, largest_live: u64) -> TrackId {
        let id = self.high_water.max(largest_live).saturating_add(1);
        self.high_water = id;
        TrackId::new(id)
    }

    /// The first id of a block the caller numbers itself; pair with [`Self::reserve_through`].
    pub fn peek(&self, largest_live: u64) -> Option<u64> {
        self.high_water.max(largest_live).checked_add(1)
    }

    /// Marks every id up to `id` as issued.
    pub fn reserve_through(&mut self, id: u64) {
        self.high_water = self.high_water.max(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_ids_are_never_reused_after_the_largest_track_is_removed() {
        let mut ids = TrackIdAllocator::default();
        assert_eq!(ids.allocate(4), TrackId::new(5));
        // Track 5 is removed again; `max + 1` would hand 5 out a second time.
        assert_eq!(ids.allocate(4), TrackId::new(6));
        // Ids restored from a session are skipped.
        assert_eq!(ids.allocate(40), TrackId::new(41));
        assert_eq!(ids.allocate(0), TrackId::new(42));
        // A block numbered by the caller is reserved even if its tracks are removed later.
        assert_eq!(ids.peek(0), Some(43));
        ids.reserve_through(47);
        assert_eq!(ids.allocate(0), TrackId::new(48));
    }
}
