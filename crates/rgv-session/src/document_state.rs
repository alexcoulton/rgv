//! Document identity and dirty tracking, and the autosave bookkeeping keyed on it (APP-4).

use rgv_core::SessionDocument;

/// File-job identity is deliberately separate from the control protocol's
/// workspace/evidence revisions and navigation query generations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentTicket {
    pub epoch: u64,
    pub revision: u64,
    pub request: u64,
}

#[derive(Default)]
pub struct DocumentState {
    epoch: u64,
    revision: u64,
    saved_revision: Option<u64>,
    next_request: u64,
    latest_load: u64,
    last_applied_save: u64,
    document: Option<SessionDocument>,
}

impl DocumentState {
    /// Observe the canonical persisted projection before a document operation
    /// starts or commits. Ordinary redraws must not clone large metadata tables.
    /// Comparing values covers native controls without treating selection,
    /// hover, failed drafts or redundant redraws as document edits.
    pub fn observe(&mut self, document: SessionDocument) -> bool {
        if self.document.as_ref() == Some(&document) {
            return false;
        }
        self.document = Some(document);
        self.revision += 1;
        true
    }

    pub fn replace(&mut self, document: SessionDocument, saved: bool) {
        self.epoch += 1;
        self.revision = 1;
        self.saved_revision = saved.then_some(1);
        self.document = Some(document);
        self.latest_load = 0;
        self.last_applied_save = 0;
    }

    pub fn is_dirty(&self) -> bool {
        self.saved_revision != Some(self.revision)
    }

    pub fn mark_saved_baseline(&mut self) {
        self.saved_revision = Some(self.revision);
    }

    pub fn cancel_load(&mut self, ticket: DocumentTicket) {
        if self.current_load(ticket) {
            self.latest_load = 0;
        }
    }

    pub fn ticket(&mut self) -> DocumentTicket {
        self.next_request += 1;
        DocumentTicket {
            epoch: self.epoch,
            revision: self.revision,
            request: self.next_request,
        }
    }

    pub fn begin_load(&mut self) -> DocumentTicket {
        let ticket = self.ticket();
        self.latest_load = ticket.request;
        ticket
    }

    pub fn current_load(&self, ticket: DocumentTicket) -> bool {
        ticket.epoch == self.epoch && ticket.request == self.latest_load
    }

    pub fn load_can_commit(&self, ticket: DocumentTicket) -> bool {
        self.current_load(ticket) && ticket.revision == self.revision
    }

    /// Commit rule for a load the user started from the GUI. The unsaved-changes gate has
    /// already resolved what happens to the current document, so revision bumps that happen
    /// while the file is read (finished track loads, navigation, hover-driven persisted state)
    /// must not reject the open. Only a newer load or a replaced document supersedes it.
    pub fn gui_load_can_commit(&self, ticket: DocumentTicket) -> bool {
        self.current_load(ticket)
    }

    /// Identity of the current document content for autosave bookkeeping.
    pub fn autosave_marker(&self) -> AutosaveMarker {
        AutosaveMarker {
            epoch: self.epoch,
            revision: self.revision,
        }
    }

    pub fn complete_save(&mut self, ticket: DocumentTicket) -> bool {
        if ticket.epoch != self.epoch || ticket.request < self.last_applied_save {
            return false;
        }
        self.last_applied_save = ticket.request;
        self.saved_revision = Some(ticket.revision);
        true
    }
}

/// Identity of the document content most recently written by autosave.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AutosaveMarker {
    pub epoch: u64,
    pub revision: u64,
}

/// Autosave only when the document has unsaved edits that the last autosave did not capture.
pub fn autosave_needed(
    document_dirty: bool,
    current: AutosaveMarker,
    last_autosaved: Option<AutosaveMarker>,
) -> bool {
    document_dirty && last_autosaved != Some(current)
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
    fn gui_opens_survive_background_revisions_but_not_newer_loads() {
        let mut state = DocumentState::default();
        let mut doc = document();
        state.replace(doc.clone(), true);
        let load = state.begin_load();
        doc.locus.end += 10;
        assert!(state.observe(doc), "a background change bumps the revision");
        assert!(
            !state.load_can_commit(load),
            "automation keeps the strict revision check"
        );
        assert!(state.gui_load_can_commit(load));
        let newer = state.begin_load();
        assert!(!state.gui_load_can_commit(load));
        assert!(state.gui_load_can_commit(newer));
        state.replace(document(), true);
        assert!(!state.gui_load_can_commit(newer));
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

    #[test]
    fn autosave_runs_only_for_edits_it_has_not_already_captured() {
        let first = AutosaveMarker {
            epoch: 1,
            revision: 4,
        };
        assert!(!autosave_needed(false, first, None), "clean document");
        assert!(autosave_needed(true, first, None));
        assert!(
            !autosave_needed(true, first, Some(first)),
            "an unchanged dirty document is not rewritten and cannot rotate out older snapshots"
        );
        let edited = AutosaveMarker {
            epoch: 1,
            revision: 5,
        };
        assert!(autosave_needed(true, edited, Some(first)));
        let reopened = AutosaveMarker {
            epoch: 2,
            revision: 4,
        };
        assert!(autosave_needed(true, reopened, Some(first)));
    }
}
