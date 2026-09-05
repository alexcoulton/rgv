use std::sync::Arc;

use crate::AlignmentHandle;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentInteractionToken {
    pub hovered: bool,
    pub selected: bool,
    pub selected_chain: bool,
    pub highlighted: bool,
}

/// Camera- and page-independent interaction state keyed only by stable source handles.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AlignmentInteractionState {
    hovered: Option<AlignmentHandle>,
    selected: Option<AlignmentHandle>,
    selected_chain: Arc<[AlignmentHandle]>,
    highlighted: Arc<[AlignmentHandle]>,
}

impl AlignmentInteractionState {
    /// Export preserves persistent emphasis but never pointer hover.
    #[must_use]
    pub fn without_hover(&self) -> Self {
        let mut state = self.clone();
        state.hovered = None;
        state
    }

    #[must_use]
    pub fn new(
        hovered: Option<AlignmentHandle>,
        selected: Option<AlignmentHandle>,
        selected_chain: impl IntoIterator<Item = AlignmentHandle>,
    ) -> Self {
        let mut selected_chain = selected_chain.into_iter().collect::<Vec<_>>();
        selected_chain.sort_unstable();
        selected_chain.dedup();
        Self {
            hovered,
            selected,
            selected_chain: selected_chain.into(),
            highlighted: Arc::from([]),
        }
    }

    #[must_use]
    pub fn with_highlighted(
        mut self,
        highlighted: impl IntoIterator<Item = AlignmentHandle>,
    ) -> Self {
        let mut highlighted = highlighted.into_iter().collect::<Vec<_>>();
        highlighted.sort_unstable();
        highlighted.dedup();
        self.highlighted = highlighted.into();
        self
    }

    #[must_use]
    pub const fn hovered(&self) -> Option<AlignmentHandle> {
        self.hovered
    }

    #[must_use]
    pub const fn selected(&self) -> Option<AlignmentHandle> {
        self.selected
    }

    #[must_use]
    pub fn selected_chain(&self) -> &[AlignmentHandle] {
        &self.selected_chain
    }

    #[must_use]
    pub fn highlighted(&self) -> &[AlignmentHandle] {
        &self.highlighted
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hovered.is_none()
            && self.selected.is_none()
            && self.selected_chain.is_empty()
            && self.highlighted.is_empty()
    }

    #[must_use]
    pub fn token(&self, handle: AlignmentHandle) -> AlignmentInteractionToken {
        AlignmentInteractionToken {
            hovered: self.hovered == Some(handle),
            selected: self.selected == Some(handle),
            selected_chain: self.selected_chain.binary_search(&handle).is_ok(),
            highlighted: self.highlighted.binary_search(&handle).is_ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AlignmentSourceRevision, RevisionFingerprint, SourceFingerprint};
    use rgv_hts::AlignmentSourcePosition;

    fn handle(position: u64) -> AlignmentHandle {
        AlignmentHandle::new(
            AlignmentSourceRevision::new(
                SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
                RevisionFingerprint::new(3, 4),
            ),
            AlignmentSourcePosition::bam_virtual_offset(position),
        )
    }

    #[test]
    fn handle_overlay_state_is_sorted_deduplicated_and_page_independent() {
        let first = handle(1);
        let second = handle(2);
        let state =
            AlignmentInteractionState::new(Some(first), Some(second), [second, first, second]);
        assert_eq!(state.selected_chain(), &[first, second]);
        assert!(!state.is_empty());
        assert_eq!(
            state.token(first),
            AlignmentInteractionToken {
                hovered: true,
                selected: false,
                selected_chain: true,
                highlighted: false,
            }
        );
    }

    #[test]
    fn default_interaction_requires_no_overlay_scan() {
        assert!(AlignmentInteractionState::default().is_empty());
    }
}
