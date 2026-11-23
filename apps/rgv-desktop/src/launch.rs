use crate::control_service::read_control_secret;
use crate::sources::{primary_alignment_track, source_path, track_source};
use crate::workspace::navigation::{ImportedLocusRequest, imported_locus_request};
use anyhow::{Context as _, Result};
use clap::Parser;
use rgv_automation::AutomationBatch;
use rgv_core::{
    DesktopSettings, GenomeConfiguration, GenomeLandmarkCatalogue, IgvImportReport,
    LandmarkAssetRole, SessionDocument, TrackSource,
};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Parser)]
#[command(about = "A fast native genome viewer written in Rust")]
pub(super) struct LaunchOptions {
    /// Indexed BAM or CRAM file to open. If omitted, the synthetic GPU demo is shown.
    #[arg(long)]
    pub(super) bam: Option<PathBuf>,
    /// Reference FASTA used for CRAM decoding and future base-level comparison.
    #[arg(long)]
    pub(super) reference: Option<PathBuf>,
    /// Explicit BAI, CSI, or CRAI URL for a remote BAM/CRAM. Conventional adjacent URLs are tried otherwise.
    #[arg(long)]
    pub(super) remote_index_url: Option<String>,
    /// Explicit FAI URL for a remote reference FASTA. The conventional `.fai` URL is used otherwise.
    #[arg(long)]
    pub(super) remote_reference_index_url: Option<String>,
    /// Initial one-based locus, for example chr1:900001-920000.
    #[arg(long)]
    pub(super) locus: Option<String>,
    /// Display name for the reference assembly loaded from the alignment header.
    #[arg(long, default_value = "BAM header")]
    pub(super) genome_name: String,
    /// Directory containing the schema-versioned built-in landmark catalog (or its parent
    /// holding an `igv/` catalogue). Defaults, in order, to `RGV_LANDMARK_RESOURCE_ROOT`, the
    /// landmark folder setting, the user data directory, then bundled/development resources.
    #[arg(long)]
    pub(super) landmark_resource_root: Option<PathBuf>,
    /// Portable JSON genome bundle describing FASTA, aliases, cytobands, and default annotations.
    #[arg(long, conflicts_with_all = ["session", "import_igv"])]
    pub(super) genome_config: Option<PathBuf>,
    /// IGV-compatible genome: a hosted genome id such as hg38, or the path or URL of an IGV
    /// genome JSON. Its small resources are cached, so it opens offline afterwards.
    #[arg(long, conflicts_with_all = ["session", "import_igv", "genome_config"])]
    pub(super) genome: Option<String>,
    /// UCSC cytoband TSV used to draw chromosome ideograms.
    #[arg(long)]
    pub(super) cytobands: Option<PathBuf>,
    /// Explicit contig alias in ALIAS=CANONICAL form. May be repeated.
    #[arg(long = "contig-alias", value_name = "ALIAS=CANONICAL")]
    pub(super) contig_aliases: Vec<String>,
    /// Complete read limit: at or below it every eligible viewport read is shown; above it no individual reads are shown.
    #[arg(long, default_value_t = 50_000)]
    pub(super) max_alignments: usize,
    /// BED or BED12 feature track. May be repeated.
    #[arg(long)]
    pub(super) bed: Vec<PathBuf>,
    /// GFF3 gene or feature track. May be repeated.
    #[arg(long)]
    pub(super) gff: Vec<PathBuf>,
    /// GTF gene model track. May be repeated.
    #[arg(long)]
    pub(super) gtf: Vec<PathBuf>,
    /// UCSC genePred transcript model. May be repeated.
    #[arg(long)]
    pub(super) gene_pred: Vec<PathBuf>,
    /// UCSC refFlat transcript model. May be repeated.
    #[arg(long)]
    pub(super) ref_flat: Vec<PathBuf>,
    /// ENCODE narrowPeak BED6+4 feature track. May be repeated.
    #[arg(long)]
    pub(super) narrow_peak: Vec<PathBuf>,
    /// ENCODE broadPeak BED6+3 feature track. May be repeated.
    #[arg(long)]
    pub(super) broad_peak: Vec<PathBuf>,
    /// UCSC PSL target-alignment track. May be repeated.
    #[arg(long)]
    pub(super) psl: Vec<PathBuf>,
    /// IGV/PLINK-compatible GWAS summary track. May be repeated.
    #[arg(long)]
    pub(super) gwas: Vec<PathBuf>,
    /// Indexed VCF or BCF variant track. May be repeated.
    #[arg(long)]
    pub(super) vcf: Vec<PathBuf>,
    /// `BigWig` quantitative track. May be repeated.
    #[arg(long)]
    pub(super) bigwig: Vec<PathBuf>,
    /// IGV tiled data format (TDF) quantitative track. May be repeated.
    #[arg(long)]
    pub(super) tdf: Vec<PathBuf>,
    /// bedGraph quantitative track. May be repeated.
    #[arg(long)]
    pub(super) bedgraph: Vec<PathBuf>,
    /// fixedStep or variableStep WIG quantitative track. May be repeated.
    #[arg(long)]
    pub(super) wig: Vec<PathBuf>,
    /// BEDPE interaction track. May be repeated.
    #[arg(long)]
    pub(super) bedpe: Vec<PathBuf>,
    /// UCSC 18-column interact track. May be repeated.
    #[arg(long)]
    pub(super) interact: Vec<PathBuf>,
    /// SEG/CBS multi-sample segmented copy-number track. May be repeated.
    #[arg(long)]
    pub(super) seg: Vec<PathBuf>,
    /// Mutation Annotation Format (cancer MAF) track. May be repeated.
    #[arg(long)]
    pub(super) maf: Vec<PathBuf>,
    /// `BigBed` feature track. May be repeated.
    #[arg(long)]
    pub(super) bigbed: Vec<PathBuf>,
    /// UCSC bigGenePred (`BigBed` BED12+8) transcript track. May be repeated.
    #[arg(long)]
    pub(super) big_gene_pred: Vec<PathBuf>,
    /// ENCODE bigNarrowPeak (`BigBed` BED6+4) peak track. May be repeated.
    #[arg(long)]
    pub(super) big_narrow_peak: Vec<PathBuf>,
    /// Open a native RGV JSON session.
    #[arg(long, conflicts_with = "import_igv")]
    pub(super) session: Option<PathBuf>,
    /// Import a subset of an IGV XML session.
    #[arg(long)]
    pub(super) import_igv: Option<PathBuf>,
    /// Override the platform settings-file location.
    #[arg(long)]
    pub(super) settings_path: Option<PathBuf>,
    /// Directory used for manual interaction traces and automatic critical-failure snapshots.
    #[arg(long)]
    pub(super) diagnostic_trace_dir: Option<PathBuf>,
    /// Begin a bounded manual interaction trace as soon as the viewer is initialized.
    #[arg(long)]
    pub(super) start_diagnostic_trace: bool,
    /// Run a versioned deterministic automation batch.
    #[arg(long)]
    pub(super) batch: Option<PathBuf>,
    /// Write structured automation results here (defaults beside the batch).
    #[arg(long, requires = "batch")]
    pub(super) automation_report: Option<PathBuf>,
    /// Enable authenticated local control on 127.0.0.1 at this port. Port 0 selects a free port.
    #[arg(long, requires = "control_secret_file", conflicts_with = "batch")]
    pub(super) control_port: Option<u16>,
    /// Read the local-control bearer secret from this protected file.
    #[arg(long, requires = "control_port", conflicts_with = "batch")]
    pub(super) control_secret_file: Option<PathBuf>,
    /// Restrict local-control input and output paths to this directory.
    #[arg(long, requires = "control_port")]
    pub(super) control_root: Option<PathBuf>,
    /// Let local-control clients open URLs on loopback, link-local and private-network
    /// addresses. Off by default so a control client cannot use the viewer to reach services
    /// only this machine or network can see.
    #[arg(long, requires = "control_port")]
    pub(super) control_allow_private_urls: bool,
    #[arg(skip)]
    pub(super) loaded_session: Option<SessionDocument>,
    #[arg(skip)]
    pub(super) loaded_genome: Option<GenomeConfiguration>,
    #[arg(skip)]
    pub(super) loaded_landmark_catalogue: Option<GenomeLandmarkCatalogue>,
    #[arg(skip)]
    pub(super) landmark_catalogue_error: Option<String>,
    #[arg(skip)]
    pub(super) landmark_resource_source: Option<rgv_core::LandmarkRootSource>,
    #[arg(skip)]
    pub(super) loaded_settings: DesktopSettings,
    #[arg(skip)]
    pub(super) loaded_settings_recovery: Option<rgv_core::SettingsRecovery>,
    #[arg(skip)]
    pub(super) loaded_igv_report: Option<IgvImportReport>,
    /// Set when the imported IGV session's locus was not an interval.
    #[arg(skip)]
    pub(super) loaded_igv_locus: Option<ImportedLocusRequest>,
    #[arg(skip)]
    pub(super) recovered_unclean_exit: bool,
    #[arg(skip)]
    pub(super) recovery_candidates: Vec<PathBuf>,
    #[arg(skip)]
    pub(super) loaded_batch: Option<AutomationBatch>,
    #[arg(skip)]
    pub(super) control_secret: Option<String>,
}

