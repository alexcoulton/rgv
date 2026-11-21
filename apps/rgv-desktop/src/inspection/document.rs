use crate::inspection::model::SelectedSupplementaryChain;
use crate::inspection::read::inspector_fields_text;
use rgv_core::TrackId;
use std::fmt::Write as _;
use std::sync::Arc;

/// Format-neutral presentation data for the Properties panel.
///
/// Source records remain owned by their parsers and snapshots. Adapters build
/// this bounded document so the UI never needs to know whether a field came
/// from BAM, VCF, GTF, BED, or a custom source.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct InspectorDocument {
    /// Stable across redraws for the same semantic selection.
    pub(crate) identity: String,
    pub(crate) header: InspectorHeader,
    pub(crate) sections: Vec<InspectorSection>,
    pub(crate) actions: Vec<InspectorAction>,
    pub(crate) raw_fields: Vec<InspectorField>,
    pub(crate) notices: Vec<InspectorNotice>,
    /// Transitional schema-v4 projection. Semantic adapters populate this
    /// from their former flat output so UI restructuring cannot reorder or
    /// rename automation evidence accidentally.
    compatibility_fields: Vec<InspectorAutomationField>,
    copy_details_override: Option<InspectorCopyDetails>,
}

impl InspectorDocument {
    pub(crate) fn new(identity: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            identity: identity.into(),
            header: InspectorHeader {
                title: title.into(),
                ..InspectorHeader::default()
            },
            ..Self::default()
        }
    }

    /// Compatibility constructor used while source-specific adapters migrate.
    /// Every field remains ordered and receives an explicit schema-v4 slot.
    pub(crate) fn from_legacy_fields(
        identity: impl Into<String>,
        title: impl Into<String>,
        fields: Vec<(String, String)>,
    ) -> Self {
        let mut document = Self::new(identity, title);
        document.sections.push(InspectorSection {
            id: "details".to_owned(),
            title: "Details".to_owned(),
            priority: 0,
            initially_expanded: true,
            fields: fields
                .into_iter()
                .enumerate()
                .map(|(order, (label, value))| InspectorField::legacy(order, label, value))
                .collect(),
        });
        document
    }

    /// Reproduces the existing automation schema-v4 flat field representation.
    /// Presentation order and automation order are deliberately independent so
    /// semantic sections can evolve without breaking existing clients.
    pub(crate) fn flatten_automation_fields(&self) -> Vec<(String, String)> {
        if !self.compatibility_fields.is_empty() {
            let mut fields = self.compatibility_fields.clone();
            fields.sort_by_key(|field| field.order);
            return fields
                .into_iter()
                .map(|field| (field.label, field.value))
                .collect();
        }
        let mut fields = self
            .sections
            .iter()
            .flat_map(|section| &section.fields)
            .chain(&self.raw_fields)
            .filter_map(|field| {
                field.automation.as_ref().map(|automation| {
                    (
                        automation.order,
                        automation.label.clone(),
                        automation.value.clone(),
                    )
                })
            })
            .collect::<Vec<_>>();
        fields.sort_by_key(|(order, _, _)| *order);
        fields
            .into_iter()
            .map(|(_, label, value)| (label, value))
            .collect()
    }

    pub(crate) fn preserve_automation_fields(&mut self, fields: &[(String, String)]) {
        self.compatibility_fields = fields
            .iter()
            .enumerate()
            .map(|(order, (label, value))| InspectorAutomationField {
                order,
                label: label.clone(),
                value: value.clone(),
            })
            .collect();
    }

    pub(crate) fn preserve_copy_details(&mut self, details: String) {
        self.copy_details_override = Some(InspectorCopyDetails::Text(details));
    }

    /// Retains a complete clipboard representation without constructing it on
    /// every redraw. Source adapters may capture lightweight shared query data;
    /// the renderer remains format-neutral and evaluates the payload on click.
    pub(crate) fn preserve_deferred_copy_details(
        &mut self,
        identity: impl Into<String>,
        build: impl Fn() -> String + 'static,
    ) {
        self.copy_details_override = Some(InspectorCopyDetails::Deferred {
            identity: identity.into(),
            build: Arc::new(build),
        });
    }

    pub(crate) fn copy_details_text(&self) -> String {
        self.copy_details_override.as_ref().map_or_else(
            || inspector_fields_text(&self.flatten_automation_fields()),
            InspectorCopyDetails::build,
        )
    }

    pub(crate) fn all_fields(&self) -> impl Iterator<Item = &InspectorField> {
        self.sections
            .iter()
            .flat_map(|section| &section.fields)
            .chain(&self.raw_fields)
    }
}

