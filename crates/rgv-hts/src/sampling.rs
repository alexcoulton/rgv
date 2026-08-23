use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap},
};

/// Matches IGV's default short-read downsampling window and per-window cap.
pub(crate) const WINDOW_BASES: u64 = 50;
pub(crate) const RECORDS_PER_WINDOW: usize = 100;

struct Scored<T> {
    score: u64,
    serial: usize,
    value: T,
}

impl<T> PartialEq for Scored<T> {
    fn eq(&self, other: &Self) -> bool {
        (self.score, self.serial) == (other.score, other.serial)
    }
}

impl<T> Eq for Scored<T> {}

impl<T> PartialOrd for Scored<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Scored<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.score, self.serial).cmp(&(other.score, other.serial))
    }
}

/// Deterministically retains the best-scoring records in fixed genomic
/// windows. Adding records in a different window does not change an existing
/// window's selection unless the global safety bound is reached.
pub(crate) struct WindowedSampler<T> {
    windows: BTreeMap<u64, BinaryHeap<Scored<T>>>,
    maximum_records: usize,
    records_per_window: usize,
}

impl<T> WindowedSampler<T> {
    pub(crate) fn new(maximum_records: usize, records_per_window: usize) -> Self {
        Self {
            windows: BTreeMap::new(),
            maximum_records: maximum_records.max(1),
            records_per_window: records_per_window.max(1),
        }
    }

    pub(crate) fn would_retain(&self, window: u64, score: u64, serial: usize) -> bool {
        self.windows.get(&window).is_none_or(|records| {
            records.len() < self.records_per_window
                || records
                    .peek()
                    .is_some_and(|largest| (score, serial) < (largest.score, largest.serial))
        })
    }

    pub(crate) fn insert(&mut self, window: u64, score: u64, serial: usize, value: T) {
        let records = self.windows.entry(window).or_default();
        if records.len() == self.records_per_window {
            records.pop();
        }
        records.push(Scored {
            score,
            serial,
            value,
        });
    }

    pub(crate) fn into_values(self) -> Vec<T> {
        let retained = self.windows.into_values().flat_map(BinaryHeap::into_vec);
        let mut bounded = BinaryHeap::with_capacity(self.maximum_records);
        for record in retained {
            if bounded.len() < self.maximum_records {
                bounded.push(record);
            } else if bounded.peek().is_some_and(|largest| record < *largest) {
                bounded.pop();
                bounded.push(record);
            }
        }
        bounded
            .into_vec()
            .into_iter()
            .map(|record| record.value)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighboring_windows_do_not_displace_an_existing_window() {
        let mut sampler = WindowedSampler::new(10, 2);
        for (serial, score) in [9, 2, 7].into_iter().enumerate() {
            if sampler.would_retain(10, score, serial) {
                sampler.insert(10, score, serial, (10, score));
            }
        }
        for (offset, score) in [8, 1, 6].into_iter().enumerate() {
            let serial = offset + 3;
            if sampler.would_retain(11, score, serial) {
                sampler.insert(11, score, serial, (11, score));
            }
        }
        let mut values = sampler.into_values();
        values.sort_unstable();
        assert_eq!(values, vec![(10, 2), (10, 7), (11, 1), (11, 6)]);
    }

    #[test]
    fn global_safety_bound_remains_hard() {
        let mut sampler = WindowedSampler::new(3, 2);
        for serial in 0..10 {
            let window = serial as u64 / 2;
            sampler.insert(window, serial as u64, serial, serial);
        }
        assert_eq!(sampler.into_values().len(), 3);
    }
}