impl LaunchOptions {
    /// Loads `--genome-config` or `--genome` and derives the reference and ideogram from it.
    fn load_launch_genome(&mut self) -> Result<()> {
        let configuration = if let Some(path) = &self.genome_config {
            Some(GenomeConfiguration::read(path).with_context(|| {
                format!("failed to read genome configuration {}", path.display())
            })?)
        } else if let Some(genome) = &self.genome {
            let import = crate::workspace::genomes::load_genome_blocking(genome)?;
            for notice in &import.notices {
                tracing::warn!(%genome, %notice, "genome description notice");
            }
            Some(import.configuration)
        } else {
            None
        };
        self.loaded_genome = if let Some(configuration) = configuration {
            self.reference = Some(source_path(&configuration.reference));
            if self.remote_reference_index_url.is_none() {
                self.remote_reference_index_url
                    .clone_from(&configuration.reference_index);
            }
            self.genome_name.clone_from(&configuration.name);
            self.cytobands = match &configuration.cytobands {
                Some(TrackSource::Local { path }) => Some(PathBuf::from(path)),
                Some(TrackSource::Remote { .. }) => {
                    anyhow::bail!("remote cytoband files are not yet supported")
                }
                None => None,
            };
            Some(configuration)
        } else {
            None
        };
        Ok(())
    }

