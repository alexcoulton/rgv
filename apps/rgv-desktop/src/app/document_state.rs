pub(crate) use rgv_session::document_state::{DocumentState, DocumentTicket};

impl crate::Viewer {
    /// All dirty-state readers and file completion gates call this first, so
    /// native edits are included without serializing/cloning state on redraws.
    pub(crate) fn observe_document_changes(&mut self) {
        let document = self.session_document();
        self.session.document_state.observe(document);
    }

    pub(crate) fn replace_document_identity(&mut self, saved: bool) {
        // Undo steps describe the document that was just replaced.
        self.session.undo.clear();
        let document = self.session_document();
        self.session.document_state.replace(document, saved);
    }
}
