use super::*;

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
    /// Portable JSON genome bundle describing FASTA, aliases, cytobands, and default annotations.
    #[arg(long, conflicts_with_all = ["session", "import_igv"])]
    pub(super) genome_config: Option<PathBuf>,
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
    #[arg(skip)]
    pub(super) loaded_session: Option<SessionDocument>,
    #[arg(skip)]
    pub(super) loaded_genome: Option<GenomeConfiguration>,
    #[arg(skip)]
    pub(super) loaded_settings: DesktopSettings,
    #[arg(skip)]
    pub(super) loaded_igv_report: Option<IgvImportReport>,
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
        self.loaded_settings = if settings_path.exists() {
            match DesktopSettings::read(&settings_path) {
                Ok(settings) => settings,
                Err(error) => {
                    tracing::warn!(path = %settings_path.display(), %error, "ignoring invalid settings file");
                    DesktopSettings::default()
                }
            }
        } else {
            DesktopSettings::default()
        };
        self.loaded_genome = if let Some(path) = &self.genome_config {
            let configuration = GenomeConfiguration::read(path).with_context(|| {
                format!("failed to read genome configuration {}", path.display())
            })?;
            self.reference = Some(source_path(&configuration.reference));
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
        let document = if let Some(path) = &self.session {
            Some(
                SessionDocument::read(path)
                    .with_context(|| format!("failed to read RGV session {}", path.display()))?,
            )
        } else if let Some(path) = &self.import_igv {
            let xml = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read IGV session {}", path.display()))?;
            let imported = SessionDocument::import_igv_with_report(&xml)
                .with_context(|| format!("failed to import IGV session {}", path.display()))?;
            let mut document = imported.document;
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            document.resolve_project_paths(std::path::absolute(parent)?);
            self.loaded_igv_report = Some(imported.report);
            Some(document)
        } else {
            None
        };
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
            self.locus = Some(format!(
                "{}:{}-{}",
                document.locus.contig,
                document.locus.start + 1,
                document.locus.end
            ));
            self.max_alignments = primary_alignment_track(document)
                .map_or(document.maximum_alignments, |track| {
                    track.maximum_alignments
                })
                .min(document.maximum_alignments);
        }
        self.loaded_session = document;
        Ok(())
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

pub(super) fn autosave_snapshot_path(
    settings_path: &Path,
    project_path: Option<&Path>,
    now: SystemTime,
) -> (PathBuf, String) {
    let prefix = autosave_group_prefix(project_path);
    let milliseconds = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let name = format!("{prefix}-{milliseconds:020}.autosave.json");
    (autosave_directory(settings_path).join(name), prefix)
}

pub(super) fn prune_autosaves(
    directory: &Path,
    prefix: &str,
    retention: usize,
) -> std::io::Result<()> {
    let file_prefix = format!("{prefix}-");
    let mut matching = std::fs::read_dir(directory)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    name.starts_with(&file_prefix) && name.ends_with(".autosave.json")
                })
        })
        .collect::<Vec<_>>();
    matching.sort_unstable();
    let remove_count = matching.len().saturating_sub(retention);
    for path in matching.into_iter().take(remove_count) {
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

pub(super) struct RunMarker {
    path: Option<PathBuf>,
    previous_unclean_exit: bool,
}

impl RunMarker {
    pub(super) fn start(settings_path: &Path) -> Self {
        let marker_path = settings_path.with_file_name("rgv-running.marker");
        let previous_unclean_exit = marker_path.exists();
        let parent_ready = marker_path.parent().is_none_or(|parent| {
            if parent.as_os_str().is_empty() {
                return true;
            }
            if let Err(error) = std::fs::create_dir_all(parent) {
                tracing::warn!(%error, "could not create run-marker directory");
                return false;
            }
            true
        });
        let path = parent_ready
            .then(|| {
                let marker = format!("pid={}\n", std::process::id());
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
