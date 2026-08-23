use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
pub struct ViewportGeneration(u64);

impl ViewportGeneration {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Shared cancellation clock. Work is current only while its generation matches the clock.
#[derive(Clone, Debug, Default)]
pub struct GenerationClock(Arc<AtomicU64>);

impl GenerationClock {
    pub fn current(&self) -> ViewportGeneration {
        ViewportGeneration::new(self.0.load(Ordering::Acquire))
    }

    pub fn advance(&self) -> ViewportGeneration {
        ViewportGeneration::new(self.0.fetch_add(1, Ordering::AcqRel) + 1)
    }

    pub fn is_current(&self, generation: ViewportGeneration) -> bool {
        self.current() == generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advancing_invalidates_old_work() {
        let clock = GenerationClock::default();
        let old = clock.current();
        let new = clock.advance();
        assert!(!clock.is_current(old));
        assert!(clock.is_current(new));
    }
}
