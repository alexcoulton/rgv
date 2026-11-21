#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiCommand {
    OpenAlignment,
    OpenUrl,
    AddTrack,
    OpenSession,
    SaveSession,
    ExportPng,
    ClearCache,
    Settings,
    FocusLocus,
    OpenTrackSettings,
    ToggleTrackVisibility,
    ToggleTrackCollapsed,
    PreviousEvidence,
    NextEvidence,
    PreviousPanel,
    NextPanel,
    Metadata,
    Regions,
    ReviewQueue,
    Undo,
    Redo,
}

pub const SEARCHABLE_COMMANDS: &[(&str, UiCommand)] = &[
    ("Open BAM/CRAM", UiCommand::OpenAlignment),
    ("Open remote URL", UiCommand::OpenUrl),
    ("Add genomic track", UiCommand::AddTrack),
    ("Open RGV or IGV session", UiCommand::OpenSession),
    ("Save RGV session", UiCommand::SaveSession),
    ("Export PNG image", UiCommand::ExportPng),
    ("Clear caches", UiCommand::ClearCache),
    ("Open settings", UiCommand::Settings),
    ("Focus locus field", UiCommand::FocusLocus),
    ("Open selected track settings", UiCommand::OpenTrackSettings),
    (
        "Toggle selected track visibility",
        UiCommand::ToggleTrackVisibility,
    ),
    (
        "Toggle selected track collapsed",
        UiCommand::ToggleTrackCollapsed,
    ),
    (
        "Select previous loaded evidence",
        UiCommand::PreviousEvidence,
    ),
    ("Select next loaded evidence", UiCommand::NextEvidence),
    ("Activate previous view panel", UiCommand::PreviousPanel),
    ("Activate next view panel", UiCommand::NextPanel),
    ("Open sample metadata", UiCommand::Metadata),
    ("Open regions of interest", UiCommand::Regions),
    ("Open review queue", UiCommand::ReviewQueue),
    ("Undo last edit", UiCommand::Undo),
    ("Redo last undone edit", UiCommand::Redo),
];

pub fn command_matches(label: &str, search: &str) -> bool {
    search.is_empty()
        || label
            .to_ascii_lowercase()
            .contains(&search.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displaced_toolbar_commands_remain_searchable() {
        assert!(
            SEARCHABLE_COMMANDS
                .iter()
                .any(|(_, command)| *command == UiCommand::OpenAlignment)
        );
        assert!(
            SEARCHABLE_COMMANDS
                .iter()
                .any(|(_, command)| *command == UiCommand::OpenUrl)
        );
        assert!(
            SEARCHABLE_COMMANDS
                .iter()
                .any(|(_, command)| *command == UiCommand::AddTrack)
        );
        assert!(
            SEARCHABLE_COMMANDS
                .iter()
                .any(|(_, command)| *command == UiCommand::ExportPng)
        );
        assert!(
            SEARCHABLE_COMMANDS
                .iter()
                .any(|(_, command)| *command == UiCommand::ClearCache)
        );
        assert!(command_matches("Open settings", "SET"));
        assert!(!command_matches("Open settings", "export"));
        for required in [
            UiCommand::OpenTrackSettings,
            UiCommand::PreviousEvidence,
            UiCommand::NextEvidence,
            UiCommand::PreviousPanel,
            UiCommand::NextPanel,
            UiCommand::Metadata,
            UiCommand::Regions,
            UiCommand::ReviewQueue,
        ] {
            assert!(
                SEARCHABLE_COMMANDS
                    .iter()
                    .any(|(_, command)| *command == required)
            );
        }
    }
}
