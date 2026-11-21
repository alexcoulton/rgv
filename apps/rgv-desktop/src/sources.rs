use super::*;

#[allow(clippy::too_many_lines)]
pub(super) fn build_track_specs(options: &LaunchOptions) -> (Vec<TrackSpec>, TrackCollection) {
    if let Some(session) = &options.loaded_session {
        return session_track_specs(session);
    }
    let mut specifications = Vec::new();
    let mut tracks = TrackCollection::default();
    let mut next_id = 1_u64;
    for (paths, specification_kind, track_kind, format) in [
        (&options.bed, TrackSpecKind::Bed, TrackKind::Feature, None),
        (
            &options.gff,
            TrackSpecKind::Gff3,
            TrackKind::GeneModel,
            None,
        ),
        (&options.gtf, TrackSpecKind::Gtf, TrackKind::GeneModel, None),
        (
            &options.gene_pred,
            TrackSpecKind::GenePred,
            TrackKind::GeneModel,
            None,
        ),
        (
            &options.ref_flat,
            TrackSpecKind::RefFlat,
            TrackKind::GeneModel,
            None,
        ),
        (
            &options.narrow_peak,
            TrackSpecKind::NarrowPeak,
            TrackKind::Feature,
            None,
        ),
        (
            &options.broad_peak,
            TrackSpecKind::BroadPeak,
            TrackKind::Feature,
            None,
        ),
        (&options.psl, TrackSpecKind::Psl, TrackKind::Feature, None),
        (&options.gwas, TrackSpecKind::Gwas, TrackKind::Gwas, None),
        (
            &options.vcf,
            TrackSpecKind::VcfOrBcf,
            TrackKind::Variant,
            None,
        ),
        (
            &options.bigwig,
            TrackSpecKind::BigWig,
            TrackKind::Quantitative,
            None,
        ),
        (
            &options.tdf,
            TrackSpecKind::Tdf,
            TrackKind::Quantitative,
            None,
        ),
        (
            &options.bedgraph,
            TrackSpecKind::BedGraph,
            TrackKind::Quantitative,
            None,
        ),
        (
            &options.wig,
            TrackSpecKind::Wig,
            TrackKind::Quantitative,
            None,
        ),
        (
            &options.bedpe,
            TrackSpecKind::BedPe,
            TrackKind::Interaction,
            None,
        ),
        (
            &options.interact,
            TrackSpecKind::Interact,
            TrackKind::Interaction,
            None,
        ),
        (
            &options.seg,
            TrackSpecKind::Seg,
            TrackKind::CopyNumber,
            None,
        ),
        (
            &options.maf,
            TrackSpecKind::MutationMaf,
            TrackKind::Mutation,
            None,
        ),
        (
            &options.bigbed,
            TrackSpecKind::BigBed,
            TrackKind::Feature,
            None,
        ),
        (
            &options.big_gene_pred,
            TrackSpecKind::BigGenePred,
            TrackKind::GeneModel,
            Some(TrackFormat::BigGenePred),
        ),
        (
            &options.big_narrow_peak,
            TrackSpecKind::BigNarrowPeak,
            TrackKind::Feature,
            Some(TrackFormat::BigNarrowPeak),
        ),
    ] {
        for path in paths {
            let id = TrackId::new(next_id);
            next_id = next_id.saturating_add(1);
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            specifications.push(TrackSpec {
                id,
                name: name.clone(),
                path: path.clone(),
                kind: specification_kind,
            });
            let mut config = TrackConfig::new(id, name, track_kind, track_source(path));
            config.format = format;
            tracks.push(config);
        }
    }
    if let Some(genome) = &options.loaded_genome {
        for annotation in &genome.default_annotations {
            let Some(kind) =
                track_spec_kind(annotation.kind, &annotation.source, annotation.format)
            else {
                continue;
            };
            let id = TrackId::new(next_id);
            next_id = next_id.saturating_add(1);
            specifications.push(TrackSpec {
                id,
                name: annotation.name.clone(),
                path: source_path(&annotation.source),
                kind,
            });
            let mut config = TrackConfig::new(
                id,
                annotation.name.clone(),
                annotation.kind,
                annotation.source.clone(),
            );
            config.format = annotation.format;
            tracks.push(config);
        }
    }
    (specifications, tracks)
}

