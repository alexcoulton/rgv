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

#[cfg(test)]
mod tests {
    use crate::ContigId;

    use super::*;

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
