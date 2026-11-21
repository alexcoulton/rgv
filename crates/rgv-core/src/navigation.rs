use std::collections::VecDeque;

use crate::GenomicInterval;

const DEFAULT_HISTORY_CAPACITY: usize = 256;

#[derive(Clone, Debug)]
pub struct NavigationHistory {
    entries: VecDeque<GenomicInterval>,
    cursor: usize,
    capacity: usize,
}

impl NavigationHistory {
    pub fn new(initial: GenomicInterval) -> Self {
        Self::with_capacity(initial, DEFAULT_HISTORY_CAPACITY)
    }

    pub fn with_capacity(initial: GenomicInterval, capacity: usize) -> Self {
        let mut entries = VecDeque::new();
        entries.push_back(initial);
        Self {
            entries,
            cursor: 0,
            capacity: capacity.max(1),
        }
    }

    pub fn visit(&mut self, interval: GenomicInterval) {
        if self.current() == interval {
            return;
        }
        self.entries.truncate(self.cursor.saturating_add(1));
        self.entries.push_back(interval);
        if self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
        self.cursor = self.entries.len().saturating_sub(1);
    }

    pub fn back(&mut self) -> Option<GenomicInterval> {
        if !self.can_go_back() {
            return None;
        }
        self.cursor -= 1;
        self.entries.get(self.cursor).copied()
    }

    pub fn forward(&mut self) -> Option<GenomicInterval> {
        if !self.can_go_forward() {
            return None;
        }
        self.cursor += 1;
        self.entries.get(self.cursor).copied()
    }

    pub fn current(&self) -> GenomicInterval {
        self.entries[self.cursor]
    }

    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns a read-only view of retained entries without moving the cursor.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &GenomicInterval> {
        self.entries.iter()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn select(&mut self, index: usize) -> Option<GenomicInterval> {
        let interval = self.entries.get(index).copied()?;
        self.cursor = index;
        Some(interval)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One visited locus identified by contig *name*.
///
/// A [`ContigId`](crate::ContigId) is only an index into one dictionary: the
/// workspace assembly and each alignment source number the same contig
/// differently, and without any dictionary every contig is id zero. History
/// therefore keys on the name and re-resolves the id when an entry is
/// revisited.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NavigationEntry {
    pub contig_name: String,
    /// Zero-based, half-open.
    pub start: u64,
    pub end: u64,
}

impl NavigationEntry {
    pub fn new(contig_name: impl Into<String>, interval: GenomicInterval) -> Self {
        Self {
            contig_name: contig_name.into(),
            start: interval.start(),
            end: interval.end(),
        }
    }

    /// Re-expresses the entry in `dictionary`, clamped to the contig there.
    /// `None` when that dictionary has no such contig (see
    /// [`resolve_contig_name`](crate::resolve_contig_name)) or the entry lies
    /// wholly beyond its end.
    pub fn resolve(
        &self,
        dictionary: &crate::ReferenceDictionary,
        aliases: &crate::ContigAliases,
    ) -> Option<GenomicInterval> {
        let contig = crate::resolve_contig_name(dictionary, aliases, &self.contig_name)
            .ok()
            .flatten()?;
        let length = dictionary.get(contig)?.length();
        GenomicInterval::new(contig, self.start, self.end.min(length)).ok()
    }

    /// The entry under a caller-supplied id, for callers with no dictionary.
    pub fn interval(&self, contig: crate::ContigId) -> Option<GenomicInterval> {
        GenomicInterval::new(contig, self.start, self.end).ok()
    }
}

/// Back/forward history whose entries carry the contig name.
///
/// Behaves exactly like [`NavigationHistory`] (bounded, forward branch
/// discarded on a new visit, consecutive duplicates collapsed) except that two
/// visits are the same only if they name the same contig, whatever numeric id
/// each happened to carry.
#[derive(Clone, Debug)]
pub struct NamedNavigationHistory {
    entries: VecDeque<NavigationEntry>,
    cursor: usize,
    capacity: usize,
}

impl NamedNavigationHistory {
    pub fn new(initial: NavigationEntry) -> Self {
        Self::with_capacity(initial, DEFAULT_HISTORY_CAPACITY)
    }

    pub fn with_capacity(initial: NavigationEntry, capacity: usize) -> Self {
        let mut entries = VecDeque::new();
        entries.push_back(initial);
        Self {
            entries,
            cursor: 0,
            capacity: capacity.max(1),
        }
    }

    pub fn visit(&mut self, entry: NavigationEntry) {
        if *self.current() == entry {
            return;
        }
        self.entries.truncate(self.cursor.saturating_add(1));
        self.entries.push_back(entry);
        if self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
        self.cursor = self.entries.len().saturating_sub(1);
    }

    pub fn back(&mut self) -> Option<&NavigationEntry> {
        if !self.can_go_back() {
            return None;
        }
        self.cursor -= 1;
        self.entries.get(self.cursor)
    }

    pub fn forward(&mut self) -> Option<&NavigationEntry> {
        if !self.can_go_forward() {
            return None;
        }
        self.cursor += 1;
        self.entries.get(self.cursor)
    }

    pub fn current(&self) -> &NavigationEntry {
        &self.entries[self.cursor]
    }

    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// A history always holds at least its initial entry.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns a read-only view of retained entries without moving the cursor.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &NavigationEntry> {
        self.entries.iter()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn select(&mut self, index: usize) -> Option<&NavigationEntry> {
        self.entries.get(index)?;
        self.cursor = index;
        self.entries.get(index)
    }
}

#[cfg(test)]
mod tests {
    use crate::{ContigAliases, ContigId, ReferenceDictionary};

    use super::*;

    fn named(contig: &str, id: u32, start: u64) -> NavigationEntry {
        NavigationEntry::new(
            contig,
            GenomicInterval::new(ContigId::new(id), start, start + 10).unwrap(),
        )
    }

    #[test]
    fn id_only_history_conflates_contigs_that_named_history_keeps_apart() {
        // Without a dictionary every contig is id zero: the id-keyed history
        // treats chr2:0-10 as a revisit of chr1:0-10 and drops it.
        let mut by_id = NavigationHistory::new(interval(0));
        by_id.visit(interval(0));
        assert_eq!(by_id.len(), 1);

        let mut by_name = NamedNavigationHistory::new(named("chr1", 0, 0));
        by_name.visit(named("chr2", 0, 0));
        assert_eq!(by_name.len(), 2);
        assert_eq!(by_name.back().unwrap().contig_name, "chr1");
        assert_eq!(by_name.forward().unwrap().contig_name, "chr2");

        // The same contig under two dictionaries' ids is one place.
        by_name.visit(named("chr2", 7, 0));
        assert_eq!(by_name.len(), 2);
    }

    #[test]
    fn named_history_is_bounded_branches_and_selects() {
        let mut history = NamedNavigationHistory::with_capacity(named("chr1", 0, 0), 3);
        for start in [10, 20, 30] {
            history.visit(named("chr1", 0, start));
        }
        assert_eq!(history.len(), 3);
        assert!(!history.is_empty());
        assert_eq!(history.back().unwrap().start, 20);
        assert_eq!(history.back().unwrap().start, 10);
        assert!(history.back().is_none());
        history.visit(named("chrX", 3, 5_000_000_000));
        assert!(!history.can_go_forward());
        assert_eq!(history.cursor(), 1);
        assert_eq!(history.entries().len(), 2);
        assert_eq!(history.select(0).unwrap().start, 10);
        assert!(history.select(9).is_none());
        assert_eq!(history.current().start, 10);
    }

    #[test]
    fn entries_resolve_by_name_against_each_dictionary() {
        let assembly = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 1_000),
            ("chr2".to_owned(), 6_000_000_000),
        ]);
        let track = ReferenceDictionary::from_contigs([
            ("2".to_owned(), 6_000_000_000),
            ("1".to_owned(), 1_000),
        ]);
        let aliases = ContigAliases::default();
        let entry = named("chr2", 1, 5_000_000_000);
        let in_assembly = entry.resolve(&assembly, &aliases).unwrap();
        let in_track = entry.resolve(&track, &aliases).unwrap();
        assert_eq!(in_assembly.contig(), ContigId::new(1));
        assert_eq!(in_track.contig(), ContigId::new(0));
        assert_eq!(in_track.start(), 5_000_000_000);
        assert_eq!(in_track.len(), 10);