#[derive(Clone)]
enum InspectorCopyDetails {
    Text(String),
    Deferred {
        identity: String,
        build: Arc<dyn Fn() -> String>,
    },
}

impl InspectorCopyDetails {
    fn build(&self) -> String {
        match self {
            Self::Text(details) => details.clone(),
            Self::Deferred { build, .. } => build(),
        }
    }
}

impl std::fmt::Debug for InspectorCopyDetails {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(details) => formatter
                .debug_tuple("Text")
                .field(&format_args!("{} bytes", details.len()))
                .finish(),
            Self::Deferred { identity, .. } => formatter
                .debug_struct("Deferred")
                .field("identity", identity)
                .finish_non_exhaustive(),
        }
    }
}

impl PartialEq for InspectorCopyDetails {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
            && self.comparison_key() == other.comparison_key()
    }
}

impl InspectorCopyDetails {
    fn comparison_key(&self) -> &str {
        match self {
            Self::Text(details) => details,
            Self::Deferred { identity, .. } => identity,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct InspectorHeader {
    pub(crate) title: String,
    pub(crate) subtitle: Option<String>,
    pub(crate) badges: Vec<InspectorBadge>,
    pub(crate) location: Option<InspectorLocation>,
    pub(crate) source: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InspectorBadge {
    pub(crate) label: String,
    pub(crate) tone: InspectorTone,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum InspectorTone {
    #[default]
    Neutral,
    Informative,
    Positive,
    Warning,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InspectorLocation {
    pub(crate) contig: String,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) strand: Option<rgv_hts::Strand>,
    pub(crate) display: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InspectorSection {
    /// Stable identity used by egui to retain expansion state across redraws.
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) priority: i16,
    pub(crate) initially_expanded: bool,
    pub(crate) fields: Vec<InspectorField>,
}

impl InspectorSection {
    pub(crate) fn new(id: impl Into<String>, title: impl Into<String>, priority: i16) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            priority,
            initially_expanded: true,
            fields: Vec::new(),
        }
    }

    pub(crate) fn collapsed(mut self) -> Self {
        self.initially_expanded = false;
        self
    }

    pub(crate) fn with_fields(mut self, fields: Vec<InspectorField>) -> Self {
        self.fields = fields;
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InspectorField {
    /// Stable within its section; duplicate source keys use distinct IDs.
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) value: InspectorValue,
    /// Complete underlying value used by copy affordances. When absent, the
    /// value's complete textual representation is used.
    pub(crate) copy_value: Option<String>,
    pub(crate) provenance: InspectorProvenance,
    pub(crate) automation: Option<InspectorAutomationField>,
}

impl InspectorField {
    pub(crate) fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        value: InspectorValue,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            copy_value: None,
            provenance: InspectorProvenance::Source,
            automation: None,
        }
    }

    pub(crate) fn legacy(order: usize, label: String, value: String) -> Self {
        let inspector_value = InspectorValue::from_legacy(&label, &value);
        let provenance = if matches!(inspector_value, InspectorValue::Missing { .. }) {
            InspectorProvenance::Missing("the source did not supply this optional value".to_owned())
        } else {
            InspectorProvenance::Source
        };
        Self {
            id: format!("field-{order}"),
            value: inspector_value,
            copy_value: Some(value.clone()),
            provenance,
            automation: Some(InspectorAutomationField {
                order,
                label: label.clone(),
                value,
            }),
            label,
        }
    }

    pub(crate) fn with_copy_value(mut self, value: impl Into<String>) -> Self {
        self.copy_value = Some(value.into());
        self
    }

    pub(crate) fn with_provenance(mut self, provenance: InspectorProvenance) -> Self {
        self.provenance = provenance;
        self
    }

    pub(crate) fn complete_copy_value(&self) -> String {
        self.copy_value
            .clone()
            .unwrap_or_else(|| self.value.complete_text())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InspectorAutomationField {
    pub(crate) order: usize,
    pub(crate) label: String,
    pub(crate) value: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum InspectorValue {
    Text(String),
    Identifier(String),
    GenomicLocation(InspectorLocation),
    Number {
        display: String,
        value: Option<f64>,
    },
    Measurement {
        display: String,
        value: Option<f64>,
        unit: String,
    },
    Status {
        label: String,
        tone: InspectorTone,
    },
    Tags(Vec<String>),
    List(Vec<String>),
    Sequence {
        display: String,
        total_bases: usize,
        truncated: bool,
    },
    SupplementaryChainDiagram(SelectedSupplementaryChain),
    Table(InspectorTable),
    Missing {
        display: String,
        consequential: bool,
    },
    RawKeyValue {
        key: String,
        value: String,
    },
}

impl InspectorValue {
    fn from_legacy(label: &str, value: &str) -> Self {
        let label_lower = label.to_ascii_lowercase();
        let value_lower = value.to_ascii_lowercase();
        if ["missing", "not supplied", "unavailable", "not loaded"]
            .iter()
            .any(|missing| {
                value_lower == *missing || value_lower.starts_with(&format!("{missing} "))
            })
        {
            return Self::Missing {
                display: value.to_owned(),
                consequential: false,
            };
        }
        if (label_lower.contains("location")
            || label_lower == "position"
            || label_lower == "locus"
            || label_lower == "source"
            || label_lower == "target"
            || label_lower.contains("interval"))
            && let Some(location) = parse_inspector_location(value)
        {
            return Self::GenomicLocation(location);
        }
        if label_lower == "id"
            || label_lower.contains("identifier")
            || label_lower == "read"
            || label_lower.contains("identity")
        {
            return Self::Identifier(value.to_owned());
        }
        if let Ok(number) = value.replace(',', "").parse::<f64>() {
            return Self::Number {
                display: value.to_owned(),
                value: Some(number),
            };
        }
        if let Some((number, unit)) = value.split_once(' ')
            && let Ok(number) = number.replace(',', "").parse::<f64>()
            && !unit.is_empty()
        {
            return Self::Measurement {
                display: value.to_owned(),
                value: Some(number),
                unit: unit.to_owned(),
            };
        }
        if label_lower.contains("status")
            || label_lower.contains("state")
            || label_lower.contains("classification")
            || label_lower.contains("directionality")
            || label_lower == "filter"
        {
            let tone = if value_lower == "pass" || value_lower.contains("complete") {
                InspectorTone::Positive
            } else if value_lower.contains("unavailable")
                || value_lower.contains("incomplete")
                || value_lower.contains("unknown")
            {
                InspectorTone::Warning
            } else {
                InspectorTone::Neutral
            };
            return Self::Status {
                label: value.to_owned(),
                tone,
            };
        }
        Self::Text(value.to_owned())
    }

    pub(crate) fn display_text(&self) -> String {
        match self {
            Self::Text(value)
            | Self::Identifier(value)
            | Self::Number { display: value, .. }
            | Self::Measurement { display: value, .. }
            | Self::Missing { display: value, .. } => value.clone(),
            Self::GenomicLocation(location) => location.display.clone(),
            Self::Status { label, .. } => label.clone(),
            Self::Tags(values) | Self::List(values) => values.join(", "),
            Self::Sequence { display, .. } => display.clone(),
            Self::SupplementaryChainDiagram(chain) => {
                format!("{} supplementary segments", chain.segments.len())
            }
            Self::Table(table) => format!("{} rows", table.total_rows),
            Self::RawKeyValue { key, value } => format!("{key}={value}"),
        }
    }

    pub(crate) fn complete_text(&self) -> String {
        match self {
            Self::Table(table) => table.copy_text(),
            _ => self.display_text(),
        }
    }

    pub(crate) fn uses_monospace(&self) -> bool {
        matches!(
            self,
            Self::Identifier(_)
                | Self::GenomicLocation(_)
                | Self::Sequence { .. }
                | Self::RawKeyValue { .. }
        )
    }
}

fn parse_inspector_location(value: &str) -> Option<InspectorLocation> {
    let coordinate = value
        .split_whitespace()
        .next()?
        .trim_end_matches([';', ',']);
    let (contig, positions) = coordinate.rsplit_once(':')?;
    if contig.is_empty() {
        return None;
    }
    let positions = positions.replace(',', "");
    let (start, end) = positions
        .split_once('-')
        .or_else(|| positions.split_once('–'))
        .map_or_else(
            || {
                let position = positions.parse::<u64>().ok()?;
                Some((position.saturating_sub(1), position))
            },
            |(start, end)| {
                Some((
                    start.parse::<u64>().ok()?.saturating_sub(1),
                    end.parse().ok()?,
                ))
            },
        )?;
    (start <= end).then(|| InspectorLocation {
        contig: contig.to_owned(),
        start,
        end,
        strand: None,
        display: value.to_owned(),
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct InspectorTable {
    pub(crate) columns: Vec<String>,
    /// Adapters keep this collection bounded and report the complete row count.
    pub(crate) rows: Vec<Vec<String>>,
    pub(crate) total_rows: usize,
    pub(crate) omitted_rows: usize,
    pub(crate) search_label: Option<String>,
}

impl InspectorTable {
    pub(crate) fn copy_text(&self) -> String {
        let mut output = self.columns.join("\t");
        for row in &self.rows {
            output.push('\n');
            output.push_str(&row.join("\t"));
        }
        if self.omitted_rows > 0 {
            let _ = write!(
                output,
                "\n… {} rows omitted from presentation",
                self.omitted_rows
            );
        }
        output
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum InspectorProvenance {
    #[default]
    Source,
    Parsed,
    Inferred(String),
    Aggregated(String),
    Truncated {
        displayed: usize,
        /// Complete cardinality or byte count when the source retained it.
        /// `None` is used instead of inventing a total after parser-level
        /// truncation has made the original size unknowable.
        total: Option<usize>,
        reason: String,
    },
    Missing(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InspectorNotice {
    pub(crate) id: String,
    pub(crate) tone: InspectorTone,
    pub(crate) message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InspectorAction {
    pub(crate) id: InspectorActionId,
    pub(crate) label: String,
    pub(crate) enabled: bool,
    pub(crate) disabled_reason: Option<String>,
    pub(crate) hover_text: Option<String>,
    pub(crate) emphasis: InspectorActionEmphasis,
}

impl InspectorAction {
    pub(crate) fn new(id: InspectorActionId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            enabled: true,
            disabled_reason: None,
            hover_text: None,
            emphasis: InspectorActionEmphasis::Normal,
        }
    }

    pub(crate) fn disabled(mut self, reason: impl Into<String>) -> Self {
        self.enabled = false;
        self.disabled_reason = Some(reason.into());
        self
    }

    pub(crate) fn with_hover_text(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        if !text.is_empty() {
            self.hover_text = Some(text);
        }
        self
    }

    pub(crate) fn primary(mut self) -> Self {
        self.emphasis = InspectorActionEmphasis::Primary;
        self
    }

    pub(crate) fn low_emphasis(mut self) -> Self {
        self.emphasis = InspectorActionEmphasis::Low;
        self
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum InspectorActionId {
    CopyDetails,
    CopyCigar,
    CopyReadSequence,
    CopyEventSequence,
    CopyAuxiliaryTags,
    CopyVisibleReference,
    CopyVisibleConsensus,
    OpenMate,
    MoveReadToTop,
    NavigateChain(usize),
    OpenChainSegment(usize),
    OpenAllChainSegments,
    ExportJunctionBed,
    ExportSashimiPng,
    ExportSashimiSvg,
    AddFeatureToReview,
    OpenInteractionEndpoint(InspectorEndpoint),
    OpenVariantPartner(usize),
    OpenVariantEndpoint,
    InspectVariantPileup(TrackId, u64),
    InspectVariantModifications(TrackId, u64),
    ExportVariantGenotypes,
    ExportVariantEvidence,
    ShowAllTranscriptsForGene(String),
    ClearSelection,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum InspectorEndpoint {
    Source,
    Target,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum InspectorActionEmphasis {
    Primary,
    #[default]
    Normal,
    Low,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn compatibility_flattening_is_exact_after_semantic_reordering() {
        let legacy = vec![
            ("Track".to_owned(), "GENCODE".to_owned()),
            ("Name".to_owned(), "STK40".to_owned()),
            ("Attribute · tag".to_owned(), "basic".to_owned()),
            ("Attribute · tag".to_owned(), "CCDS".to_owned()),
        ];
        let mut document =
            InspectorDocument::from_legacy_fields("feature:1", "STK40", legacy.clone());
        document.sections[0].fields.reverse();

        assert_eq!(document.flatten_automation_fields(), legacy);
    }

    #[test]
    fn repeated_raw_keys_remain_ordered_and_distinct() {
        let mut document = InspectorDocument::new("feature:1", "STK40");
        document.raw_fields = vec![
            InspectorField::new(
                "attribute-0",
                "tag",
                InspectorValue::RawKeyValue {
                    key: "tag".to_owned(),
                    value: "basic".to_owned(),
                },
            ),
            InspectorField::new(
                "attribute-1",
                "tag",
                InspectorValue::RawKeyValue {
                    key: "tag".to_owned(),
                    value: "CCDS".to_owned(),
                },
            ),
        ];

        assert_eq!(document.raw_fields.len(), 2);
        assert_eq!(document.raw_fields[0].label, document.raw_fields[1].label);
        assert_ne!(document.raw_fields[0].id, document.raw_fields[1].id);
    }

    #[test]
    fn abbreviated_display_keeps_the_complete_copy_value() {
        let field = InspectorField::new(
            "sequence",
            "Sequence",
            InspectorValue::Sequence {
                display: "ACGT…".to_owned(),
                total_bases: 12,
                truncated: true,
            },
        )
        .with_copy_value("ACGTACGTACGT");

        assert_eq!(field.value.display_text(), "ACGT…");
        assert_eq!(field.complete_copy_value(), "ACGTACGTACGT");
    }

    #[test]
    fn generic_legacy_fallback_assigns_semantic_types_and_honest_missingness() {
        let document = InspectorDocument::from_legacy_fields(
            "unknown:1",
            "Unknown record",
            vec![
                ("Location".to_owned(), "chrUn:1,001-1,020".to_owned()),
                ("Depth".to_owned(), "17".to_owned()),
                ("Span".to_owned(), "20 bp".to_owned()),
                ("Parse status".to_owned(), "incomplete".to_owned()),
                ("Optional score".to_owned(), "not supplied".to_owned()),
            ],
        );
        let fields = document.all_fields().collect::<Vec<_>>();

        assert!(matches!(
            fields[0].value,
            InspectorValue::GenomicLocation(InspectorLocation {
                start: 1_000,
                end: 1_020,
                ..
            })
        ));
        assert!(matches!(
            fields[1].value,
            InspectorValue::Number {
                value: Some(17.0),
                ..
            }
        ));
        assert!(matches!(
            fields[2].value,
            InspectorValue::Measurement {
                value: Some(20.0),
                ref unit,
                ..
            } if unit == "bp"
        ));
        assert!(matches!(fields[3].value, InspectorValue::Status { .. }));
        assert!(matches!(fields[4].value, InspectorValue::Missing { .. }));
        assert!(matches!(
            fields[4].provenance,
            InspectorProvenance::Missing(_)
        ));
    }

    #[test]
    fn deferred_copy_details_are_built_only_when_requested() {
        let builds = Arc::new(AtomicUsize::new(0));
        let observed_builds = Arc::clone(&builds);
        let mut document = InspectorDocument::new("large:1", "Large record");
        document.preserve_deferred_copy_details("large:1:copy", move || {
            observed_builds.fetch_add(1, Ordering::SeqCst);
            "complete details".to_owned()
        });

        let cloned = document.clone();
        assert_eq!(builds.load(Ordering::SeqCst), 0);
        assert_eq!(cloned.copy_details_text(), "complete details");
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }
}