    pub(super) fn load_session(&mut self) -> Result<()> {
        let settings_path = self
            .settings_path
            .clone()
            .unwrap_or_else(default_settings_path);
        self.settings_path = Some(settings_path.clone());
        self.loaded_batch = self
            .batch
            .as_ref()
            .map(AutomationBatch::read)
            .transpose()
            .context("failed to read automation batch")?;
        self.load_control_configuration()?;
        // A missing file is a first run; an unreadable one is preserved
        // beside itself and reported once the window exists (APP-11).
        let (settings, recovery) = DesktopSettings::read_or_recover(&settings_path);
        if let Some(recovery) = &recovery {
            tracing::warn!(
                path = %settings_path.display(),
                error = %recovery.error,
                "settings file is unreadable; using defaults"
            );
        }
        self.loaded_settings = settings;
        self.loaded_settings_recovery = recovery;
        // The landmark folder setting is one of the catalogue locations.
        self.load_landmark_catalogue();
        self.load_launch_genome()?;
        let mut document = if let Some(path) = &self.session {
            Some(
                SessionDocument::read(path)
                    .with_context(|| format!("failed to read RGV session {}", path.display()))?,
            )
        } else if let Some(path) = &self.import_igv {
            let xml = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read IGV session {}", path.display()))?;
            let imported = SessionDocument::import_igv_with_report(&xml)
                .with_context(|| format!("failed to import IGV session {}", path.display()))?;
            self.loaded_igv_locus = imported_locus_request(&imported.locus_resolution);
            let mut document = imported.document;
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            document.resolve_project_paths(std::path::absolute(parent)?);
            let mut report = imported.report;
            crate::workspace::genomes::resolve_imported_session_genome(&mut document, &mut report);
            if document.remote_reference_index_url.is_some() {
                self.remote_reference_index_url
                    .clone_from(&document.remote_reference_index_url);
            }
            self.loaded_igv_report = Some(report);
            Some(document)
        } else {
            None
        };
        if let Some(document) = &mut document {
            if let Some(error) = Self::rebind_builtin_session_tracks(
                document,
                self.landmark_resource_root.as_deref(),
                self.loaded_landmark_catalogue.as_ref(),
            ) {
                self.landmark_catalogue_error = Some(error);
            }
        }
        if let Some(document) = &document {
            self.bam = primary_alignment_track(document).map(|track| source_path(&track.source));
            self.reference = document.reference.as_ref().map(source_path);
            if let Some(name) = &document.genome_name {
                self.genome_name.clone_from(name);
            }
            self.cytobands = match &document.cytobands {
                Some(TrackSource::Local { path }) => Some(PathBuf::from(path)),
                Some(TrackSource::Remote { .. }) => {
                    anyhow::bail!("remote session cytoband files are not yet supported")
                }
                None => None,
            };
            // An imported placeholder is not a locus: leave it unset so the
            // default opens until the request can be resolved.
            self.locus = self.loaded_igv_locus.is_none().then(|| {
                format!(
                    "{}:{}-{}",
                    document.locus.contig,
                    document.locus.start + 1,
                    document.locus.end
                )
            });
            self.max_alignments = primary_alignment_track(document)
                .map_or(document.maximum_alignments, |track| {
                    track.maximum_alignments
                })
                .min(document.maximum_alignments);
        }
        self.loaded_session = document;
        Ok(())
    }

    fn load_landmark_catalogue(&mut self) {
        let candidates = landmark_root_candidates(
            self.landmark_resource_root.as_deref(),
            std::env::var_os("RGV_LANDMARK_RESOURCE_ROOT"),
            self.loaded_settings.landmark_resource_root.as_deref(),
            default_landmark_user_directory(),
            std::env::current_exe()
                .ok()
                .as_deref()
                .and_then(Path::parent),
        );
        let discovery = rgv_core::discover_landmark_root(candidates);
        let problem = discovery.configuration_problem();
        let Some(found) = discovery.root else {
            self.landmark_resource_root = None;
            self.loaded_landmark_catalogue = None;
            self.landmark_resource_source = None;
            // Finding nothing at the default locations means landmarks are
            // not configured, which is not a failure; a location the user
            // named without a catalogue is.
            self.landmark_catalogue_error = problem.map(|problem| {
                format!(
                    "Landmark folder unusable: {problem}; automatic Genes tracks are unavailable"
                )
            });
            if let Some(error) = &self.landmark_catalogue_error {
                tracing::warn!(%error);
            } else {
                tracing::info!("no built-in landmark catalogue is configured");
            }
            return;
        };
        if let Some(problem) = problem {
            tracing::warn!(%problem, "skipping an unusable landmark folder");
        }
        let (root, catalogue, error) = read_landmark_catalogue(&found.path);
        self.landmark_resource_root = Some(root);
        self.landmark_resource_source = Some(found.source);
        self.loaded_landmark_catalogue = catalogue;
        self.landmark_catalogue_error = error;
    }