pub(super) fn session_track_specs(session: &SessionDocument) -> (Vec<TrackSpec>, TrackCollection) {
    let mut specifications = Vec::new();
    let mut tracks = TrackCollection::default();
    for track in &session.tracks {
        let Some(kind) = track_spec_kind(track.kind, &track.source, track.format) else {
            continue;
        };
        specifications.push(TrackSpec {
            id: track.id,
            name: track.name.clone(),
            path: source_path(&track.source),
            kind,
        });
        tracks.push(track.clone());
    }
    (specifications, tracks)
}

pub(super) fn source_path(source: &TrackSource) -> PathBuf {
    match source {
        TrackSource::Local { path } => PathBuf::from(path),
        TrackSource::Remote { url } => PathBuf::from(url),
    }
}

pub(super) fn tdf_channel_display_name(
    base_name: &str,
    embedded_name: &str,
    index: usize,
) -> String {
    let embedded_name = embedded_name.trim();
    if embedded_name.is_empty() {
        format!("{base_name} · channel {}", index + 1)
    } else {
        embedded_name.to_owned()
    }
}

pub(super) struct TdfChannelExpansion {
    pub(super) selected_name: String,
    pub(super) specifications: Vec<TrackSpec>,
    pub(super) children: Vec<TrackConfig>,
}

pub(super) fn build_tdf_channel_expansion(
    parent: &TrackConfig,
    path: &Path,
    track_names: &[String],
    selected_index: usize,
    mut next_id: u64,
) -> Result<TdfChannelExpansion> {
    if track_names.is_empty() || selected_index >= track_names.len() {
        anyhow::bail!("TDF source returned an invalid embedded-channel catalogue");
    }
    let mut specifications = Vec::with_capacity(track_names.len().saturating_sub(1));
    let mut children = Vec::with_capacity(track_names.len().saturating_sub(1));
    for (index, embedded_name) in track_names.iter().enumerate() {
        if index == selected_index {
            continue;
        }
        let id = TrackId::new(next_id);
        next_id = next_id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("TDF channel track ID space is exhausted"))?;
        let name = tdf_channel_display_name(&parent.name, embedded_name, index);
        specifications.push(TrackSpec {
            id,
            name: name.clone(),
            path: path.to_path_buf(),
            kind: TrackSpecKind::TdfChannel { index },
        });
        let mut child = parent.clone();
        child.id = id;
        child.name = name;
        child.format = Some(TrackFormat::TdfChannel { index });
        children.push(child);
    }
    Ok(TdfChannelExpansion {
        selected_name: tdf_channel_display_name(
            &parent.name,
            &track_names[selected_index],
            selected_index,
        ),
        specifications,
        children,
    })
}

pub(super) fn inferred_automation_track_kind(
    source: &TrackSource,
    explicit: Option<TrackKind>,
) -> TrackKind {
    if let Some(kind) = explicit {
        return kind;
    }
    classify_genomic_source(&source.redacted_label())
        .map_or(TrackKind::Feature, rgv_core::ClassifiedGenomicSource::kind)
}

pub(super) fn opened_project_status(path: &Path, unresolved: &[UnresolvedSessionSource]) -> String {
    if unresolved.is_empty() {
        return format!("Opened project {}", path.display());
    }
    let sources = unresolved
        .iter()
        .map(|issue| format!("{}: {}", issue.label, issue.path.display()))
        .collect::<Vec<_>>()
        .join(" · ");
    format!(
        "Opened project {} with {} unresolved local source(s) · {sources}",
        path.display(),
        unresolved.len()
    )
}

pub(super) fn primary_alignment_track(session: &SessionDocument) -> Option<&SessionAlignmentTrack> {
    session.alignment_tracks.first()
}

