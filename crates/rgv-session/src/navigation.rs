use anyhow::{Context as _, Result};
use rgv_automation::AutomationViewportOperation;
use rgv_core::{
    ContigAliases, ContigId, GenomicInterval, IgvLocusResolution, Locus, LocusError, LocusRange,
    MAXIMUM_VIEW_PANELS, NavigationEntry, ReferenceDictionary, Viewport,
};
use rgv_engine::{TrackSearchHit, TrackSearchResults};
use rgv_hts::AlignmentRecord;

/// Bases shown on each side of a single-position locus such as `chr1:12345`.
pub const SINGLE_POSITION_FLANK: u64 = 20;

/// Resolves a typed locus with the shared [`Locus`] grammar, or the opening
/// view (the first 100 kb of the first contig) when none was requested.
pub fn resolve_initial_locus(
    dictionary: &ReferenceDictionary,
    aliases: &ContigAliases,
    requested: Option<&str>,
) -> Result<(GenomicInterval, String, u64)> {
    let Some(requested) = requested else {
        let first = dictionary
            .iter()
            .next()
            .context("alignment header has no reference sequences")?;
        anyhow::ensure!(
            first.length() > 0,
            "contig {} has zero length",
            first.name()
        );
        let interval = GenomicInterval::new(first.id(), 0, first.length().min(100_000))?;
        return Ok((interval, first.name().to_owned(), first.length()));
    };
    let resolved = Locus::resolve(requested, dictionary, aliases)?;
    let interval = resolved
        .interval(SINGLE_POSITION_FLANK)?
        .widened_to_minimum_view(resolved.contig_length);
    Ok((interval, resolved.contig_name, resolved.contig_length))
}

/// What to open for an imported IGV session whose `Session.locus` was not a
/// coordinate interval (CORE-1). The imported document then only holds a
/// one-base placeholder, which must never be navigated to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportedLocusRequest {
    /// IGV's whole-genome view (`All`), or a missing or unparseable locus.
    Default,
    /// A bare contig, or a feature such as a gene symbol.
    Named(String),
}

pub fn imported_locus_request(resolution: &IgvLocusResolution) -> Option<ImportedLocusRequest> {
    match resolution {
        IgvLocusResolution::Interval => None,
        IgvLocusResolution::Named { name } => Some(ImportedLocusRequest::Named(name.clone())),
        IgvLocusResolution::Unresolved { .. } => Some(ImportedLocusRequest::Default),
    }
}

impl ImportedLocusRequest {
    /// The locus each panel asks for once a dictionary is known.
    pub fn requested_locus(&self) -> Option<String> {
        match self {
            Self::Default => None,
            Self::Named(name) => Some(name.clone()),
        }
    }

    pub fn feature_name(&self) -> Option<&str> {
        match self {
            Self::Default => None,
            Self::Named(name) => Some(name),
        }
    }
}

/// Shown until a dictionary places the imported session's real locus.
pub const IMPORTED_PLACEHOLDER_LOCUS: &str = "chr1:1-100000";

/// A panel's requested locus resolved against a newly opened dictionary.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestedLocus {
    pub interval: GenomicInterval,
    pub contig_name: String,
    pub contig_length: u64,
    /// Set when the request was an imported feature name rather than a
    /// contig: the default locus was opened and this name should be found by
    /// annotation search.
    pub feature_search: Option<String>,
}

/// [`resolve_initial_locus`], except that an imported IGV name which is not a
/// contig opens the default locus and asks for a feature search instead of
/// failing.
pub fn resolve_requested_locus(
    dictionary: &ReferenceDictionary,
    aliases: &ContigAliases,
    requested: Option<&str>,
    imported_feature_name: Option<&str>,
) -> Result<RequestedLocus> {
    let direct = |requested| {
        resolve_initial_locus(dictionary, aliases, requested).map(
            |(interval, contig_name, contig_length)| RequestedLocus {
                interval,
                contig_name,
                contig_length,
                feature_search: None,
            },
        )
    };
    let is_imported_name = requested.is_some() && requested == imported_feature_name;
    if is_imported_name
        && let Some(name) = requested
        && matches!(
            Locus::resolve(name, dictionary, aliases),
            Err(LocusError::UnknownContig(_))
        )
    {
        return direct(None).map(|locus| RequestedLocus {
            feature_search: Some(name.to_owned()),
            ..locus
        });
    }
    direct(requested)
}

