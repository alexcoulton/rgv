//! IGV-compatible genomes: the hosted genome list and loading a genome description off the UI
//! thread. Small genome resources are cached on disk by `rgv-hts`, so a genome which has been
//! opened once can be opened again offline.

use super::super::*;

use rgv_core::{IGV_HOSTED_GENOME_LIST_URL, IgvGenomeDescription, IgvGenomeListEntry};
use rgv_hts::{IgvGenomeImport, load_hosted_igv_genome, load_igv_genome, load_igv_genome_list};

/// Where genome descriptions and their small companions are cached. `RGV_GENOME_CACHE_DIR`
/// overrides the default location beside the settings file.
pub(crate) fn genome_cache_directory() -> PathBuf {
    if let Some(directory) = std::env::var_os("RGV_GENOME_CACHE_DIR") {
        return PathBuf::from(directory);
    }
    default_settings_path()
        .parent()
        .map_or_else(|| PathBuf::from("genomes"), |parent| parent.join("genomes"))
}

/// The hosted genome list to read. `RGV_GENOME_LIST_URL` overrides IGV's own list and may name a
/// local file.
pub(crate) fn hosted_genome_list_location() -> String {
    std::env::var("RGV_GENOME_LIST_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| IGV_HOSTED_GENOME_LIST_URL.to_owned())
}

/// Whether a JSON file is an IGV genome description rather than an RGV document.
pub(crate) fn is_igv_genome_description(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .is_some_and(|text| IgvGenomeDescription::parse(&text).is_ok())
}

/// Loads a genome by hosted id (`hg38`), description URL, or description path. Blocking: call
/// from a worker thread, or at start-up before the window exists.
pub(crate) fn load_genome_blocking(genome: &str) -> Result<IgvGenomeImport> {
    let cache = genome_cache_directory();
    let remote = remote_range_config();
    let is_location = genome.starts_with("http://")
        || genome.starts_with("https://")
        || Path::new(genome).is_file();
    let import = if is_location {
        load_igv_genome(genome, &cache, &remote)
    } else {
        load_hosted_igv_genome(genome, &hosted_genome_list_location(), &cache, &remote)
    }
    .with_context(|| format!("could not load genome {genome}"))?;
    Ok(import)
}

/// Fills an imported IGV session's reference, cytobands, aliases and assembly identity from the
/// genome its `genome` attribute names. Values the session already carries are kept.
pub(crate) fn apply_genome_to_imported_session(
    document: &mut SessionDocument,
    report: &mut IgvImportReport,
    configuration: &GenomeConfiguration,
) {
    if document.reference.is_none() {
        document.reference = Some(configuration.reference.clone());
        document
            .remote_reference_index_url
            .clone_from(&configuration.reference_index);
    }
    if document.cytobands.is_none() {
        document.cytobands.clone_from(&configuration.cytobands);
    }
    for (alias, target) in &configuration.contig_aliases {
        document
            .contig_aliases
            .entry(alias.clone())
            .or_insert_with(|| target.clone());
    }
    if document.assembly_identity.is_none() {
        document
            .assembly_identity
            .clone_from(&configuration.assembly_identity);
    }
    report
        .approximated
        .retain(|notice| !notice.starts_with("Session.genome identifier"));
    report.imported.push(format!(
        "Session.genome -> hosted genome {} (reference, cytobands and chromosome aliases)",
        configuration.name
    ));
    if !configuration.default_annotations.is_empty() {
        report.approximated.push(
            "The genome's default annotation tracks were not added; only tracks listed in the \
             session were imported"
                .to_owned(),
        );
    }
    document.genome_name = Some(configuration.name.clone());
}

/// Resolves an imported IGV session's genome id (for example `hg38`) through the hosted genome
/// list. Blocking: call from a worker thread or at start-up. A failure leaves the session as
/// imported and is recorded in the report.
pub(crate) fn resolve_imported_session_genome(
    document: &mut SessionDocument,
    report: &mut IgvImportReport,
) {
    // Unit tests import IGV sessions freely; they must not reach the network or write to the
    // user's genome cache.
    if cfg!(test) || document.reference.is_some() {
        return;
    }
    let Some(genome) = document.genome_name.clone() else {
        return;
    };
    match load_genome_blocking(&genome) {
        Ok(import) => apply_genome_to_imported_session(document, report, &import.configuration),
        Err(error) => report.approximated.push(format!(
            "Session.genome '{genome}' could not be resolved to a hosted genome: {error:#}"
        )),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum HostedGenomeListStatus {
    #[default]
    NotLoaded,
    Loading,
    Loaded {
        /// The server could not be reached and the list is the last cached copy.
        from_cache: bool,
    },
    Failed(String),
}

#[derive(Debug, Default)]
pub(crate) struct HostedGenomes {
    pub(crate) entries: Vec<IgvGenomeListEntry>,
    pub(crate) status: HostedGenomeListStatus,
    pub(crate) filter: String,
}

impl HostedGenomes {
    /// Entries whose name or id contains the filter, case-insensitively.
    pub(crate) fn matching<'a>(
        entries: &'a [IgvGenomeListEntry],
        filter: &str,
    ) -> impl Iterator<Item = &'a IgvGenomeListEntry> {
        let needle = filter.trim().to_lowercase();
        entries.iter().filter(move |entry| {
            needle.is_empty()
                || entry.name.to_lowercase().contains(&needle)
                || entry.id.to_lowercase().contains(&needle)
        })
    }
}

pub(crate) enum GenomeTaskOutcome {
    List {
        entries: Vec<IgvGenomeListEntry>,
        from_cache: bool,
    },
    Genome(Box<IgvGenomeImport>),
    /// A built-in landmark bundle verified and decoded off the event loop.
    Landmark(Box<crate::workspace::landmarks::PreparedLandmarkContext>),
}

pub(crate) struct GenomeTask {
    pub(crate) label: String,
    pub(crate) is_list: bool,
    /// Set for a landmark install: what the preparation was computed for.
    pub(crate) landmark: Option<crate::workspace::landmarks::LandmarkInstallRequest>,
    pub(crate) worker: JoinHandle<std::result::Result<GenomeTaskOutcome, String>>,
}

impl Viewer {
    pub(crate) fn start_hosted_genome_list_refresh(&mut self) {
        if self.hosted_genomes.status == HostedGenomeListStatus::Loading {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("rgv-genome-list".to_owned())
            .spawn(|| {
                load_igv_genome_list(
                    &hosted_genome_list_location(),
                    &genome_cache_directory(),
                    &remote_range_config(),
                )
                .map(|(entries, from_cache)| GenomeTaskOutcome::List {
                    entries,
                    from_cache,
                })
                .map_err(|error| error.to_string())
            });
        match spawned {
            Ok(worker) => {
                self.hosted_genomes.status = HostedGenomeListStatus::Loading;
                self.genome_tasks.push(GenomeTask {
                    label: "hosted genome list".to_owned(),
                    is_list: true,
                    landmark: None,
                    worker,
                });
            }
            Err(error) => {
                self.hosted_genomes.status = HostedGenomeListStatus::Failed(error.to_string());
            }
        }
    }

    /// Loads a genome by hosted id, description URL, or description path without blocking the
    /// interface; the genome is applied when its resources are ready.
    pub(crate) fn start_genome_load(&mut self, genome: String) {
        let label = genome.clone();
        match std::thread::Builder::new()
            .name("rgv-genome-load".to_owned())
            .spawn(move || {
                load_genome_blocking(&genome)
                    .map(|import| GenomeTaskOutcome::Genome(Box::new(import)))
                    .map_err(|error| format!("{error:#}"))
            }) {
            Ok(worker) => {
                self.data_status = format!("Loading genome {label}…");
                self.genome_tasks.push(GenomeTask {
                    label,
                    is_list: false,
                    landmark: None,
                    worker,
                });
            }
            Err(error) => self.report_error(format!("Could not start genome load: {error}")),
        }
        self.mark_display_dirty("explicit display invalidation");
    }

    pub(crate) fn poll_genome_tasks(&mut self) {
        let mut index = 0;
        while index < self.genome_tasks.len() {
            if !self.genome_tasks[index].worker.is_finished() {
                index += 1;
                continue;
            }
            let task = self.genome_tasks.remove(index);
            let outcome = task
                .worker
                .join()
                .unwrap_or_else(|_| Err("the genome worker stopped unexpectedly".to_owned()));
            if let Some(request) = &task.landmark {
                let outcome = outcome.and_then(|outcome| match outcome {
                    GenomeTaskOutcome::Landmark(prepared) => Ok(prepared),
                    _ => Err("the landmark worker returned an unrelated result".to_owned()),
                });
                self.finish_landmark_install(request, outcome);
                self.mark_display_dirty("explicit display invalidation");
                continue;
            }
            match outcome {
                Ok(GenomeTaskOutcome::Landmark(_)) => {}
                Ok(GenomeTaskOutcome::List {
                    entries,
                    from_cache,
                }) => {
                    self.hosted_genomes.entries = entries;
                    self.hosted_genomes.status = HostedGenomeListStatus::Loaded { from_cache };
                }
                Ok(GenomeTaskOutcome::Genome(import)) => {
                    let IgvGenomeImport {
                        configuration,
                        notices,
                    } = *import;
                    let name = configuration.name.clone();
                    match self.apply_genome_configuration(&configuration) {
                        Ok(()) => {
                            self.data_status = if notices.is_empty() {
                                format!("Opening genome {name}…")
                            } else {
                                format!("Opening genome {name}… ({})", notices.join("; "))
                            };
                        }
                        Err(error) => {
                            self.report_error(format!("Could not open genome {name}: {error:#}"));
                        }
                    }
                    for notice in notices {
                        tracing::warn!(genome = %name, %notice, "genome description notice");
                    }
                }
                Err(error) if task.is_list => {
                    self.hosted_genomes.status = HostedGenomeListStatus::Failed(error);
                }
                Err(error) => {
                    self.report_error(format!("Could not load genome {}: {error}", task.label));
                }
            }
            self.mark_display_dirty("explicit display invalidation");
        }
    }

    /// Applies a genome: assembly identity, aliases, cytobands, reference and default tracks.
    pub(crate) fn apply_genome_configuration(
        &mut self,
        configuration: &GenomeConfiguration,
    ) -> Result<()> {
        let cytoband_path = match &configuration.cytobands {
            Some(TrackSource::Local { path }) => Some(PathBuf::from(path)),
            Some(TrackSource::Remote { .. }) => {
                anyhow::bail!("remote cytoband files are not yet supported")
            }
            None => None,
        };
        let annotations = configuration
            .default_annotations
            .iter()
            .map(|annotation| {
                track_spec_kind(annotation.kind, &annotation.source, annotation.format)
                    .map(|kind| {
                        (
                            source_path(&annotation.source),
                            kind,
                            annotation.kind,
                            annotation.name.clone(),
                            annotation.format,
                        )
                    })
                    .with_context(|| format!("unsupported default annotation {}", annotation.name))
            })
            .collect::<Result<Vec<_>>>()?;

        self.assembly_name.clone_from(&configuration.name);
        self.assembly_identity
            .clone_from(&configuration.assembly_identity);
        self.assembly
            .identity
            .clone_from(&configuration.assembly_identity);
        self.assembly.default_annotation_count = configuration.default_annotations.len();
        self.contig_aliases
            .clone_from(&configuration.contig_aliases);
        self.cytoband_path = cytoband_path;
        let reference = source_path(&configuration.reference);
        self.open_reference_source_with_remote_index(
            &reference,
            configuration.reference_index.clone(),
        );
        for (annotation_path, kind, track_kind, name, format) in annotations {
            self.add_configured_track_source(&annotation_path, kind, track_kind, name, format)?;
        }
        Ok(())
    }
}
