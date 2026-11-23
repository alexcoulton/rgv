//! Expanding one gene of a gene-model track drawn one transcript per gene into every
//! transcript, and folding it back.
//!
//! Clicking a gene's representative transcript toggles its gene; the feature's right-click
//! menu and the properties panel offer the same toggle by name. The expanded genes are part of
//! the track's settings, so they are saved with the session and undone like any setting.

use rgv_core::{
    FeatureDisplayMode, FeatureTrackSettings, MAX_EXPANDED_GENES, TrackId, TrackKind,
    TranscriptDisplay,
};
use rgv_engine::TrackSnapshotData;

use crate::Viewer;
use crate::action::Action;
use crate::inspection::model::FeatureSelection;

/// A multi-transcript gene of a track that shows one transcript per gene.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GeneTranscriptsTarget {
    pub(crate) track_id: TrackId,
    pub(crate) gene_key: String,
    /// The gene's label, as the track draws it.
    pub(crate) gene_label: String,
    /// Whether the gene is drawn with every transcript now.
    pub(crate) expanded: bool,
    /// Whether the record the target was taken from is the gene's representative.
    pub(crate) representative: bool,
}

/// The right-click menu of a gene-model feature.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FeatureContextMenuState {
    pub(crate) target: GeneTranscriptsTarget,
    pub(crate) screen_position: egui::Pos2,
}

/// Whether a track's settings fold multi-transcript genes to one transcript at all.
pub(crate) fn folds_transcripts(kind: TrackKind, settings: &FeatureTrackSettings) -> bool {
    kind == TrackKind::GeneModel
        && settings.transcripts == TranscriptDisplay::OnePerGene
        && settings.display_mode != FeatureDisplayMode::Collapsed
}

/// `settings` with one gene expanded to every transcript, or folded back to one. The number of
/// expanded genes is bounded; past the bound the oldest by name are folded first.
pub(crate) fn with_gene_expanded(
    settings: &FeatureTrackSettings,
    gene_key: &str,
    expanded: bool,
) -> FeatureTrackSettings {
    let mut next = settings.clone();
    if expanded {
        next.expanded_genes.insert(gene_key.to_owned());
        while next.expanded_genes.len() > MAX_EXPANDED_GENES {
            let Some(first) = next
                .expanded_genes
                .iter()
                .find(|key| key.as_str() != gene_key)
                .cloned()
            else {
                break;
            };
            next.expanded_genes.remove(&first);
        }
    } else {
        next.expanded_genes.remove(gene_key);
    }
    next
}

/// The target of `record` on a track with `kind` and `settings`, when the record belongs to a
/// gene with more than one transcript and the track folds them.
pub(crate) fn gene_transcripts_target(
    track_id: TrackId,
    kind: TrackKind,
    settings: &FeatureTrackSettings,
    record: &rgv_hts::FeatureRecord,
) -> Option<GeneTranscriptsTarget> {
    if !folds_transcripts(kind, settings) {
        return None;
    }
    let choice = record.transcript_choice()?;
    Some(GeneTranscriptsTarget {
        track_id,
        gene_key: choice.gene_key.to_owned(),
        gene_label: record.display_label().unwrap_or(choice.gene_key).to_owned(),
        expanded: settings.expanded_genes.contains(choice.gene_key),
        representative: choice.is_representative(),
    })
}