    pub(super) fn rebind_builtin_session_tracks(
        document: &mut SessionDocument,
        root: Option<&Path>,
        catalogue: Option<&GenomeLandmarkCatalogue>,
    ) -> Option<String> {
        let context = document.landmark_context.clone().or_else(|| {
            document.tracks.iter().find_map(|track| {
                track
                    .built_in
                    .as_ref()
                    .filter(|identity| {
                        identity.asset_role == LandmarkAssetRole::RepresentativeGenes
                    })
                    .map(|identity| rgv_core::BuiltInLandmarkContext {
                        bundle_id: identity.bundle_id.clone(),
                        bundle_version: identity.bundle_version.clone(),
                    })
            })
        });
        let built_in_bundles = document
            .tracks
            .iter()
            .filter_map(|track| track.built_in.as_ref())
            .map(|identity| identity.bundle_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let (Some(root), Some(catalogue)) = (root, catalogue) else {
            if !built_in_bundles.is_empty() || context.is_some() {
                document
                    .suppressed_builtin_gene_bundles
                    .extend(built_in_bundles);
                if let Some(context) = context {
                    document
                        .suppressed_builtin_gene_bundles
                        .insert(context.bundle_id);
                }
                document.tracks.retain(|track| track.built_in.is_none());
                document.landmark_context = None;
                document.cytobands = None;
                document.contig_aliases.clear();
                document.normalize_presentation();
                return Some(
                    "Saved built-in landmark tracks could not be rebound because their catalogue is unavailable; they remain suppressed until restored"
                        .to_owned(),
                );
            }
            return None;
        };

        let context_error = context.as_ref().and_then(|context| {
            let bundle = catalogue.bundle(&context.bundle_id)?;
            (bundle.version != context.bundle_version).then(|| {
                format!(
                    "Saved landmark context {} {} is not installed",
                    context.bundle_id, context.bundle_version
                )
            })
        });
        let context_error = context_error.or_else(|| {
            context.as_ref().and_then(|context| {
                (catalogue.bundle(&context.bundle_id).is_none()).then(|| {
                    format!(
                        "Saved landmark context {} is not installed",
                        context.bundle_id
                    )
                })
            })
        });
        let rebound_context = if let (Some(context), None) = (&context, &context_error) {
            (|| -> Result<_> {
                let aliases_path = catalogue.verify_asset(
                    root,
                    &context.bundle_id,
                    LandmarkAssetRole::ContigAliases,
                )?;
                let cytobands_path = catalogue.verify_asset(
                    root,
                    &context.bundle_id,
                    LandmarkAssetRole::Cytobands,
                )?;
                let aliases = std::fs::read_to_string(&aliases_path)
                    .with_context(|| format!("failed to read {}", aliases_path.display()))?;
                Ok((rgv_core::parse_landmark_aliases(&aliases)?, cytobands_path))
            })()
            .map_err(|error| {
                format!(
                    "Saved landmark context {} failed integrity verification: {error}",
                    context.bundle_id
                )
            })
        } else if let Some(error) = context_error {
            Err(error)
        } else {
            Ok((document.contig_aliases.clone(), PathBuf::new()))
        };

        let mut track_errors = Vec::new();
        let mut newly_suppressed = std::collections::BTreeSet::new();
        document.tracks.retain_mut(|track| {
            let Some(identity) = &track.built_in else {
                return true;
            };
            if context.as_ref().is_some_and(|context| {
                context.bundle_id == identity.bundle_id && rebound_context.is_err()
            }) {
                newly_suppressed.insert(identity.bundle_id.clone());
                return false;
            }
            let rebound = (|| -> Result<PathBuf> {
                let bundle = catalogue
                    .bundle(&identity.bundle_id)
                    .with_context(|| format!("bundle {} is not installed", identity.bundle_id))?;
                let asset = bundle.asset(identity.asset_role).with_context(|| {
                    format!(
                        "asset {:?} is not installed for {}",
                        identity.asset_role, identity.bundle_id
                    )
                })?;
                anyhow::ensure!(
                    bundle.version == identity.bundle_version
                        && asset.sha256.eq_ignore_ascii_case(&identity.sha256),
                    "version/checksum is not installed for {}",
                    identity.bundle_id
                );
                Ok(catalogue.verify_asset(root, &identity.bundle_id, identity.asset_role)?)
            })();
            match rebound {
                Ok(path) => {
                    track.source = track_source(&path);
                    true
                }
                Err(error) => {
                    newly_suppressed.insert(identity.bundle_id.clone());
                    track_errors.push(format!(
                        "Saved landmark asset {} could not be rebound: {error}",
                        identity.bundle_id
                    ));
                    false
                }
            }
        });
        document
            .suppressed_builtin_gene_bundles
            .extend(newly_suppressed);

        match (context, rebound_context) {
            (Some(context), Ok((aliases, cytobands_path))) => {
                document.contig_aliases = aliases;
                document.cytobands = Some(track_source(&cytobands_path));
                document.landmark_context = Some(context);
            }
            (Some(context), Err(error)) => {
                document
                    .suppressed_builtin_gene_bundles
                    .insert(context.bundle_id);
                document.landmark_context = None;
                document.cytobands = None;
                document.contig_aliases.clear();
                track_errors.push(error);
            }
            (None, _) => {}
        }
        document.normalize_presentation();
        if track_errors.is_empty() {
            None
        } else {
            Some(format!(
                "{}; built-in genes remain suppressed until restored",
                track_errors.join("; ")
            ))
        }
    }

    pub(super) fn load_control_configuration(&mut self) -> Result<()> {
        if self.control_port.is_none() {
            return Ok(());
        }
        let secret_path = self
            .control_secret_file
            .as_deref()
            .context("--control-port requires --control-secret-file")?;
        self.control_secret = Some(
            read_control_secret(secret_path)
                .map_err(anyhow::Error::msg)
                .context("invalid local-control secret")?,
        );
        let root = self
            .control_root
            .clone()
            .unwrap_or(std::env::current_dir().context("could not resolve the current directory")?);
        self.control_root = Some(
            root.canonicalize()
                .with_context(|| format!("could not resolve control root {}", root.display()))?,
        );
        Ok(())
    }
}

/// Reads and validates `catalog.json` in a discovered catalogue directory.
/// Returns the directory, the catalogue when valid, and the error otherwise.
pub(crate) fn read_landmark_catalogue(
    root: &Path,
) -> (PathBuf, Option<GenomeLandmarkCatalogue>, Option<String>) {
    let catalogue_path = root.join("catalog.json");
    match GenomeLandmarkCatalogue::read(&catalogue_path) {
        Ok(catalogue) => {
            tracing::info!(
                path = %catalogue_path.display(),
                bundles = catalogue.bundles.len(),
                "loaded built-in landmark catalogue"
            );
            (root.to_path_buf(), Some(catalogue), None)
        }
        Err(error) => {
            let message = format!(
                "Built-in landmark catalogue {} is invalid: {error}",
                catalogue_path.display()
            );
            tracing::warn!(%message);
            (root.to_path_buf(), None, Some(message))
        }
    }
}

/// Landmark catalogue locations in discovery order: `--landmark-resource-root` alone when given;
/// otherwise `RGV_LANDMARK_RESOURCE_ROOT`, the landmark folder setting, the platform user data
/// directory, then the development and packaged locations relative to the working directory and
/// the executable.
pub(super) fn landmark_root_candidates(
    explicit: Option<&Path>,
    environment: Option<std::ffi::OsString>,
    setting: Option<&Path>,
    user_data: Option<PathBuf>,
    executable_directory: Option<&Path>,
) -> Vec<rgv_core::LandmarkRootCandidate> {
    use rgv_core::{LandmarkRootCandidate, LandmarkRootSource};
    let candidate = |source, path: PathBuf| LandmarkRootCandidate { source, path };
    if let Some(path) = explicit {
        return vec![candidate(
            LandmarkRootSource::CommandLine,
            path.to_path_buf(),
        )];
    }
    let mut candidates = Vec::new();
    if let Some(path) = environment.filter(|value| !value.is_empty()) {
        candidates.push(candidate(LandmarkRootSource::Environment, path.into()));
    }
    if let Some(path) = setting.filter(|path| !path.as_os_str().is_empty()) {
        candidates.push(candidate(LandmarkRootSource::Setting, path.to_path_buf()));
    }
    if let Some(path) = user_data {
        candidates.push(candidate(LandmarkRootSource::UserData, path));
    }
    candidates.extend(
        [
            PathBuf::from("benchmark-data/large-public-alignments/landmarks/igv"),
            PathBuf::from("resources/landmarks"),
        ]
        .into_iter()
        .map(|path| candidate(LandmarkRootSource::Bundled, path)),
    );
    if let Some(directory) = executable_directory {
        candidates.extend(
            [
                directory.join("landmarks"),
                directory.join("resources/landmarks"),
                directory.join("../Resources/landmarks"),
            ]
            .into_iter()
            .map(|path| candidate(LandmarkRootSource::Bundled, path)),
        );
    }
    candidates
}

/// The per-user landmark folder: `landmarks` in the platform user data directory
/// (`~/Library/Application Support/RGV` on macOS, `%APPDATA%\RGV` on Windows,
/// `$XDG_DATA_HOME/rgv` or `~/.local/share/rgv` elsewhere).
pub(super) fn default_landmark_user_directory() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(|directory| PathBuf::from(directory).join("RGV").join("landmarks"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|directory| {
            PathBuf::from(directory)
                .join("Library")
                .join("Application Support")
                .join("RGV")
                .join("landmarks")
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(|directory| PathBuf::from(directory).join("rgv").join("landmarks"))
            .or_else(|| {
                std::env::var_os("HOME").map(|directory| {
                    PathBuf::from(directory)
                        .join(".local")
                        .join("share")
                        .join("rgv")
                        .join("landmarks")
                })
            })
    }
}

pub(super) fn default_settings_path() -> PathBuf {
    if let Some(directory) = std::env::var_os("RGV_CONFIG_DIR") {
        return PathBuf::from(directory).join("settings.json");
    }
    #[cfg(target_os = "windows")]
    if let Some(directory) = std::env::var_os("APPDATA") {
        return PathBuf::from(directory).join("RGV").join("settings.json");
    }
    #[cfg(target_os = "macos")]
    if let Some(directory) = std::env::var_os("HOME") {
        return PathBuf::from(directory)
            .join("Library")
            .join("Application Support")
            .join("RGV")
            .join("settings.json");
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(directory) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(directory).join("rgv").join("settings.json");
        }
        if let Some(directory) = std::env::var_os("HOME") {
            return PathBuf::from(directory)
                .join(".config")
                .join("rgv")
                .join("settings.json");
        }
    }
    PathBuf::from("rgv-settings.json")
}

pub(super) const MAX_RECOVERY_CANDIDATES: usize = 100;

pub(super) fn autosave_directory(settings_path: &Path) -> PathBuf {
    settings_path.with_file_name("autosaves")
}

pub(super) fn stable_path_hash(path: Option<&Path>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let value = path.map_or_else(|| "untitled".into(), |path| path.to_string_lossy());
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub(super) fn autosave_group_prefix(project_path: Option<&Path>) -> String {
    let label = project_path
        .and_then(Path::file_stem)
        .and_then(|value| value.to_str())
        .unwrap_or("untitled");
    let mut safe = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(32)
        .collect::<String>();
    if safe.is_empty() {
        safe.push_str("project");
    }
    format!("{safe}-{:016x}", stable_path_hash(project_path))
}

/// Autosave group for one document in one running instance.
///
/// A saved project is identified by its path. Untitled projects have no path, so every running
/// instance would otherwise share one group and rotate out each other's snapshots; they are
/// scoped by the application instance instead.
pub(super) fn autosave_instance_prefix(project_path: Option<&Path>, instance_id: &str) -> String {
    let group = autosave_group_prefix(project_path);
    if project_path.is_some() {
        return group;
    }
    let instance = instance_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(16)
        .collect::<String>();
    if instance.is_empty() {
        group
    } else {
        format!("{group}-{instance}")
    }
}

pub(super) fn autosave_snapshot_path(
    settings_path: &Path,
    project_path: Option<&Path>,
    instance_id: &str,
    now: SystemTime,
) -> (PathBuf, String) {
    let prefix = autosave_instance_prefix(project_path, instance_id);
    let milliseconds = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let name = format!("{prefix}-{milliseconds:020}.autosave.json");
    (autosave_directory(settings_path).join(name), prefix)
}

fn autosave_timestamp(path: &Path) -> Option<&str> {
    path.file_name()?
        .to_str()?
        .strip_suffix(".autosave.json")?
        .rsplit('-')
        .next()
}

/// Keeps the newest `retention` snapshots of this group, then bounds the whole directory:
/// instance-scoped groups of earlier runs are never pruned by their own instance again, so the
/// oldest snapshots of other groups are removed beyond [`MAX_RECOVERY_CANDIDATES`].
pub(super) fn prune_autosaves(
    directory: &Path,
    prefix: &str,
    retention: usize,
) -> std::io::Result<()> {
    let file_prefix = format!("{prefix}-");
    let (mut matching, mut others): (Vec<_>, Vec<_>) = std::fs::read_dir(directory)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.ends_with(".autosave.json"))
        })
        .partition(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.starts_with(&file_prefix))
        });
    matching.sort_unstable();
    let remove_count = matching.len().saturating_sub(retention);
    for path in matching.into_iter().take(remove_count) {
        std::fs::remove_file(path)?;
    }
    // Oldest first by the zero-padded timestamp, which sorts lexicographically.
    others.sort_unstable_by(|left, right| {
        autosave_timestamp(left)
            .cmp(&autosave_timestamp(right))
            .then_with(|| left.cmp(right))
    });
    let remove_count = others.len().saturating_sub(MAX_RECOVERY_CANDIDATES);
    for path in others.into_iter().take(remove_count) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub(super) fn discover_recovery_candidates(settings_path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(autosave_directory(settings_path)) else {
        return Vec::new();
    };
    let mut candidates = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.ends_with(".autosave.json"))
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|left, right| right.cmp(left));
    candidates.truncate(MAX_RECOVERY_CANDIDATES);
    candidates
}