        // Clamped to the contig; unknown or out-of-range entries do not resolve.
        let clamped = named("chr1", 0, 995).resolve(&assembly, &aliases).unwrap();
        assert_eq!((clamped.start(), clamped.end()), (995, 1_000));
        assert!(
            named("chr1", 0, 2_000)
                .resolve(&assembly, &aliases)
                .is_none()
        );
        assert!(named("chr9", 0, 0).resolve(&assembly, &aliases).is_none());
        assert_eq!(
            entry.interval(ContigId::new(0)).unwrap().start(),
            5_000_000_000
        );
    }

    fn interval(start: u64) -> GenomicInterval {
        GenomicInterval::new(ContigId::new(0), start, start + 10).unwrap()
    }

    #[test]
    fn visiting_after_back_discards_forward_branch() {
        let mut history = NavigationHistory::new(interval(0));
        history.visit(interval(10));
        history.visit(interval(20));
        assert_eq!(history.back(), Some(interval(10)));
        history.visit(interval(30));
        assert!(!history.can_go_forward());
        assert_eq!(history.back(), Some(interval(10)));
    }

    #[test]
    fn history_is_bounded_and_deduplicated() {
        let mut history = NavigationHistory::with_capacity(interval(0), 3);
        history.visit(interval(0));
        history.visit(interval(10));
        history.visit(interval(20));
        history.visit(interval(30));
        assert_eq!(history.len(), 3);
        assert_eq!(history.back(), Some(interval(20)));
        assert_eq!(history.back(), Some(interval(10)));
        assert!(!history.can_go_back());
    }

    #[test]
    fn observation_does_not_move_the_cursor() {
        let mut history = NavigationHistory::new(interval(0));
        history.visit(interval(10));
        history.visit(interval(20));
        assert_eq!(history.back(), Some(interval(10)));
        let before = history.current();
        assert_eq!(history.entries().copied().collect::<Vec<_>>().len(), 3);
        assert_eq!(history.cursor(), 1);
        assert_eq!(history.current(), before);
        assert_eq!(history.select(2), Some(interval(20)));
    }
}