/// Whether one item of a locus expression should navigate instead of being
/// sent to ID/annotation search.
///
/// It navigates when it resolves, and also when it is recognisably a locus
/// that is merely wrong (`chr1:0`, `chr99:1-100`), so the user is told why.
/// A name that matches no contig and carries no coordinate range, such as a
/// gene symbol, the variant ID `chr1:12345:A:G` or an HGVS expression, does
/// not.
pub fn locus_item_is_navigable(
    item: &str,
    dictionary: Option<&ReferenceDictionary>,
    aliases: &ContigAliases,
) -> bool {
    if let Some(dictionary) = dictionary {
        match Locus::resolve(item, dictionary, aliases) {
            Err(LocusError::UnknownContig(_)) => {}
            Err(LocusError::Empty) => return false,
            Ok(_) | Err(_) => return true,
        }
    }
    match Locus::parse(item) {
        Ok(locus) => locus.range != LocusRange::WholeContig,
        Err(LocusError::Empty) => false,
        Err(_) => true,
    }
}

/// Whether every locus of `expression` is navigable (see
/// [`locus_item_is_navigable`]).
pub fn locus_expression_is_navigable(
    expression: &str,
    dictionary: Option<&ReferenceDictionary>,
    aliases: &ContigAliases,
) -> bool {
    let loci = rgv_core::split_loci(expression);
    !loci.is_empty()
        && loci
            .iter()
            .all(|locus| locus_item_is_navigable(locus, dictionary, aliases))
}

pub fn parse_contig_aliases(values: &[String]) -> Result<ContigAliases> {
    let mut aliases = ContigAliases::default();
    for value in values {
        let (alias, target) = value
            .split_once('=')
            .with_context(|| format!("contig alias must use ALIAS=CANONICAL, got {value}"))?;
        anyhow::ensure!(
            !alias.is_empty()
                && !target.is_empty()
                && !alias.contains([' ', '\t', '\n'])
                && !target.contains([' ', '\t', '\n']),
            "contig alias must contain two non-empty contig names, got {value}"
        );
        if let Some(previous) = aliases.insert(alias.to_owned(), target.to_owned()) {
            anyhow::ensure!(
                previous == target,
                "contig alias {alias} has conflicting targets {previous} and {target}"
            );
        }
    }
    Ok(aliases)
}

/// A locus for a workspace with no reference dictionary: the contig name is
/// taken as typed and its length is unknown, so a coordinate range is needed.
pub fn standalone_locus(locus: &str) -> Result<(GenomicInterval, String, u64)> {
    let parsed = Locus::parse(locus)?;
    let (start, end) = match parsed.range {
        LocusRange::WholeContig => anyhow::bail!(
            "standalone track locus must use name:start-end or name:position, got {locus}"
        ),
        LocusRange::Position(position) => (
            position.saturating_sub(SINGLE_POSITION_FLANK),
            position
                .saturating_add(1)
                .saturating_add(SINGLE_POSITION_FLANK),
        ),
        LocusRange::Interval { start, end } => (start, end),
    };
    let contig_length = end.saturating_mul(2).max(end.saturating_add(1_000_000));
    Ok((
        GenomicInterval::new(ContigId::new(0), start, end)?.widened_to_minimum_view(contig_length),
        parsed.contig,
        contig_length,
    ))
}

/// Splits a locus-box expression into its loci (see [`rgv_core::split_loci`]).
pub fn split_locus_expression(expression: &str) -> Result<Vec<&str>> {
    let loci = rgv_core::split_loci(expression);
    anyhow::ensure!(!loci.is_empty(), "enter at least one genomic locus");
    anyhow::ensure!(
        loci.len() <= MAXIMUM_VIEW_PANELS,
        "a workspace supports at most {MAXIMUM_VIEW_PANELS} view panels"
    );
    Ok(loci)
}