/// Whether a process with this id is currently running. An undeterminable answer counts as
/// not running, so a recovery prompt is offered rather than suppressed.
pub(super) fn process_is_alive(pid: u32) -> bool {
    use std::process::{Command, Stdio};
    if cfg!(unix) {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    } else if cfg!(windows) {
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .is_ok_and(|output| {
                String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
            })
    } else {
        false
    }
}

/// Process id recorded in a run marker file name or, for the legacy shared marker, its body.
fn run_marker_pid(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    if let Some(pid) = name
        .strip_prefix("rgv-running.")
        .and_then(|rest| rest.strip_suffix(".marker"))
        && let Ok(pid) = pid.parse()
    {
        return Some(pid);
    }
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("pid="))?
        .trim()
        .parse()
        .ok()
}

/// One marker per running process (`rgv-running.<pid>.marker`). A marker whose process is no
/// longer alive is evidence of an unclean exit; a marker of a live process is simply another
/// instance and is left alone.
pub(super) struct RunMarker {
    path: Option<PathBuf>,
    previous_unclean_exit: bool,
}

impl RunMarker {
    pub(super) fn start(settings_path: &Path) -> Self {
        Self::start_with(settings_path, std::process::id(), process_is_alive)
    }

    pub(super) fn start_with(
        settings_path: &Path,
        own_pid: u32,
        is_alive: impl Fn(u32) -> bool,
    ) -> Self {
        let marker_path = settings_path.with_file_name(format!("rgv-running.{own_pid}.marker"));
        let directory = marker_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let mut previous_unclean_exit = false;
        if let Ok(entries) = std::fs::read_dir(&directory) {
            for path in entries
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path())
            {
                let is_marker =
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("rgv-running") && name.ends_with(".marker")
                        });
                if !is_marker {
                    continue;
                }
                // Our own pid cannot belong to an earlier live instance: the marker is stale.
                let stale =
                    run_marker_pid(&path).is_none_or(|pid| pid == own_pid || !is_alive(pid));
                if stale {
                    previous_unclean_exit = true;
                    if let Err(error) = std::fs::remove_file(&path) {
                        tracing::warn!(%error, "could not remove stale run marker");
                    }
                }
            }
        }
        let parent_ready = match std::fs::create_dir_all(&directory) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "could not create run-marker directory");
                false
            }
        };
        let path = parent_ready
            .then(|| {
                let marker = format!("pid={own_pid}\n");
                match std::fs::write(&marker_path, marker) {
                    Ok(()) => Some(marker_path.clone()),
                    Err(error) => {
                        tracing::warn!(%error, "could not create unclean-exit marker");
                        None
                    }
                }
            })
            .flatten();
        Self {
            path,
            previous_unclean_exit,
        }
    }

    pub(super) const fn previous_unclean_exit(&self) -> bool {
        self.previous_unclean_exit
    }

    pub(super) fn finish(&mut self) {
        if let Some(path) = self.path.take()
            && let Err(error) = std::fs::remove_file(path)
        {
            tracing::warn!(%error, "could not remove clean-exit marker");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::{SessionLocus, TrackConfig, TrackId, TrackKind};
    use std::time::Duration;

    #[test]
    fn run_markers_are_per_process_and_only_dead_processes_mean_an_unclean_exit() {
        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("settings.json");
        let alive = |pid: u32| pid == 100;

        let mut first = RunMarker::start_with(&settings, 100, alive);
        assert!(!first.previous_unclean_exit());
        assert!(directory.path().join("rgv-running.100.marker").exists());

        // A second instance starts while the first is still running: not a crash, and the
        // first instance's marker is untouched.
        let mut second = RunMarker::start_with(&settings, 200, alive);
        assert!(!second.previous_unclean_exit());
        assert!(directory.path().join("rgv-running.100.marker").exists());

        // The second exits cleanly without clearing the first instance's marker.
        second.finish();
        assert!(directory.path().join("rgv-running.100.marker").exists());
        assert!(!directory.path().join("rgv-running.200.marker").exists());

        // Process 300 crashed earlier and left its marker; a legacy shared marker is also stale.
        std::fs::write(directory.path().join("rgv-running.300.marker"), "pid=300\n").unwrap();
        std::fs::write(directory.path().join("rgv-running.marker"), "pid=301\n").unwrap();
        let mut third = RunMarker::start_with(&settings, 400, alive);
        assert!(third.previous_unclean_exit());
        assert!(!directory.path().join("rgv-running.300.marker").exists());
        assert!(!directory.path().join("rgv-running.marker").exists());
        assert!(directory.path().join("rgv-running.100.marker").exists());

        // The stale markers were consumed: the next start is clean.
        let mut fourth = RunMarker::start_with(&settings, 500, |pid| pid == 100 || pid == 400);
        assert!(!fourth.previous_unclean_exit());
        first.finish();
        third.finish();
        fourth.finish();
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn this_process_is_alive_and_an_unused_pid_is_not() {
        assert!(process_is_alive(std::process::id()));
        assert!(!process_is_alive(u32::MAX - 7));
    }

    #[test]
    fn untitled_autosaves_are_scoped_to_the_instance_and_the_directory_is_bounded() {
        let project = Path::new("/data/review.json");
        assert_eq!(
            autosave_instance_prefix(Some(project), "instance-a"),
            autosave_instance_prefix(Some(project), "instance-b"),
            "a saved project is one group whichever instance writes it"
        );
        let first = autosave_instance_prefix(None, "instance-a");
        let second = autosave_instance_prefix(None, "instance-b");
        assert_ne!(first, second);
        assert!(first.starts_with("untitled-"));

        let directory = tempfile::tempdir().unwrap();
        let settings = directory.path().join("settings.json");
        let autosaves = autosave_directory(&settings);
        std::fs::create_dir(&autosaves).unwrap();
        let write = |instance: &str, millisecond: u64| {
            let (path, prefix) = autosave_snapshot_path(
                &settings,
                None,
                instance,
                UNIX_EPOCH + Duration::from_millis(millisecond),
            );
            std::fs::write(&path, b"snapshot").unwrap();
            (path, prefix)
        };
        let (other_instance, _) = write("instance-b", 1);
        let mut prefix = String::new();
        for millisecond in 2..=5 {
            prefix = write("instance-a", millisecond).1;
        }
        prune_autosaves(&autosaves, &prefix, 2).unwrap();
        assert!(
            other_instance.exists(),
            "one instance's retention never rotates out another instance's snapshot"
        );
        assert_eq!(std::fs::read_dir(&autosaves).unwrap().count(), 3);

        // Groups left by earlier runs are bounded as a whole, oldest first.
        for index in 0..MAX_RECOVERY_CANDIDATES as u64 + 5 {
            write(&format!("old{index}"), 1_000 + index);
        }
        prune_autosaves(&autosaves, &prefix, 2).unwrap();
        assert_eq!(
            std::fs::read_dir(&autosaves).unwrap().count(),
            MAX_RECOVERY_CANDIDATES + 2
        );
        assert!(
            !other_instance.exists(),
            "the oldest foreign snapshot goes first"
        );
    }

    #[test]
    fn manual_diagnostic_trace_launch_is_explicit_and_selects_its_output_directory() {
        let default = LaunchOptions::parse_from(["rgv"]);
        assert!(!default.start_diagnostic_trace);
        assert!(default.diagnostic_trace_dir.is_none());

        let enabled = LaunchOptions::parse_from([
            "rgv",
            "--start-diagnostic-trace",
            "--diagnostic-trace-dir",
            "diagnostic-traces",
        ]);
        assert!(enabled.start_diagnostic_trace);
        assert_eq!(
            enabled.diagnostic_trace_dir,
            Some(PathBuf::from("diagnostic-traces"))
        );
    }

    #[test]
    fn landmark_locations_are_consulted_in_the_documented_order() {
        use rgv_core::LandmarkRootSource;
        let sources = |candidates: &[rgv_core::LandmarkRootCandidate]| {
            candidates
                .iter()
                .map(|candidate| candidate.source)
                .collect::<Vec<_>>()
        };
        let all = landmark_root_candidates(
            None,
            Some("/env/landmarks".into()),
            Some(Path::new("/setting/landmarks")),
            Some(PathBuf::from("/user/landmarks")),
            Some(Path::new("/app/bin")),
        );
        assert_eq!(
            sources(&all)[..4],
            [
                LandmarkRootSource::Environment,
                LandmarkRootSource::Setting,
                LandmarkRootSource::UserData,
                LandmarkRootSource::Bundled,
            ]
        );
        assert_eq!(all[0].path, PathBuf::from("/env/landmarks"));
        assert_eq!(all[1].path, PathBuf::from("/setting/landmarks"));
        assert_eq!(all[2].path, PathBuf::from("/user/landmarks"));
        assert_eq!(
            all[3].path,
            PathBuf::from("benchmark-data/large-public-alignments/landmarks/igv")
        );
        assert_eq!(
            all.last().unwrap().path,
            Path::new("/app/bin/../Resources/landmarks")
        );

        // The command line is authoritative; empty values are not locations.
        let explicit = landmark_root_candidates(
            Some(Path::new("/cli")),
            Some("/env".into()),
            Some(Path::new("/setting")),
            None,
            None,
        );
        assert_eq!(sources(&explicit), [LandmarkRootSource::CommandLine]);
        let unset =
            landmark_root_candidates(None, Some("".into()), Some(Path::new("")), None, None);
        assert!(
            sources(&unset)
                .iter()
                .all(|source| *source == LandmarkRootSource::Bundled)
        );
    }

    #[test]
    fn an_unusable_or_invalid_landmark_folder_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let mut options = LaunchOptions::parse_from(["rgv"]);
        // A folder without a catalogue named on the command line is a
        // configuration error, reported as such.
        options.landmark_resource_root = Some(temp.path().to_path_buf());
        options.load_landmark_catalogue();
        assert!(options.landmark_resource_root.is_none());
        assert!(
            options
                .landmark_catalogue_error
                .as_deref()
                .is_some_and(|error| error.contains("contains no catalog.json"))
        );

        // An invalid catalogue is a failure that names the file.
        std::fs::create_dir(temp.path().join("igv")).unwrap();
        std::fs::write(temp.path().join("igv/catalog.json"), b"{}").unwrap();
        options.landmark_resource_root = Some(temp.path().to_path_buf());
        options.load_landmark_catalogue();
        assert_eq!(
            options.landmark_resource_root.as_deref(),
            Some(temp.path().join("igv").as_path())
        );
        assert_eq!(
            options.landmark_resource_source,
            Some(rgv_core::LandmarkRootSource::CommandLine)
        );
        assert!(options.loaded_landmark_catalogue.is_none());
        assert!(
            options
                .landmark_catalogue_error
                .as_deref()
                .is_some_and(|error| error.contains("is invalid"))
        );
    }

    #[test]
    fn unavailable_catalogue_never_trusts_a_saved_builtin_path() {
        let mut document = SessionDocument::new(SessionLocus {
            contig: "chr1".to_owned(),
            start: 0,
            end: 100,
        });
        document.landmark_context = Some(rgv_core::BuiltInLandmarkContext {
            bundle_id: "hg38".to_owned(),
            bundle_version: "1.0-rgv1".to_owned(),
        });
        document.tracks.push(
            TrackConfig::new(
                TrackId::new(7),
                "Genes".to_owned(),
                TrackKind::GeneModel,
                TrackSource::Local {
                    path: "/untrusted/developer/path/genes.genePred.gz".to_owned(),
                },
            )
            .with_built_in(rgv_core::BuiltInTrackIdentity {
                bundle_id: "hg38".to_owned(),
                bundle_version: "1.0-rgv1".to_owned(),
                asset_role: LandmarkAssetRole::RepresentativeGenes,
                sha256: "a".repeat(64),
            }),
        );
        document.normalize_presentation();
        let mut options = LaunchOptions::parse_from(["rgv"]);
        options.landmark_resource_root = None;
        options.loaded_landmark_catalogue = None;

        options.landmark_catalogue_error = LaunchOptions::rebind_builtin_session_tracks(
            &mut document,
            options.landmark_resource_root.as_deref(),
            options.loaded_landmark_catalogue.as_ref(),
        );

        assert!(document.tracks.is_empty());
        assert!(document.landmark_context.is_none());
        assert!(document.suppressed_builtin_gene_bundles.contains("hg38"));
        assert!(options.landmark_catalogue_error.is_some());
        assert!(document.presentation.pane_tracks.is_empty());
    }
}
