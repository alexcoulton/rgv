//! Which transcripts of a gene-model track are drawn, and how they are labelled.
//!
//! Sources mark every transcript of a multi-transcript gene with its place in the gene
//! ([`FeatureRecord::transcript_choice`]). One per gene draws the representative only, labelled
//! `GENE +N` with the number of other transcripts; a gene the user expanded draws every
//! transcript, each labelled with its transcript name. The protein-coding filter hides records
//! of other genes. Hidden records keep their indices (and so their object identities); they
//! are neither packed into rows nor drawn.

use rgv_core::{FeatureDisplayMode, FeatureTrackSettings, TranscriptDisplay};
use rgv_model::FeatureRecord;

/// Most characters of a drawn feature label.
pub const MAX_FEATURE_LABEL_CHARACTERS: usize = 32;

/// The presentation of one gene-model track's records, index for index.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeaturePresentation {
    hidden: Vec<bool>,
    labels: Vec<Option<String>>,
}

fn truncated(text: &str, characters: usize) -> String {
    text.chars().take(characters).collect()
}

/// `base` followed by `suffix`, shortening `base` so the whole fits the label limit.
fn with_suffix(base: &str, suffix: &str) -> String {
    let room = MAX_FEATURE_LABEL_CHARACTERS.saturating_sub(suffix.chars().count());
    format!("{}{suffix}", truncated(base, room))
}

/// The label of a representative drawn alone: `CLASP2 +11`.
#[must_use]
pub fn representative_label(base: &str, alternatives: usize) -> String {
    with_suffix(base, &format!(" +{alternatives}"))
}

/// The label of one transcript of an expanded gene: its transcript name when that names the
/// gene (`CLASP2-201`), else the gene and the transcript identifier.
fn expanded_label(record: &FeatureRecord, base: &str) -> Option<String> {
    let transcript_name = record
        .attributes
        .iter()
        .find(|(key, value)| key == "transcript_name" && !value.trim().is_empty())
        .map(|(_, value)| value.as_str());
    match (transcript_name, record.transcript_identifier()) {
        (Some(name), _) if name.starts_with(base) => {
            Some(truncated(name, MAX_FEATURE_LABEL_CHARACTERS))
        }
        (Some(name), _) | (None, Some(name)) if name != base => {
            Some(with_suffix(base, &format!(" {name}")))
        }
        _ => None,
    }
}

impl FeaturePresentation {
    /// The presentation of a gene-model track's `records` under its `settings`. One per gene is
    /// not applied in the collapsed display mode, which overlays every transcript in one row;
    /// the protein-coding filter is.
    #[must_use]
    pub fn gene_model(records: &[FeatureRecord], settings: &FeatureTrackSettings) -> Self {
        let one_per_gene = settings.transcripts == TranscriptDisplay::OnePerGene
            && settings.display_mode != FeatureDisplayMode::Collapsed;
        let mut presentation = Self {
            hidden: vec![false; records.len()],
            labels: vec![None; records.len()],
        };
        for (index, record) in records.iter().enumerate() {
            if settings.protein_coding_only && !record.is_protein_coding() {
                presentation.hidden[index] = true;
                continue;
            }
            if !one_per_gene {
                continue;
            }
            let Some(choice) = record.transcript_choice() else {
                continue;
            };
            let base = record.display_label().unwrap_or_default();
            if settings.expanded_genes.contains(choice.gene_key) {
                presentation.labels[index] = expanded_label(record, base);
            } else if choice.is_representative() {
                presentation.labels[index] =
                    Some(representative_label(base, choice.alternatives()));
            } else {
                presentation.hidden[index] = true;
            }
        }
        presentation
    }

    /// Whether record `index` is left out of the rows and the drawing.
    #[must_use]
    pub fn is_hidden(&self, index: usize) -> bool {
        self.hidden.get(index).copied().unwrap_or(false)
    }

    /// The label record `index` is drawn with (before the layout's own truncation), when it
    /// differs from the record's display label.
    #[must_use]
    pub fn label(&self, index: usize) -> Option<&str> {
        self.labels.get(index).and_then(Option::as_deref)
    }

    /// The text record `index` of `records` is labelled with, at most
    /// [`MAX_FEATURE_LABEL_CHARACTERS`] characters.
    #[must_use]
    pub fn label_text(
        presentation: Option<&Self>,
        records: &[FeatureRecord],
        index: usize,
    ) -> String {
        presentation
            .and_then(|presentation| presentation.label(index))
            .or_else(|| records.get(index).and_then(FeatureRecord::display_label))
            .map(|label| truncated(label, MAX_FEATURE_LABEL_CHARACTERS))
            .unwrap_or_default()
    }

    /// Records drawn.
    #[must_use]
    pub fn shown(&self) -> usize {
        self.hidden.iter().filter(|hidden| !**hidden).count()
    }
}

#[cfg(test)]
#[path = "feature_presentation_tests.rs"]
mod tests;