pub(super) fn upsert_primary_alignment_config<'a>(
    alignment_tracks: &'a mut Vec<SessionAlignmentTrack>,
    auxiliary_tracks: &[TrackConfig],
    source: TrackSource,
    name: &str,
) -> &'a mut SessionAlignmentTrack {
    if alignment_tracks.is_empty() {
        let next_id = alignment_tracks
            .iter()
            .map(|track| track.id.get())
            .chain(auxiliary_tracks.iter().map(|track| track.id.get()))
            .max()
            .unwrap_or_default()
            .saturating_add(1);
        alignment_tracks.push(SessionAlignmentTrack::new(
            TrackId::new(next_id),
            name.to_owned(),
            source.clone(),
        ));
    }
    let primary = &mut alignment_tracks[0];
    name.clone_into(&mut primary.name);
    primary.source = source;
    primary
}

pub(super) fn source_display_name(source: Option<&TrackSource>, fallback: &str) -> String {
    source.map_or_else(
        || fallback.to_owned(),
        |source| {
            let label = source.redacted_label();
            Path::new(&label)
                .file_name()
                .map_or(label.clone(), |name| name.to_string_lossy().into_owned())
        },
    )
}

pub(super) fn load_assembly(
    name: &str,
    dictionary: ReferenceDictionary,
    cytoband_path: Option<&Path>,
) -> GenomeAssembly {
    load_assembly_reporting(name, dictionary, cytoband_path).0
}

/// As [`load_assembly`], also returning why the cytoband file could not be used. The assembly
/// then falls back to the plain ideogram, which must be explained to the user rather than only
/// written to the log.
pub(super) fn load_assembly_reporting(
    name: &str,
    dictionary: ReferenceDictionary,
    cytoband_path: Option<&Path>,
) -> (GenomeAssembly, Option<String>) {
    let mut assembly = GenomeAssembly::from_dictionary(name, dictionary);
    let Some(path) = cytoband_path else {
        return (assembly, None);
    };
    match std::fs::read_to_string(path)
        .with_context(|| format!("failed to read cytobands from {}", path.display()))
        .and_then(|text| parse_ucsc_cytobands(&text, &assembly.dictionary).map_err(Into::into))
    {
        Ok(cytobands) => {
            assembly.cytobands = cytobands;
            (assembly, None)
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "using ideogram fallback");
            let failure = format!(
                "Could not load cytobands from {}: {error:#}; showing the plain ideogram",
                path.display()
            );
            (assembly, Some(failure))
        }
    }
}

pub(super) fn alignment_open_options(
    reference_path: Option<PathBuf>,
    remote_index_url: Option<String>,
    remote_reference_index_url: Option<String>,
) -> AlignmentOpenOptions {
    let mut options = AlignmentOpenOptions {
        reference_path,
        remote_index_url,
        remote_reference_index_url,
        ..AlignmentOpenOptions::default()
    };
    options.remote_range = remote_range_config();
    options
}

pub(super) fn reference_open_options(remote_index_url: Option<String>) -> FastaOpenOptions {
    FastaOpenOptions {
        remote_index_url,
        remote_range: remote_range_config(),
        ..FastaOpenOptions::default()
    }
}

/// Starts the annotation-track engine with the desktop's remote configuration, so remote
/// bigWig, bigBed, TDF and text tracks get the same bearer authentication, cancellation and
/// budgets as remote alignments instead of anonymous defaults.
pub(super) fn spawn_track_engine(
    specifications: Vec<(TrackSpec, BTreeMap<String, String>)>,
) -> TrackEngine {
    TrackEngine::spawn_with_remote_config(specifications, 8, remote_range_config())
}

/// [`spawn_track_engine`] for tracks that need no query-contig aliases.
pub(super) fn spawn_track_engine_without_aliases(specifications: Vec<TrackSpec>) -> TrackEngine {
    spawn_track_engine(
        specifications
            .into_iter()
            .map(|specification| (specification, BTreeMap::new()))
            .collect(),
    )
}