/// The hit a search for `query` navigates to: the only exact match; when several exact matches
/// are transcripts of one gene (one contig, each overlapping the widest), the widest; with no
/// exact match, the only hit. `None` asks the user to choose.
pub fn preferred_annotation_search_hit<'a>(
    results: &'a TrackSearchResults,
    query: &str,
) -> Option<&'a TrackSearchHit> {
    let normalized = query.trim().to_lowercase();
    let exact = results
        .hits
        .iter()
        .filter(|hit| hit.matched_term == normalized)
        .collect::<Vec<_>>();
    match exact.as_slice() {
        [] if results.hits.len() == 1 => results.hits.first(),
        [] => None,
        [only] => Some(only),
        several => one_gene_span(several),
    }
}

/// The widest of `hits` when every hit lies on its contig and overlaps it: several transcripts
/// (or several tracks' copies) of one gene, which a gene search shows as one locus.
fn one_gene_span<'a>(hits: &[&'a TrackSearchHit]) -> Option<&'a TrackSearchHit> {
    let widest = *hits.iter().max_by_key(|hit| {
        (
            hit.record.end.saturating_sub(hit.record.start),
            std::cmp::Reverse(hit.record.start),
        )
    })?;
    hits.iter()
        .all(|hit| {
            hit.record.contig == widest.record.contig
                && hit.record.start < widest.record.end
                && widest.record.start < hit.record.end
        })
        .then_some(widest)
}

pub fn annotation_search_hit_locus(hit: &TrackSearchHit) -> String {
    format!(
        "{}:{}-{}",
        hit.record.contig,
        hit.record.start.saturating_add(1),
        hit.record.end
    )
}

pub fn annotation_search_hit_primary_label(hit: &TrackSearchHit) -> &str {
    hit.record
        .display_label()
        .unwrap_or(hit.matched_term.as_str())
}

pub fn annotation_search_hit_secondary_identity(hit: &TrackSearchHit) -> String {
    let identity = hit.record.presentation_identity();
    let primary = annotation_search_hit_primary_label(hit);
    let mut identifiers = [identity.gene_id, identity.transcript_id]
        .into_iter()
        .flatten()
        .filter(|value| !value.eq_ignore_ascii_case(primary))
        .collect::<Vec<_>>();
    identifiers.dedup();
    identifiers.join(" · ")
}

pub fn mate_locus(
    record: &AlignmentRecord,
    dictionary: &ReferenceDictionary,
    visible_span: u64,
) -> Option<String> {
    if !record.flags.is_paired() || record.flags.is_mate_unmapped() {
        return None;
    }
    let mate_contig = record.mate_contig?;
    let mate_start = record.mate_start?;
    let contig = dictionary.get(mate_contig)?;
    if contig.length() == 0 || mate_start >= contig.length() {
        return None;
    }
    let span = visible_span.max(1).min(contig.length());
    let mut start = mate_start.saturating_sub(span / 2);
    let end = start.saturating_add(span).min(contig.length());
    start = end.saturating_sub(span);
    Some(format!("{}:{}-{}", contig.name(), start + 1, end))
}