impl Viewer {
    /// The gene of a selected feature in the active panel, when its track folds transcripts.
    pub(crate) fn gene_transcripts_target_of(
        &self,
        selection: FeatureSelection,
    ) -> Option<GeneTranscriptsTarget> {
        let track = self
            .session
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == selection.track_id)?;
        let record = self
            .session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Features(records) => records.get(selection.record_index),
                _ => None,
            })?;
        gene_transcripts_target(track.id, track.kind, &track.feature, record)
    }

    /// Draws one gene of a track with every transcript, or with its representative only, as
    /// one undoable track-settings change.
    pub(crate) fn set_gene_transcripts_expanded(
        &mut self,
        track_id: TrackId,
        gene_key: &str,
        expanded: bool,
    ) {
        let Some(track) = self
            .session
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
        else {
            return;
        };
        let mut config = track.clone();
        config.feature = with_gene_expanded(&track.feature, gene_key, expanded);
        if config.feature == track.feature {
            return;
        }
        if let Err(error) = self.apply_action(Action::SetTrackConfigs {
            tracks: vec![config],
        }) {
            self.report_error(error.to_string());
        }
    }

    /// After a click selected `selection`: a click on a gene's representative expands the
    /// gene, and a click on the representative of an expanded gene folds it again.
    pub(crate) fn toggle_gene_transcripts_on_click(&mut self, selection: FeatureSelection) {
        if let Some(target) = self
            .gene_transcripts_target_of(selection)
            .filter(|target| target.representative)
        {
            self.set_gene_transcripts_expanded(target.track_id, &target.gene_key, !target.expanded);
        }
    }

    /// Opens the right-click menu of the gene-model feature under the cursor; false when there
    /// is no folded multi-transcript gene there.
    pub(crate) fn open_feature_context_menu_at_cursor(&mut self) -> bool {
        if !self.cursor_over_unobstructed_genomic_canvas() {
            return false;
        }
        let Some(target) = self
            .feature_at_cursor()
            .and_then(|selection| self.gene_transcripts_target_of(selection))
        else {
            return false;
        };
        let scale = self.window.scale_factor().max(f64::EPSILON);
        self.feature_context_menu = Some(FeatureContextMenuState {
            target,
            screen_position: egui::pos2(
                (self.cursor.x / scale) as f32,
                (self.cursor.y / scale) as f32,
            ),
        });
        self.window.request_redraw();
        true
    }
}

/// Identifiers of the transcripts in `records` of the gene `record` belongs to, representative
/// first; empty for a record of a single-transcript gene.
pub(crate) fn gene_transcript_identifiers(
    records: &[rgv_hts::FeatureRecord],
    record: &rgv_hts::FeatureRecord,
) -> Vec<String> {
    let Some(choice) = record.transcript_choice() else {
        return Vec::new();
    };
    let mut transcripts = records
        .iter()
        .filter_map(|candidate| {
            let candidate_choice = candidate.transcript_choice()?;
            (candidate_choice.gene_key == choice.gene_key
                && candidate.contig == record.contig
                && candidate_choice.representative == choice.representative)
                .then(|| {
                    (
                        candidate_choice.rank,
                        candidate
                            .transcript_identifier()
                            .unwrap_or("unnamed")
                            .to_owned(),
                    )
                })
        })
        .collect::<Vec<_>>();
    transcripts.sort();
    transcripts.dedup();
    transcripts
        .into_iter()
        .take(crate::inspection::model::MAX_INSPECTOR_GENE_TRANSCRIPTS)
        .map(|(_, identifier)| identifier)
        .collect()
}

/// Why a search hit on a gene-model track is not drawn as itself, for the status line: the
/// protein-coding filter hides its gene, or its gene is drawn as another (representative)
/// transcript. Search still finds and navigates to it.
pub(crate) fn search_hit_presentation_note(
    track: &rgv_core::TrackConfig,
    record: &rgv_hts::FeatureRecord,
) -> Option<String> {
    if track.kind != TrackKind::GeneModel {
        return None;
    }
    if track.feature.protein_coding_only && !record.is_protein_coding() {
        return Some("hidden by the track's Protein-coding only filter".to_owned());
    }
    let choice = record.transcript_choice()?;
    (folds_transcripts(track.kind, &track.feature)
        && !choice.is_representative()
        && !track.feature.expanded_genes.contains(choice.gene_key))
    .then(|| {
        format!(
            "drawn as its gene's representative {}; click it to show all transcripts",
            choice.representative
        )
    })
}

/// The label of the menu entry that toggles a gene.
pub(crate) fn gene_transcripts_toggle_label(target: &GeneTranscriptsTarget) -> String {
    if target.expanded {
        format!("Show one transcript of {}", target.gene_label)
    } else {
        format!("Show all transcripts of {}", target.gene_label)
    }
}

