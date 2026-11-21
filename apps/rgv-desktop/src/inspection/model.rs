use crate::inspection::document::{
    InspectorDocument, InspectorField, InspectorNotice, InspectorSection, InspectorTone,
    InspectorValue,
};
use rgv_automation::AutomationSelectionSnapshot;
use rgv_core::{
    CopyNumberTrackSettings, GwasTrackSettings, InteractionTrackSettings,
    QuantitativeTrackSettings, TrackId, VariantTrackSettings,
};
use rgv_hts::SupplementarySegment;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct InspectorSnapshot {
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) auxiliary: Vec<(String, String)>,
    pub(crate) event_sequence: Option<String>,
    pub(crate) cigar: Option<String>,
    pub(crate) read_sequence: Option<BoundedSequenceCopy>,
}

impl InspectorSnapshot {
    pub(crate) fn document(
        &self,
        identity: impl Into<String>,
        title: impl Into<String>,
    ) -> InspectorDocument {
        let mut document =
            InspectorDocument::from_legacy_fields(identity, title, self.fields.clone());
        document.header.source = self
            .fields
            .iter()
            .find(|(label, _)| label == "Track")
            .map(|(_, value)| value.clone());
        let location = document.all_fields().find_map(|field| match &field.value {
            InspectorValue::GenomicLocation(location) => Some(location.clone()),
            _ => None,
        });
        document.header.location = location;
        if let Some(details) = document.sections.first_mut()
            && let Some(event_start) = details
                .fields
                .iter()
                .position(|field| field.label == "Selected event")
        {
            let event_fields = details.fields.split_off(event_start);
            document.sections.push(
                InspectorSection::new("selected-event", "Selected event", 20)
                    .with_fields(event_fields),
            );
        }
        document.raw_fields = self
            .auxiliary
            .iter()
            .enumerate()
            .map(|(index, (key, value))| {
                InspectorField::new(
                    format!("auxiliary-{index}"),
                    key,
                    InspectorValue::RawKeyValue {
                        key: key.clone(),
                        value: value.clone(),
                    },
                )
                .with_copy_value(format!("{key}:{value}"))
            })
            .collect();
        if !document.raw_fields.is_empty() {
            document.notices.push(InspectorNotice {
                id: "auxiliary-tag-provenance".to_owned(),
                tone: InspectorTone::Informative,
                message: format!(
                    "{} loaded auxiliary SAM tags are available below in source order.",
                    document.raw_fields.len()
                ),
            });
        }
        document.preserve_automation_fields(&self.fields);
        document
    }
}

#[derive(Clone)]
pub(crate) struct RetainedEvidenceSnapshot {
    pub(crate) selection: AutomationSelectionSnapshot,
    pub(crate) inspector: Option<InspectorSnapshot>,
    pub(crate) export: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoundedSequenceCopy {
    pub(crate) text: String,
    pub(crate) copied_bases: usize,
    pub(crate) total_bases: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SelectedSupplementaryChain {
    pub(crate) read_name: Vec<u8>,
    pub(crate) segments: Vec<SupplementarySegment>,
    pub(crate) active_segment: usize,
    pub(crate) malformed_entries: usize,
    pub(crate) truncated: bool,
    pub(crate) invalid_tag_type: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChainDiagramSegment {
    pub(crate) segment_index: usize,
    pub(crate) read_start: u64,
    pub(crate) read_end: u64,
    pub(crate) read_length: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SupplementaryChainDiagram {
    pub(crate) read_length: u64,
    pub(crate) segments: Vec<ChainDiagramSegment>,
    pub(crate) read_order: Vec<usize>,
    pub(crate) alignment_order: Vec<usize>,
}

pub(crate) use rgv_session::selection::{
    CopyNumberSelection, FeatureSelection, GwasSelection, InteractionSelection, MutationSelection,
    QuantitativeSelection, VariantSelection,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EvidenceSelection {
    Feature(usize),
    Variant(usize),
    Gwas(usize),
    Quantitative(usize, u64),
    CopyNumber(usize),
    Mutation(usize),
    Interaction(usize),
}

pub(crate) fn next_evidence_index(
    current: Option<usize>,
    count: usize,
    forward: bool,
) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(if forward {
        current.map_or(0, |index| index.saturating_add(1) % count)
    } else {
        current.map_or(count - 1, |index| index.checked_sub(1).unwrap_or(count - 1))
    })
}

#[derive(Clone)]
pub(crate) struct FeatureInspectorSnapshot {
    pub(crate) track_name: String,
    pub(crate) record: rgv_hts::FeatureRecord,
}

#[derive(Clone)]
pub(crate) struct InteractionInspectorSnapshot {
    pub(crate) track_name: String,
    pub(crate) query: Arc<rgv_hts::InteractionQuery>,
    pub(crate) record_index: usize,
    pub(crate) settings: InteractionTrackSettings,
}

#[derive(Clone)]
pub(crate) struct QuantitativeInspectorSnapshot {
    pub(crate) track_name: String,
    pub(crate) query: Arc<rgv_hts::QuantitativeQuery>,
    pub(crate) record_index: usize,
    pub(crate) selected_position: u64,
    pub(crate) operand: Option<QuantitativeOperandSnapshot>,
    pub(crate) settings: QuantitativeTrackSettings,
}

#[derive(Clone)]
pub(crate) struct GwasInspectorSnapshot {
    pub(crate) track_name: String,
    pub(crate) query: Arc<rgv_hts::GwasQuery>,
    pub(crate) record_index: usize,
    pub(crate) settings: GwasTrackSettings,
}

#[derive(Clone)]
pub(crate) struct CopyNumberInspectorSnapshot {
    pub(crate) track_name: String,
    pub(crate) query: Arc<rgv_hts::CopyNumberQuery>,
    pub(crate) segment_index: usize,
    pub(crate) settings: CopyNumberTrackSettings,
}

#[derive(Clone)]
pub(crate) struct MutationInspectorSnapshot {
    pub(crate) track_name: String,
    pub(crate) contig: String,
    pub(crate) query: Arc<rgv_hts::MutationQuery>,
    pub(crate) record_index: usize,
}

#[derive(Clone)]
pub(crate) struct QuantitativeOperandSnapshot {
    pub(crate) name: String,
    pub(crate) value: Option<f32>,
}

#[derive(Clone)]
pub(crate) struct VariantInspectorSnapshot {
    pub(crate) track_name: String,
    pub(crate) query: Arc<rgv_hts::VariantQuery>,
    pub(crate) record_index: usize,
    pub(crate) settings: VariantTrackSettings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VariantAlignmentEvidence {
    pub(crate) track_id: TrackId,
    pub(crate) track_name: String,
    pub(crate) position: u64,
    pub(crate) bin_start: u64,
    pub(crate) bin_end: u64,
    pub(crate) depth: u32,
    pub(crate) forward_depth: u32,
    pub(crate) reverse_depth: u32,
    pub(crate) allele_summary: String,
    pub(crate) modification_series: usize,
    pub(crate) modification_calls: u32,
    pub(crate) pair_templates_sampled: usize,
    pub(crate) retained_records: usize,
    pub(crate) complete_individual_reads: bool,
    pub(crate) complete_pileup: bool,
}
