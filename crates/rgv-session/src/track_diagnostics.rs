//! Source diagnostics of annotation tracks, reported to the user once per track load.

use rgv_core::TrackId;
use std::collections::BTreeSet;

/// Reports a text track's skipped (malformed) lines once per track load. Indexed sources keep
/// counting as queries meet more of the file, so later, larger counts for the same track are
/// not reported again.
#[derive(Debug, Default)]
pub struct SkippedLineReporter {
    reported: BTreeSet<TrackId>,
}

impl SkippedLineReporter {
    /// `malformed_lines` were skipped in `track` so far; `first` is the one-based line number
    /// and parse error of the first. Returns the warning to log the first time a track reports
    /// any.
    pub fn observe(
        &mut self,
        track: TrackId,
        track_name: &str,
        malformed_lines: usize,
        first: Option<&(usize, String)>,
    ) -> Option<String> {
        if malformed_lines == 0 || !self.reported.insert(track) {
            return None;
        }
        let lines = if malformed_lines == 1 {
            "line"
        } else {
            "lines"
        };
        Some(match first {
            Some((line, error)) => format!(
                "{track_name}: {malformed_lines} malformed {lines} skipped, first at line {line} \
                 ({error})"
            ),
            None => format!("{track_name}: {malformed_lines} malformed {lines} skipped"),
        })
    }

    /// Forget every track: the engine (and with it every track load) was replaced.
    pub fn reset(&mut self) {
        self.reported.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_lines_are_reported_once_per_track_load() {
        let (bed, gff) = (TrackId::new(4), TrackId::new(5));
        let first = (17_usize, "expected 3 columns".to_owned());
        let mut reporter = SkippedLineReporter::default();
        assert_eq!(reporter.observe(bed, "peaks.bed", 0, None), None);
        assert_eq!(
            reporter
                .observe(bed, "peaks.bed", 3, Some(&first))
                .as_deref(),
            Some("peaks.bed: 3 malformed lines skipped, first at line 17 (expected 3 columns)")
        );
        // Later events, and a count that grows as an indexed source is queried, stay silent.
        assert_eq!(reporter.observe(bed, "peaks.bed", 3, Some(&first)), None);
        assert_eq!(reporter.observe(bed, "peaks.bed", 40, Some(&first)), None);
        // Another track reports for itself.
        assert_eq!(
            reporter.observe(gff, "genes.gff", 1, None).as_deref(),
            Some("genes.gff: 1 malformed line skipped")
        );
        // Replacing the engine starts every track load afresh. (Track ids are never reused
        // within one engine's lifetime, so a re-added track has a new id.)
        reporter.reset();
        assert!(
            reporter
                .observe(bed, "peaks.bed", 3, Some(&first))
                .is_some()
        );
        assert!(reporter.observe(gff, "genes.gff", 1, None).is_some());
    }
}
