//! Loading tracks as an action.
//!
//! Every way of adding a source (the Open dialogs, the remote-source dialog, drag and drop, the
//! recent-files menu and the automation protocol's `load_track`) phrases it as
//! [`Action::LoadTracks`]. The host opens each source through the one source-open and engine
//! spawn path (`Session::add_track_source` for annotation and signal tracks, the alignment
//! controller spawn for BAM/CRAM).
//!
//! Undo removes the loaded tracks, which retires their engines; the undo entry keeps only their
//! configuration, never an engine or file handle. Redo reopens them from that configuration
//! (`Action::RestoreTracks`), with the same identity and place, so later steps that name the
//! track still find it. A reference FASTA replaces the workspace genome rather than adding a
//! track: it is loaded through the same action but is not an undo step, because undoing it
//! would mean reopening the previous reference and re-resolving every panel against it, which
//! is a session replacement, not an edit.

use rgv_core::{TrackKind, TrackSource};

/// What kind of source a load opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackLoadKind {
    /// A BAM or CRAM alignment track, with an optional non-conventional index URL.
    Alignment { remote_index_url: Option<String> },
    /// An annotation, variant, interaction or signal track; the format is inferred from the
    /// file name.
    Annotation,
    /// A FASTA or 2bit that becomes the workspace reference.
    Reference { remote_index_url: Option<String> },
}

/// One source to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackLoad {
    pub source: TrackSource,
    pub kind: TrackLoadKind,
    /// A display name instead of the one derived from the source.
    pub name: Option<String>,
}

impl TrackLoad {
    /// A source of an already classified track kind, as `load_track` names it.
    pub fn of_kind(source: TrackSource, kind: TrackKind, name: Option<String>) -> Self {
        Self {
            source,
            kind: match kind {
                TrackKind::Alignment => TrackLoadKind::Alignment {
                    remote_index_url: None,
                },
                TrackKind::Reference => TrackLoadKind::Reference {
                    remote_index_url: None,
                },
                _ => TrackLoadKind::Annotation,
            },
            name,
        }
    }
}
