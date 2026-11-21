use super::super::*;

/// Bases shown on each side of a single-position locus such as `chr1:12345`.
pub(crate) const SINGLE_POSITION_FLANK: u64 = 20;

/// Resolves a typed locus with the shared [`Locus`] grammar, or the opening
/// view (the first 100 kb of the first contig) when none was requested.
pub(crate) fn resolve_initial_locus(
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
    let interval = resolved.interval(SINGLE_POSITION_FLANK)?;
    Ok((interval, resolved.contig_name, resolved.contig_length))
}

/// What to open for an imported IGV session whose `Session.locus` was not a
/// coordinate interval (CORE-1). The imported document then only holds a
/// one-base placeholder, which must never be navigated to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ImportedLocusRequest {
    /// IGV's whole-genome view (`All`), or a missing or unparseable locus.
    Default,
    /// A bare contig, or a feature such as a gene symbol.
    Named(String),
}

pub(crate) fn imported_locus_request(
    resolution: &IgvLocusResolution,
) -> Option<ImportedLocusRequest> {
    match resolution {
        IgvLocusResolution::Interval => None,
        IgvLocusResolution::Named { name } => Some(ImportedLocusRequest::Named(name.clone())),
        IgvLocusResolution::Unresolved { .. } => Some(ImportedLocusRequest::Default),
    }
}

impl ImportedLocusRequest {
    /// The locus each panel asks for once a dictionary is known.
    pub(crate) fn requested_locus(&self) -> Option<String> {
        match self {
            Self::Default => None,
            Self::Named(name) => Some(name.clone()),
        }
    }

    pub(crate) fn feature_name(&self) -> Option<&str> {
        match self {
            Self::Default => None,
            Self::Named(name) => Some(name),
        }
    }
}

/// Shown until a dictionary places the imported session's real locus.
pub(crate) const IMPORTED_PLACEHOLDER_LOCUS: &str = "chr1:1-100000";

/// A panel's requested locus resolved against a newly opened dictionary.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RequestedLocus {
    pub(crate) interval: GenomicInterval,
    pub(crate) contig_name: String,
    pub(crate) contig_length: u64,
    /// Set when the request was an imported feature name rather than a
    /// contig: the default locus was opened and this name should be found by
    /// annotation search.
    pub(crate) feature_search: Option<String>,
}

/// [`resolve_initial_locus`], except that an imported IGV name which is not a
/// contig opens the default locus and asks for a feature search instead of
/// failing.
pub(crate) fn resolve_requested_locus(
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
pub(crate) fn locus_item_is_navigable(
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
pub(crate) fn locus_expression_is_navigable(
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

pub(crate) fn parse_contig_aliases(values: &[String]) -> Result<ContigAliases> {
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

pub(crate) fn launch_contig_aliases(options: &LaunchOptions) -> Result<ContigAliases> {
    if let Some(session) = &options.loaded_session {
        return Ok(session.contig_aliases.clone());
    }
    let mut aliases = options
        .loaded_genome
        .as_ref()
        .map_or_else(ContigAliases::default, |genome| {
            genome.contig_aliases.clone()
        });
    for (alias, target) in parse_contig_aliases(&options.contig_aliases)? {
        if let Some(previous) = aliases.insert(alias.clone(), target.clone()) {
            anyhow::ensure!(
                previous == target,
                "contig alias {alias} has conflicting targets {previous} and {target}"
            );
        }
    }
    anyhow::ensure!(
        aliases.len() <= rgv_core::MAXIMUM_CONTIG_ALIASES,
        "genome defines more than {} contig aliases",
        rgv_core::MAXIMUM_CONTIG_ALIASES
    );
    Ok(aliases)
}

/// A locus for a workspace with no reference dictionary: the contig name is
/// taken as typed and its length is unknown, so a coordinate range is needed.
pub(crate) fn standalone_locus(locus: &str) -> Result<(GenomicInterval, String, u64)> {
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
        GenomicInterval::new(ContigId::new(0), start, end)?,
        parsed.contig,
        contig_length,
    ))
}

/// Splits a locus-box expression into its loci (see [`rgv_core::split_loci`]).
pub(crate) fn split_locus_expression(expression: &str) -> Result<Vec<&str>> {
    let loci = rgv_core::split_loci(expression);
    anyhow::ensure!(!loci.is_empty(), "enter at least one genomic locus");
    anyhow::ensure!(
        loci.len() <= MAXIMUM_VIEW_PANELS,
        "a workspace supports at most {MAXIMUM_VIEW_PANELS} view panels"
    );
    Ok(loci)
}

pub(crate) fn preferred_annotation_search_hit<'a>(
    results: &'a TrackSearchResults,
    query: &str,
) -> Option<&'a TrackSearchHit> {
    let normalized = query.trim().to_lowercase();
    let mut exact = results
        .hits
        .iter()
        .filter(|hit| hit.matched_term == normalized);
    let first_exact = exact.next();
    if first_exact.is_some() && exact.next().is_none() {
        return first_exact;
    }
    if first_exact.is_none() && results.hits.len() == 1 {
        return results.hits.first();
    }
    None
}

pub(crate) fn annotation_search_hit_locus(hit: &TrackSearchHit) -> String {
    format!(
        "{}:{}-{}",
        hit.record.contig,
        hit.record.start.saturating_add(1),
        hit.record.end
    )
}

pub(crate) fn annotation_search_hit_primary_label(hit: &TrackSearchHit) -> &str {
    hit.record
        .display_label()
        .unwrap_or(hit.matched_term.as_str())
}

pub(crate) fn annotation_search_hit_secondary_identity(hit: &TrackSearchHit) -> String {
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

pub(crate) fn mate_locus(
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

pub(crate) fn mate_viewport(
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
