//! Collision-free object identifiers for pickable display primitives.
//!
//! A display list carries one `u32` object id per primitive; zero means "not
//! pickable". Deriving a track's ids arithmetically from its track id (for
//! example `1_000_000 + track_id * 100_000 + record_index`) silently aliases
//! two tracks as soon as one has more records than the stride, and saturating
//! arithmetic aliases every overflowing record onto `u32::MAX`. Allocate each
//! track a disjoint [`ObjectIdRange`] sized to its record count instead.

/// The id carried by primitives that do not identify a record.
pub const NO_OBJECT_ID: u32 = 0;

/// The object id of record `record_index` in a track whose ids start at
/// `base`, or [`NO_OBJECT_ID`] when it does not fit in `u32`.
///
/// Overflow never wraps or saturates onto another record's id: a record that
/// cannot be identified is drawn but is not pickable.
#[must_use]
pub fn record_object_id(base: u32, record_index: usize) -> u32 {
    u32::try_from(record_index)
        .ok()
        .and_then(|index| base.checked_add(index))
        .unwrap_or(NO_OBJECT_ID)
}

/// A contiguous block of object ids owned by one track.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ObjectIdRange {
    base: u32,
    len: u32,
}

impl ObjectIdRange {
    /// First id of the range; pass it as `TrackLayoutOptions::object_id_base`.
    #[must_use]
    pub const fn base(self) -> u32 {
        self.base
    }

    /// Number of records the range can identify.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len as usize
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub const fn contains(self, object_id: u32) -> bool {
        object_id >= self.base && object_id - self.base < self.len
    }

    /// The id of `record_index`, or `None` if the range does not cover it.
    #[must_use]
    pub fn object_id(self, record_index: usize) -> Option<u32> {
        let index = u32::try_from(record_index).ok()?;
        (index < self.len).then(|| self.base + index)
    }

    /// The record an id of this range identifies; `None` for another track's
    /// id, so a hit can never be attributed to the wrong track.
    #[must_use]
    pub fn record_index(self, object_id: u32) -> Option<usize> {
        self.contains(object_id)
            .then(|| (object_id - self.base) as usize)
    }
}

/// Hands out disjoint [`ObjectIdRange`]s, one per track, for one frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectIdAllocator {
    next: u32,
}

impl Default for ObjectIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl ObjectIdAllocator {
    /// Starts after [`NO_OBJECT_ID`].
    #[must_use]
    pub const fn new() -> Self {
        Self::starting_at(1)
    }

    /// Starts at `first` (raised to one), leaving lower ids to other users of
    /// the same id space.
    #[must_use]
    pub const fn starting_at(first: u32) -> Self {
        Self {
            next: if first == NO_OBJECT_ID { 1 } else { first },
        }
    }

    /// Reserves ids for `record_count` records. `None` when the `u32` space is
    /// exhausted; the allocator is then unchanged and the caller should lay
    /// the track out unpickable rather than reuse another track's ids.
    pub fn allocate(&mut self, record_count: usize) -> Option<ObjectIdRange> {
        let len = u32::try_from(record_count).ok()?;
        let end = self.next.checked_add(len)?;
        let range = ObjectIdRange {
            base: self.next,
            len,
        };
        self.next = end;
        Some(range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_stride_arithmetic_collides_where_allocated_ranges_do_not() {
        // The scheme being replaced: track 1's record 100,000 is track 2's record 0.
        let legacy = |track: u32, record: u32| 1_000_000 + track * 100_000 + record;
        assert_eq!(legacy(1, 100_000), legacy(2, 0));

        let mut allocator = ObjectIdAllocator::new();
        let first = allocator.allocate(250_000).unwrap();
        let second = allocator.allocate(250_000).unwrap();
        let empty = allocator.allocate(0).unwrap();
        let third = allocator.allocate(3).unwrap();
        assert!(empty.is_empty());
        assert_eq!(first.len(), 250_000);
        let mut seen = std::collections::HashSet::new();
        for range in [first, second, third] {
            for record in 0..range.len() {
                let id = range.object_id(record).unwrap();
                assert_ne!(id, NO_OBJECT_ID);
                assert!(seen.insert(id));
                assert_eq!(range.record_index(id), Some(record));
                assert_eq!(record_object_id(range.base(), record), id);
            }
        }
        assert_eq!(first.record_index(second.base()), None);
        assert_eq!(second.record_index(first.base()), None);
        assert_eq!(third.object_id(3), None);
        assert_eq!(first.record_index(NO_OBJECT_ID), None);
    }

    #[test]
    fn exhaustion_and_overflow_never_alias_another_record() {
        let mut allocator = ObjectIdAllocator::starting_at(u32::MAX - 10);
        let fits = allocator.allocate(10).unwrap();
        assert_eq!(fits.object_id(9), Some(u32::MAX - 1));
        // A refused request leaves the allocator usable and hands out nothing
        // that overlaps an earlier range.
        assert_eq!(allocator.allocate(2), None);
        assert_eq!(allocator.allocate(1), None);
        let empty = allocator.allocate(0).unwrap();
        assert!(empty.is_empty() && !fits.contains(empty.base()));
        assert_eq!(ObjectIdAllocator::starting_at(0), ObjectIdAllocator::new());

        // The previous saturating arithmetic mapped both of these to u32::MAX.
        assert_eq!(record_object_id(u32::MAX - 1, 1), u32::MAX);
        assert_eq!(record_object_id(u32::MAX - 1, 2), NO_OBJECT_ID);
        assert_eq!(record_object_id(u32::MAX - 1, 3), NO_OBJECT_ID);
        assert_eq!(record_object_id(7, usize::MAX), NO_OBJECT_ID);
        assert_eq!(record_object_id(7, 5), 12);
    }
}
