use rgv_core::SessionDocument;

/// File-job identity is deliberately separate from the control protocol's
/// workspace/evidence revisions and navigation query generations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DocumentTicket {
    pub(crate) epoch: u64,
    pub(crate) revision: u64,
    pub(crate) request: u64,
}

#[derive(Default)]
pub(crate) struct DocumentState {
    epoch: u64,
    revision: u64,
    saved_revision: Option<u64>,
    next_request: u64,
    latest_load: u64,
    last_applied_save: u64,
    document: Option<SessionDocument>,
}

impl DocumentState {
    /// Observe the canonical persisted projection at a UI/command boundary.
    /// Comparing values covers native controls without treating selection,
    /// hover, failed drafts or redundant redraws as document edits.
    pub(crate) fn observe(&mut self, document: SessionDocument) -> bool {
        if self.document.as_ref() == Some(&document) {
            return false;
        }
        self.document = Some(document);
        self.revision += 1;
        true
    }

    pub(crate) fn replace(&mut self, document: SessionDocument, saved: bool) {
        self.epoch += 1;
        self.revision = 1;
        self.saved_revision = saved.then_some(1);
        self.document = Some(document);
        self.latest_load = 0;
        self.last_applied_save = 0;
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.saved_revision != Some(self.revision)
    }

    pub(crate) fn mark_saved_baseline(&mut self) {
        self.saved_revision = Some(self.revision);
    }

    pub(crate) fn cancel_load(&mut self, ticket: DocumentTicket) {
        if self.current_load(ticket) {
            self.latest_load = 0;
        }
    }

    pub(crate) fn ticket(&mut self) -> DocumentTicket {
        self.next_request += 1;
        DocumentTicket {
            epoch: self.epoch,
            revision: self.revision,
            request: self.next_request,
        }
    }

    pub(crate) fn begin_load(&mut self) -> DocumentTicket {
        let ticket = self.ticket();
        self.latest_load = ticket.request;
        ticket
    }

    pub(crate) fn current_load(&self, ticket: DocumentTicket) -> bool {
        ticket.epoch == self.epoch && ticket.request == self.latest_load
    }

    pub(crate) fn load_can_commit(&self, ticket: DocumentTicket) -> bool {
        self.current_load(ticket) && ticket.revision == self.revision
    }

    pub(crate) fn complete_save(&mut self, ticket: DocumentTicket) -> bool {
        if ticket.epoch != self.epoch || ticket.request < self.last_applied_save {
            return false;
        }
        self.last_applied_save = ticket.request;
        self.saved_revision = Some(ticket.revision);
        true
    }
}

impl super::super::Viewer {
    pub(crate) fn observe_document_changes(&mut self) {
        let document = self.session_document();
        self.document_state.observe(document);
    }

    pub(crate) fn replace_document_identity(&mut self, saved: bool) {
        let document = self.session_document();
        self.document_state.replace(document, saved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::SessionLocus;

    fn document() -> SessionDocument {
        SessionDocument::new(SessionLocus {
            contig: "chr1".into(),
            start: 0,
            end: 100,
        })
    }

    #[test]
    fn canonical_persisted_edits_are_dirty_but_no_ops_are_not() {
        let mut state = DocumentState::default();
        let mut doc = document();
        state.replace(doc.clone(), true);
        assert!(!state.observe(doc.clone()));
        assert!(!state.is_dirty());
        doc.genome_name = Some("native edit".into());
        assert!(state.observe(doc.clone()));
        assert!(state.is_dirty());
        let save = state.ticket();
        doc.locus.start += 1;
        state.observe(doc);
        assert!(state.complete_save(save));
        assert!(
            state.is_dirty(),
            "a save records its snapshot revision, not the current one"
        );
    }

    #[test]
    fn completion_order_and_epoch_cannot_change_newer_save_state() {
        let mut state = DocumentState::default();
        state.replace(document(), false);
        let save_a = state.ticket();
        let save_b = state.ticket();
        assert!(state.complete_save(save_b));
        assert!(!state.complete_save(save_a));
        state.replace(document(), false);
        assert!(!state.complete_save(save_b));
        assert!(state.is_dirty());
    }

    #[test]
    fn new_load_even_if_it_fails_permanently_supersedes_old_load() {
        let mut state = DocumentState::default();
        state.replace(document(), true);
        let a = state.begin_load();
        let b = state.begin_load();
        assert!(!state.load_can_commit(a));
        assert!(state.load_can_commit(b));
        let mut edited = document();
        edited.genome_name = Some("edit during load".into());
        state.observe(edited);
        assert!(!state.load_can_commit(b));
        state.replace(document(), true);
        assert!(!state.current_load(b));
    }
}
