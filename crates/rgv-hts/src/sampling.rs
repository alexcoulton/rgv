use std::{cmp::Ordering, collections::BinaryHeap};

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

/// Retains every record until the explicit query safety bound is reached.
///
/// Once that bound is exceeded, deterministic scoring makes the retained set
/// independent of file iteration order. This is an emergency resource bound,
/// not a display-density policy: ordinary dense loci must not lose records
/// merely because many reads begin in the same genomic window.
pub(crate) struct BoundedSampler<T> {
    records: BinaryHeap<Scored<T>>,
    maximum_records: usize,
}

impl<T> BoundedSampler<T> {
    pub(crate) fn new(maximum_records: usize) -> Self {
        Self {
            records: BinaryHeap::with_capacity(maximum_records.max(1)),
            maximum_records: maximum_records.max(1),
        }
    }

    pub(crate) fn would_retain(&self, score: u64, serial: usize) -> bool {
        self.records.len() < self.maximum_records
            || self
                .records
                .peek()
                .is_some_and(|largest| (score, serial) < (largest.score, largest.serial))
    }

    pub(crate) fn insert(&mut self, score: u64, serial: usize, value: T) {
        if self.records.len() == self.maximum_records {
            self.records.pop();
        }
        self.records.push(Scored {
            score,
            serial,
            value,
        });
    }

    pub(crate) fn into_values(self) -> Vec<T> {
        self.records
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
    fn dense_locus_retains_every_record_below_the_safety_bound() {
        let mut sampler = BoundedSampler::new(1_000);
        for serial in 0..750 {
            let score = serial as u64;
            assert!(sampler.would_retain(score, serial));
            sampler.insert(score, serial, serial);
        }
        let mut values = sampler.into_values();
        values.sort_unstable();
        assert_eq!(values, (0..750).collect::<Vec<_>>());
    }

    #[test]
    fn global_safety_bound_is_hard_and_deterministic() {
        let mut sampler = BoundedSampler::new(3);
        for serial in 0..10 {
            let score = (9 - serial) as u64;
            if sampler.would_retain(score, serial) {
                sampler.insert(score, serial, serial);
            }
        }
        let mut values = sampler.into_values();
        values.sort_unstable();
        assert_eq!(values, vec![7, 8, 9]);
    }
}
