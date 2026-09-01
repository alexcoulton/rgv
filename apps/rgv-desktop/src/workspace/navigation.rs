use super::super::*;

pub(crate) fn resolve_initial_locus(
    dictionary: &ReferenceDictionary,
    aliases: &ContigAliases,
    requested: Option<&str>,
) -> Result<(GenomicInterval, String, u64)> {
    let (name, range) = if let Some(requested) = requested {
        requested
            .split_once(':')
            .map_or((requested, None), |(name, range)| (name, Some(range)))
    } else {
        let first = dictionary
            .iter()
            .next()
            .context("alignment header has no reference sequences")?;
        (first.name(), None)
    };
    let contig_id = resolve_contig_alias(dictionary, aliases, name)?
        .with_context(|| format!("alignment header has no contig named {name}"))?;
    let contig = dictionary
        .get(contig_id)
        .context("alignment reference dictionary is inconsistent")?;
    if contig.length() == 0 {
        anyhow::bail!("contig {name} has zero length");
    }

    let (start, end) = if let Some(range) = range {
        let range = range.replace(',', "");
        let (start, end) = range
            .split_once('-')
            .with_context(|| format!("locus must use name:start-end, got {range}"))?;
        let one_based_start: u64 = start
            .parse()
            .with_context(|| format!("invalid locus start {start}"))?;
        let one_based_end: u64 = end
            .parse()
            .with_context(|| format!("invalid locus end {end}"))?;
        if one_based_start == 0 || one_based_end < one_based_start {
            anyhow::bail!("locus coordinates must be one-based and forward");
        }
        (one_based_start - 1, one_based_end)
    } else {
        (0, contig.length().min(100_000))
    };
    let interval = GenomicInterval::bounded(contig_id, start, end, contig.length())?;
    Ok((interval, contig.name().to_owned(), contig.length()))
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
                && !alias.contains([':', ' ', '\t', '\n'])
                && !target.contains([':', ' ', '\t', '\n']),
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

pub(crate) fn standalone_locus(locus: &str) -> Result<(GenomicInterval, String, u64)> {
    let (name, range) = locus
        .split_once(':')
        .with_context(|| format!("standalone track locus must use name:start-end, got {locus}"))?;
    let range = range.replace(',', "");
    let (start, end) = range
        .split_once('-')
        .with_context(|| format!("standalone track locus must use name:start-end, got {locus}"))?;
    let one_based_start: u64 = start
        .parse()
        .with_context(|| format!("invalid locus start {start}"))?;
    let one_based_end: u64 = end
        .parse()
        .with_context(|| format!("invalid locus end {end}"))?;
    if one_based_start == 0 || one_based_end < one_based_start {
        anyhow::bail!("locus coordinates must be one-based and forward");
    }
    let contig_length = one_based_end
        .saturating_mul(2)
        .max(one_based_end.saturating_add(1_000_000));
    Ok((
        GenomicInterval::new(ContigId::new(0), one_based_start - 1, one_based_end)?,
        name.to_owned(),
        contig_length,
    ))
}

pub(crate) fn split_locus_expression(expression: &str) -> Result<Vec<&str>> {
    let loci = expression
        .split_ascii_whitespace()
        .filter(|locus| !locus.is_empty())
        .collect::<Vec<_>>();
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
