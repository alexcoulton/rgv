use crate::{
    AlignmentGpuPreparedAuthority, AlignmentGpuSpeculationGate, AlignmentGpuSpeculationPhase,
};

impl AlignmentGpuSpeculationGate {
    pub(super) fn observe_foreground(
        &mut self,
        authority: AlignmentGpuPreparedAuthority,
        budget_bytes: usize,
    ) {
        let authority_changed = self.authority.as_ref() != Some(&authority);
        let budget_increased = budget_bytes > self.budget_bytes;
        let budget_decreased = self.budget_bytes > 0 && budget_bytes < self.budget_bytes;
        self.authority = Some(authority);
        self.budget_bytes = budget_bytes;
        if budget_decreased {
            self.phase = AlignmentGpuSpeculationPhase::Suppressed;
        } else if authority_changed || budget_increased {
            self.phase = AlignmentGpuSpeculationPhase::Eligible;
        }
    }

    pub(super) const fn allows_prefetch(&self) -> bool {
        matches!(
            self.phase,
            AlignmentGpuSpeculationPhase::Eligible | AlignmentGpuSpeculationPhase::Warming
        )
    }

    pub(super) const fn suppress_for_foreground_pressure(&mut self) {
        self.phase = AlignmentGpuSpeculationPhase::Suppressed;
    }

    pub(super) const fn finish_cycle(
        &mut self,
        missing_pages_remain: bool,
        retention_limited: bool,
    ) {
        if matches!(self.phase, AlignmentGpuSpeculationPhase::Suppressed) {
            return;
        }
        self.phase = if missing_pages_remain {
            AlignmentGpuSpeculationPhase::Warming
        } else if retention_limited {
            AlignmentGpuSpeculationPhase::DemandPaged
        } else {
            AlignmentGpuSpeculationPhase::Resident
        };
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}