pub(super) fn remote_range_config() -> HttpRangeConfig {
    let mut config = HttpRangeConfig {
        minimum_coalesced_chunks: NonZeroUsize::new(8).unwrap(),
        maximum_coalesced_chunks: NonZeroUsize::new(8).unwrap(),
        ..HttpRangeConfig::default()
    };
    if let Ok(token) = std::env::var("RGV_REMOTE_BEARER_TOKEN")
        && !token.is_empty()
    {
        config.authentication = RemoteAuthentication::Bearer(token);
    }
    config
}

pub(super) fn track_spec_kind(
    kind: TrackKind,
    source: &TrackSource,
    format: Option<TrackFormat>,
) -> Option<TrackSpecKind> {
    if let Some(format) = format {
        return Some(match format {
            TrackFormat::BigGenePred => TrackSpecKind::BigGenePred,
            TrackFormat::BigNarrowPeak => TrackSpecKind::BigNarrowPeak,
            TrackFormat::TdfChannel { index } => TrackSpecKind::TdfChannel { index },
        });
    }
    let inferred = classify_genomic_source(&source.redacted_label()).map(|source| source.format);
    match kind {
        TrackKind::Feature => Some(match inferred {
            Some(GenomicSourceFormat::BigNarrowPeak) => TrackSpecKind::BigNarrowPeak,
            Some(GenomicSourceFormat::BigBed) => TrackSpecKind::BigBed,
            Some(GenomicSourceFormat::NarrowPeak) => TrackSpecKind::NarrowPeak,
            Some(GenomicSourceFormat::BroadPeak) => TrackSpecKind::BroadPeak,
            Some(GenomicSourceFormat::Psl) => TrackSpecKind::Psl,
            _ => TrackSpecKind::Bed,
        }),
        TrackKind::GeneModel => Some(match inferred {
            Some(GenomicSourceFormat::BigGenePred | GenomicSourceFormat::BigBed) => {
                TrackSpecKind::BigGenePred
            }
            Some(GenomicSourceFormat::Gtf) => TrackSpecKind::Gtf,
            Some(GenomicSourceFormat::GenePred) => TrackSpecKind::GenePred,
            Some(GenomicSourceFormat::RefFlat) => TrackSpecKind::RefFlat,
            _ => TrackSpecKind::Gff3,
        }),
        TrackKind::Variant => Some(TrackSpecKind::VcfOrBcf),
        TrackKind::Mutation => Some(TrackSpecKind::MutationMaf),
        TrackKind::Gwas => Some(TrackSpecKind::Gwas),
        TrackKind::Quantitative => Some(match inferred {
            Some(GenomicSourceFormat::BedGraph) => TrackSpecKind::BedGraph,
            Some(GenomicSourceFormat::Wig) => TrackSpecKind::Wig,
            Some(GenomicSourceFormat::Tdf) => TrackSpecKind::Tdf,
            _ => TrackSpecKind::BigWig,
        }),
        TrackKind::CopyNumber => Some(TrackSpecKind::Seg),
        TrackKind::Interaction => Some(if inferred == Some(GenomicSourceFormat::Interact) {
            TrackSpecKind::Interact
        } else {
            TrackSpecKind::BedPe
        }),
        TrackKind::Alignment | TrackKind::Reference => None,
    }
}