pub fn mate_viewport(
    record: &AlignmentRecord,
    dictionary: &ReferenceDictionary,
    source: Viewport,
) -> Option<(Viewport, String, u64)> {
    if !record.flags.is_paired() || record.flags.is_mate_unmapped() {
        return None;
    }
    let mate_contig = record.mate_contig?;
    let mate_start = record.mate_start?;
    let contig = dictionary.get(mate_contig)?;
    if contig.length() == 0 || mate_start >= contig.length() {
        return None;
    }
    let viewport = source
        .remapped_contig(mate_contig, contig.length())
        .ok()?
        .centered_on(mate_start, contig.length())
        .ok()?;
    Some((viewport, contig.name().to_owned(), contig.length()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LociNavigationPlan {
    /// Move only the active panel; every other panel keeps its locus.
    RetargetActivePanel,
    /// Lay the workspace out as exactly the listed loci, one panel each.
    ReplaceWorkspacePanels,
}

/// Entering a single locus navigates the active panel. Only an explicit multi-locus expression
/// redefines the set of panels.
pub const fn loci_navigation_plan(locus_count: usize) -> LociNavigationPlan {
    if locus_count <= 1 {
        LociNavigationPlan::RetargetActivePanel
    } else {
        LociNavigationPlan::ReplaceWorkspacePanels
    }
}

/// The active panel's contig, used to revisit history without a dictionary.
#[derive(Clone, Copy, Debug)]
pub struct HistoryPanelContig<'a> {
    pub id: ContigId,
    pub name: &'a str,
    pub length: u64,
}

/// Re-expresses a history entry for the panel: interval, contig name, length.
///
/// With a dictionary the contig is found by *name*, so the entry lands on the
/// contig the user visited however that dictionary numbers it. Without one
/// every contig shares the placeholder id and only the name tells them apart;
/// a contig other than the current one gets the same provisional length a
/// typed standalone locus does.
pub fn resolve_history_entry(
    entry: &NavigationEntry,
    dictionary: Option<&ReferenceDictionary>,
    aliases: &ContigAliases,
    current: HistoryPanelContig<'_>,
) -> Result<(GenomicInterval, String, u64)> {
    if let Some(dictionary) = dictionary {
        let interval = entry.resolve(dictionary, aliases).with_context(|| {
            format!(
                "{}:{}-{} is not on the open reference",
                entry.contig_name,
                entry.start.saturating_add(1),
                entry.end
            )
        })?;
        let contig = dictionary
            .get(interval.contig())
            .context("reference dictionary is inconsistent")?;
        return Ok((
            interval.widened_to_minimum_view(contig.length()),
            contig.name().to_owned(),
            contig.length(),
        ));
    }
    let interval = entry
        .interval(current.id)
        .context("navigation history entry is empty")?;
    let length = if entry.contig_name == current.name {
        current.length.max(entry.end)
    } else {
        entry
            .end
            .saturating_mul(2)
            .max(entry.end.saturating_add(1_000_000))
    };
    Ok((
        interval.widened_to_minimum_view(length),
        entry.contig_name.clone(),
        length,
    ))
}

/// The viewport an automation viewport operation (pan, zoom, fit) leaves a panel on.
pub fn automation_viewport_after_operation(
    current: Viewport,
    contig_length: u64,
    operation: AutomationViewportOperation,
) -> Result<Viewport> {
    Ok(match operation {
        AutomationViewportOperation::PanBases { delta } => {
            current.pan_pixels(delta as f64 / current.bases_per_pixel(), contig_length)?
        }
        AutomationViewportOperation::PanFraction { fraction } => {
            current.pan_pixels(current.pixel_width() * fraction, contig_length)?
        }
        AutomationViewportOperation::Zoom {
            factor,
            anchor_position,
        } => {
            let anchor_x = anchor_position.map_or(current.pixel_width() / 2.0, |position| {
                current.pixel_at_genomic(position)
            });
            current.zoom_at(anchor_x, factor.recip(), contig_length)?
        }
        AutomationViewportOperation::FitInterval { start, end } => {
            let bounded_length = contig_length.max(1);
            let start = start.min(bounded_length.saturating_sub(1));
            let end = end.min(bounded_length).max(start.saturating_add(1));
            let interval =
                GenomicInterval::bounded(current.interval().contig(), start, end, contig_length)?;
            Viewport::with_minimum_span(interval, current.pixel_width(), contig_length)?
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::PanelState as ViewPanelController;
    use rgv_core::PanelId;

    /// A panel opened at `locus`, as `navigate_to_locus` leaves it.
    fn panel_at(dictionary: Option<&ReferenceDictionary>, locus: &str) -> ViewPanelController {
        let (interval, name, length) = dictionary.map_or_else(
            || standalone_locus(locus).unwrap(),
            |dictionary| {
                resolve_initial_locus(dictionary, &ContigAliases::default(), Some(locus)).unwrap()
            },
        );
        ViewPanelController::new(
            PanelId::new(1),
            Viewport::new(interval, 1_000.0).unwrap(),
            None,
            name,
            length,
        )
    }

    /// The pure part of `navigate_to_locus`.
    fn visit(
        panel: &mut ViewPanelController,
        dictionary: Option<&ReferenceDictionary>,
        locus: &str,
    ) {
        let next = panel_at(dictionary, locus);
        panel.viewport = next.viewport;
        panel.contig_name = next.contig_name;
        panel.contig_length = next.contig_length;
        panel.visit_history(panel.viewport.interval());
    }

    fn revisit(
        panel: &ViewPanelController,
        entry: &NavigationEntry,
        dictionary: Option<&ReferenceDictionary>,
    ) -> (GenomicInterval, String, u64) {
        resolve_history_entry(
            entry,
            dictionary,
            &ContigAliases::default(),
            HistoryPanelContig {
                id: panel.viewport.interval().contig(),
                name: &panel.contig_name,
                length: panel.contig_length,
            },
        )
        .unwrap()
    }

    #[test]
    fn without_a_dictionary_two_contigs_are_two_entries_and_back_returns_to_the_first() {
        // APP-2: every standalone contig is id zero, so interval-keyed history
        // collapsed chr1:1-100000 and chr2:1-100000 into one entry.
        let mut panel = panel_at(None, "chr1:1-100000");
        visit(&mut panel, None, "chr2:1-100000");
        assert_eq!(panel.navigation_history.len(), 2);
        assert!(panel.navigation_history.can_go_back());
        assert_eq!(panel.contig_name, "chr2");

        let entry = panel.navigation_history.back().cloned().unwrap();
        let (interval, name, _) = revisit(&panel, &entry, None);
        assert_eq!(name, "chr1");
        assert_eq!((interval.start(), interval.end()), (0, 100_000));

        // Revisiting the same locus on the same contig is still one entry.
        let mut panel = panel_at(None, "chr1:1-100000");
        visit(&mut panel, None, "chr1:1-100000");
        assert_eq!(panel.navigation_history.len(), 1);
    }

    #[test]
    fn back_lands_on_the_named_contig_when_dictionaries_are_ordered_differently() {
        let assembly = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 1_000_000),
            ("chr2".to_owned(), 900_000),
        ]);
        let alignment = ReferenceDictionary::from_contigs([
            ("chr2".to_owned(), 900_000),
            ("chr1".to_owned(), 1_000_000),
        ]);
        let mut panel = panel_at(Some(&alignment), "chr1:1-100000");
        let visited = panel.viewport.interval();
        visit(&mut panel, Some(&alignment), "chr2:1-100000");

        let entry = panel.navigation_history.back().cloned().unwrap();
        let (interval, name, length) = revisit(&panel, &entry, Some(&alignment));
        assert_eq!((name.as_str(), length), ("chr1", 1_000_000));
        assert_eq!(interval, visited);
        // The defect: the visited id read through the assembly names chr2.
        assert_eq!(assembly.get(visited.contig()).unwrap().name(), "chr2");

        // The same entry re-expressed for a differently ordered dictionary
        // (the source was swapped meanwhile) still names chr1.
        let (interval, name, _) = revisit(&panel, &entry, Some(&assembly));
        assert_eq!(name, "chr1");
        assert_eq!(interval.contig(), assembly.id_by_name("chr1").unwrap());

        let gone = NavigationEntry {
            contig_name: "chr9".to_owned(),
            start: 0,
            end: 10,
        };
        assert!(
            resolve_history_entry(
                &gone,
                Some(&alignment),
                &ContigAliases::default(),
                HistoryPanelContig {
                    id: ContigId::new(0),
                    name: "chr2",
                    length: 900_000,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn a_single_locus_never_replaces_the_other_panels() {
        assert_eq!(
            loci_navigation_plan(1),
            LociNavigationPlan::RetargetActivePanel
        );
        assert_eq!(
            loci_navigation_plan(2),
            LociNavigationPlan::ReplaceWorkspacePanels
        );
        assert_eq!(
            loci_navigation_plan(5),
            LociNavigationPlan::ReplaceWorkspacePanels
        );
    }
}