/// Shows the feature right-click menu; a chosen entry leaves its target in `action`.
pub(crate) fn show_feature_context_menu(
    parent_ui: &egui::Ui,
    state: &mut Option<FeatureContextMenuState>,
    action: &mut Option<GeneTranscriptsTarget>,
) {
    let Some(menu) = state.clone() else {
        return;
    };
    let context = parent_ui.ctx();
    let mut open = true;
    let shown = egui::Popup::new(
        egui::Id::new("feature-canvas-context-menu"),
        context.clone(),
        menu.screen_position,
        parent_ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .open_bool(&mut open)
    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
    .width(238.0)
    .show(|ui| {
        ui.set_min_width(238.0);
        ui.label(egui::RichText::new(&menu.target.gene_label).strong());
        if ui
            .button(gene_transcripts_toggle_label(&menu.target))
            .clicked()
        {
            *action = Some(menu.target.clone());
            ui.close();
        }
    });
    if action.is_some() || !open || shown.is_none() {
        *state = None;
        context.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(rank: usize) -> rgv_hts::FeatureRecord {
        rgv_hts::FeatureRecord {
            contig: "chr1".to_owned(),
            start: 100,
            end: 200,
            name: Some("CLASP2".to_owned()),
            feature_type: Some("transcript".to_owned()),
            score: None,
            strand: rgv_hts::Strand::Forward,
            blocks: Vec::new(),
            coding_interval: None,
            peak: None,
            attributes: vec![
                ("gene_name".to_owned(), "CLASP2".to_owned()),
                (
                    rgv_model::TRANSCRIPT_CHOICE_ATTRIBUTE.to_owned(),
                    format!("{rank}\t12\tmane\tENST1\tENSG1"),
                ),
            ],
        }
    }

    #[test]
    fn a_folded_gene_is_a_target_and_toggles_by_its_key() {
        let track = TrackId::new(3);
        let settings = FeatureTrackSettings::default();
        let target =
            gene_transcripts_target(track, TrackKind::GeneModel, &settings, &record(0)).unwrap();
        assert_eq!(target.gene_key, "ENSG1");
        assert_eq!(target.gene_label, "CLASP2");
        assert!(target.representative && !target.expanded);
        assert_eq!(
            gene_transcripts_toggle_label(&target),
            "Show all transcripts of CLASP2"
        );

        let expanded = with_gene_expanded(&settings, "ENSG1", true);
        let target =
            gene_transcripts_target(track, TrackKind::GeneModel, &expanded, &record(3)).unwrap();
        assert!(target.expanded && !target.representative);
        assert_eq!(
            gene_transcripts_toggle_label(&target),
            "Show one transcript of CLASP2"
        );
        assert_eq!(with_gene_expanded(&expanded, "ENSG1", false), settings);

        // Nothing to toggle on another kind of track, with every transcript shown, collapsed,
        // or for a gene with one transcript.
        assert!(
            gene_transcripts_target(track, TrackKind::Feature, &settings, &record(0)).is_none()
        );
        let all = FeatureTrackSettings {
            transcripts: TranscriptDisplay::All,
            ..FeatureTrackSettings::default()
        };
        assert!(gene_transcripts_target(track, TrackKind::GeneModel, &all, &record(0)).is_none());
        let collapsed = FeatureTrackSettings {
            display_mode: FeatureDisplayMode::Collapsed,
            ..FeatureTrackSettings::default()
        };
        assert!(
            gene_transcripts_target(track, TrackKind::GeneModel, &collapsed, &record(0)).is_none()
        );
        let mut single = record(0);
        single.attributes.pop();
        assert!(gene_transcripts_target(track, TrackKind::GeneModel, &settings, &single).is_none());
    }

    #[test]
    fn search_hits_hidden_by_the_track_say_why() {
        let mut track = rgv_core::TrackConfig::new(
            TrackId::new(1),
            "Genes".to_owned(),
            TrackKind::GeneModel,
            rgv_core::TrackSource::Local {
                path: "genes.gtf".to_owned(),
            },
        );
        assert_eq!(search_hit_presentation_note(&track, &record(0)), None);
        assert_eq!(
            search_hit_presentation_note(&track, &record(2)).as_deref(),
            Some("drawn as its gene's representative ENST1; click it to show all transcripts")
        );
        track.feature.expanded_genes.insert("ENSG1".to_owned());
        assert_eq!(search_hit_presentation_note(&track, &record(2)), None);
        track.feature.protein_coding_only = true;
        let mut non_coding = record(0);
        non_coding
            .attributes
            .push(("gene_type".to_owned(), "lncRNA".to_owned()));
        assert_eq!(
            search_hit_presentation_note(&track, &non_coding).as_deref(),
            Some("hidden by the track's Protein-coding only filter")
        );
    }

    #[test]
    fn the_properties_panel_names_the_representative_and_lists_the_genes_transcripts() {
        use crate::inspection::auxiliary::{feature_inspector_document, feature_inspector_fields};
        use crate::inspection::document::InspectorActionId;
        use crate::inspection::model::{FeatureInspectorSnapshot, GeneTranscriptsSummary};

        let transcript = |id: &str, start: u64, tags: &[&str]| {
            let mut record = record(0);
            record.attributes.truncate(1);
            record
                .attributes
                .push(("gene_id".to_owned(), "ENSG1".to_owned()));
            record
                .attributes
                .push(("transcript_id".to_owned(), id.to_owned()));
            record
                .attributes
                .extend(tags.iter().map(|tag| ("tag".to_owned(), (*tag).to_owned())));
            record.start = start;
            record
        };
        let mut records = vec![
            transcript("ENST3", 100, &[]),
            transcript("ENST2", 120, &["MANE_Select"]),
            transcript("ENST1", 140, &["basic"]),
        ];
        rgv_model::annotate_transcript_choices(&mut records);
        let settings = FeatureTrackSettings::default();
        let selected = records[0].clone();
        let inspector = FeatureInspectorSnapshot {
            track_name: "GENCODE".to_owned(),
            gene_transcripts: GeneTranscriptsSummary {
                transcripts: gene_transcript_identifiers(&records, &selected),
                toggle: gene_transcripts_target(
                    TrackId::new(1),
                    TrackKind::GeneModel,
                    &settings,
                    &selected,
                ),
            },
            record: selected,
        };
        let fields = feature_inspector_fields(&inspector);
        let field = |label: &str| {
            fields
                .iter()
                .find(|(name, _)| name == label)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(field("Representative"), Some("ENST2 (reason: MANE Select)"));
        assert_eq!(field("Gene transcripts (3)"), Some("ENST2, ENST1, ENST3"));
        // The viewer's own bookkeeping is not shown as a source attribute.
        assert!(
            fields
                .iter()
                .all(|(name, _)| !name.contains(rgv_model::TRANSCRIPT_CHOICE_ATTRIBUTE))
        );
        let document = feature_inspector_document(&inspector);
        assert!(
            document
                .raw_fields
                .iter()
                .all(|field| field.label != rgv_model::TRANSCRIPT_CHOICE_ATTRIBUTE)
        );
        let toggle = document
            .actions
            .iter()
            .find(|action| action.id == InspectorActionId::ToggleGeneTranscripts)
            .unwrap();
        assert_eq!(toggle.label, "Show all transcripts of CLASP2");

        // The representative says so.
        let representative = records
            .iter()
            .find(|record| record.transcript_identifier() == Some("ENST2"))
            .unwrap();
        assert_eq!(
            crate::inspection::auxiliary::representative_transcript_text(representative).as_deref(),
            Some("ENST2 (reason: MANE Select) · this transcript")
        );
    }

    #[test]
    fn only_gene_model_tracks_are_presented_one_transcript_per_gene() {
        let mut records = vec![record(0), record(1)];
        records[1]
            .attributes
            .push(("transcript_id".to_owned(), "ENST2".to_owned()));
        rgv_model::annotate_transcript_choices(&mut records);
        let mut config = rgv_core::TrackConfig::new(
            TrackId::new(1),
            "Genes".to_owned(),
            TrackKind::GeneModel,
            rgv_core::TrackSource::Local {
                path: "genes.gtf".to_owned(),
            },
        );
        let presentation = crate::display::auxiliary::feature_presentation(&config, &records);
        assert_eq!(
            presentation.map(|presentation| presentation.shown()),
            Some(1)
        );
        config.kind = TrackKind::Feature;
        assert!(crate::display::auxiliary::feature_presentation(&config, &records).is_none());
    }

    #[test]
    fn expanded_genes_are_bounded() {
        let settings = FeatureTrackSettings {
            expanded_genes: (0..MAX_EXPANDED_GENES)
                .map(|index| format!("G{index:05}"))
                .collect(),
            ..FeatureTrackSettings::default()
        };
        let settings = with_gene_expanded(&settings, &format!("G{MAX_EXPANDED_GENES:05}"), true);
        assert_eq!(settings.expanded_genes.len(), MAX_EXPANDED_GENES);
        assert!(
            settings
                .expanded_genes
                .contains(&format!("G{MAX_EXPANDED_GENES:05}"))
        );
    }
}