pub(super) fn inferred_desktop_track_kind(path: &Path) -> Result<(TrackSpecKind, TrackKind)> {
    let classified = classify_genomic_source(&path.to_string_lossy())
        .with_context(|| format!("unsupported track format {}", path.display()))?;
    anyhow::ensure!(
        classified.is_supported(),
        "unsupported compressed track format {}",
        path.display()
    );
    let specification = match classified.format {
        GenomicSourceFormat::Bed => TrackSpecKind::Bed,
        GenomicSourceFormat::Gff3 => TrackSpecKind::Gff3,
        GenomicSourceFormat::Gtf => TrackSpecKind::Gtf,
        GenomicSourceFormat::GenePred => TrackSpecKind::GenePred,
        GenomicSourceFormat::RefFlat => TrackSpecKind::RefFlat,
        GenomicSourceFormat::NarrowPeak => TrackSpecKind::NarrowPeak,
        GenomicSourceFormat::BroadPeak => TrackSpecKind::BroadPeak,
        GenomicSourceFormat::Psl => TrackSpecKind::Psl,
        GenomicSourceFormat::Gwas => TrackSpecKind::Gwas,
        GenomicSourceFormat::Vcf | GenomicSourceFormat::Bcf => TrackSpecKind::VcfOrBcf,
        GenomicSourceFormat::BigWig => TrackSpecKind::BigWig,
        GenomicSourceFormat::Tdf => TrackSpecKind::Tdf,
        GenomicSourceFormat::BedGraph => TrackSpecKind::BedGraph,
        GenomicSourceFormat::Wig => TrackSpecKind::Wig,
        GenomicSourceFormat::BedPe => TrackSpecKind::BedPe,
        GenomicSourceFormat::Interact => TrackSpecKind::Interact,
        GenomicSourceFormat::Seg => TrackSpecKind::Seg,
        GenomicSourceFormat::MutationMaf => TrackSpecKind::MutationMaf,
        GenomicSourceFormat::BigBed => TrackSpecKind::BigBed,
        GenomicSourceFormat::BigGenePred => TrackSpecKind::BigGenePred,
        GenomicSourceFormat::BigNarrowPeak => TrackSpecKind::BigNarrowPeak,
        GenomicSourceFormat::Bam | GenomicSourceFormat::Cram | GenomicSourceFormat::Fasta => {
            anyhow::bail!("unsupported track format {}", path.display())
        }
    };
    Ok((specification, classified.kind()))
}

/// What a dropped (or picked) file is opened as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DroppedFileKind {
    Alignment,
    Reference,
    GenomeConfiguration,
    /// A `.json` document whose content decides: see [`classify_dropped_json`].
    Json,
    IgvSession,
    /// Anything else is offered to the track loader, which reports unsupported formats.
    Track,
}

/// Classifies by the same extension table every other open path uses
/// ([`classify_genomic_source`]), so compressed and less common spellings behave identically
/// whether a file is dropped, picked or passed on the command line.
pub(super) fn classify_dropped_file(path: &Path) -> DroppedFileKind {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "rgvgenome" => return DroppedFileKind::GenomeConfiguration,
        "json" => return DroppedFileKind::Json,
        "xml" => return DroppedFileKind::IgvSession,
        _ => {}
    }
    match classify_genomic_source(&path.to_string_lossy()).map(|classified| classified.format) {
        Some(GenomicSourceFormat::Bam | GenomicSourceFormat::Cram) => DroppedFileKind::Alignment,
        Some(GenomicSourceFormat::Fasta) => DroppedFileKind::Reference,
        _ => DroppedFileKind::Track,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DroppedJsonKind {
    IgvGenome,
    GenomeConfiguration,
    Session,
}

/// Decides what a dropped JSON document is from one parse of its text. Anything that is not
/// recognisably a genome is a project session; the session loader reports invalid documents.
pub(super) fn classify_dropped_json(text: &str) -> DroppedJsonKind {
    let Ok(serde_json::Value::Object(object)) = serde_json::from_str::<serde_json::Value>(text)
    else {
        return DroppedJsonKind::Session;
    };
    let has_text = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    };
    if has_text("fastaURL") || has_text("twoBitURL") {
        DroppedJsonKind::IgvGenome
    } else if object.contains_key("reference") && !object.contains_key("locus") {
        DroppedJsonKind::GenomeConfiguration
    } else {
        DroppedJsonKind::Session
    }
}

pub(super) fn track_source(path: &std::path::Path) -> TrackSource {
    let value = path.display().to_string();
    if value.starts_with("http://") || value.starts_with("https://") {
        TrackSource::Remote { url: value }
    } else {
        TrackSource::Local { path: value }
    }
}
