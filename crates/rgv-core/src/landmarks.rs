use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Write as _,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ContigAliases, ReferenceDictionary};

pub const LANDMARK_CATALOGUE_SCHEMA_VERSION: u32 = 1;
pub const LANDMARK_SEARCH_SCHEMA_VERSION: &str = "rgv_gene_names_v1";
pub const MAXIMUM_LANDMARK_BUNDLES: usize = 64;
pub const MAXIMUM_LANDMARK_ASSETS: usize = 32;
const MAXIMUM_LANDMARK_CONTIGS: usize = 100_000;
const MAXIMUM_LANDMARK_ALIASES: usize = 100_000;
const MAXIMUM_LANDMARK_SEARCH_ENTRIES: usize = 2_000_000;
const MAXIMUM_LANDMARK_STRING_BYTES: usize = 16_384;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandmarkAssetRole {
    RepresentativeGenes,
    DetailedTranscripts,
    GeneSearch,
    Cytobands,
    ContigAliases,
    ChromosomeSizes,
    Notice,
}

/// Portable identity for an application-provided track. The resolved local
/// path is merely a runtime cache location; this tuple is the persisted source
/// of truth used to rebind the track on another installation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuiltInTrackIdentity {
    pub bundle_id: String,
    pub bundle_version: String,
    pub asset_role: LandmarkAssetRole,
    pub sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuiltInLandmarkContext {
    pub bundle_id: String,
    pub bundle_version: String,
}

impl BuiltInLandmarkContext {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.bundle_id.trim().is_empty()
            && self.bundle_id.len() <= 256
            && !self.bundle_version.trim().is_empty()
            && self.bundle_version.len() <= 256
    }
}

impl BuiltInTrackIdentity {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.bundle_id.trim().is_empty()
            && self.bundle_id.len() <= 256
            && !self.bundle_version.trim().is_empty()
            && self.bundle_version.len() <= 256
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
}

impl LandmarkAssetRole {
    const REQUIRED: [Self; 7] = [
        Self::RepresentativeGenes,
        Self::DetailedTranscripts,
        Self::GeneSearch,
        Self::Cytobands,
        Self::ContigAliases,
        Self::ChromosomeSizes,
        Self::Notice,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandmarkAssetFormat {
    BigGenePred,
    GenePredExtGzip,
    RgvGeneNamesV1Gzip,
    UcscCytoband,
    AliasTsv,
    ChromSizesTsv,
    Markdown,
}

impl LandmarkAssetFormat {
    const fn supports(self, role: LandmarkAssetRole) -> bool {
        matches!(
            (role, self),
            (
                LandmarkAssetRole::RepresentativeGenes | LandmarkAssetRole::DetailedTranscripts,
                Self::BigGenePred | Self::GenePredExtGzip
            ) | (LandmarkAssetRole::GeneSearch, Self::RgvGeneNamesV1Gzip)
                | (LandmarkAssetRole::Cytobands, Self::UcscCytoband)
                | (LandmarkAssetRole::ContigAliases, Self::AliasTsv)
                | (LandmarkAssetRole::ChromosomeSizes, Self::ChromSizesTsv)
                | (LandmarkAssetRole::Notice, Self::Markdown)
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenomeLandmarkAsset {
    pub role: LandmarkAssetRole,
    pub path: String,
    pub format: LandmarkAssetFormat,
    pub bytes: u64,
    pub sha256: String,
    pub source_url: String,
    pub upstream_project: String,
    pub upstream_version: String,
    pub derivation: String,
    pub citation: String,
    pub license: String,
    pub license_url: String,
    #[serde(default)]
    pub required_notice: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LandmarkContigFingerprint {
    pub name: String,
    pub length: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(default = "default_true")]
    pub primary: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LandmarkAssemblyIdentity {
    pub stable_id: String,
    pub display_name: String,
    #[serde(default)]
    pub compatible_ids: Vec<String>,
    pub primary_contig_count: usize,
    pub dictionary_sha256: String,
    /// Additional exact ordered dictionaries qualified as coordinate-compatible
    /// with this assembly (for example a primary assembly plus spike-ins).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compatible_dictionary_sha256: Vec<String>,
    pub contigs: Vec<LandmarkContigFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenomeLandmarkBundle {
    pub id: String,
    pub version: String,
    pub display_name: String,
    pub provider: String,
    pub release: String,
    pub selection_policy: String,
    pub assembly: LandmarkAssemblyIdentity,
    pub assets: Vec<GenomeLandmarkAsset>,
}

impl GenomeLandmarkBundle {
    #[must_use]
    pub fn asset(&self, role: LandmarkAssetRole) -> Option<&GenomeLandmarkAsset> {
        self.assets.iter().find(|asset| asset.role == role)
    }

    #[must_use]
    pub fn stable_asset_id(&self, role: LandmarkAssetRole) -> Option<String> {
        self.asset(role)
            .map(|asset| format!("{}:{}:{role:?}:{}", self.id, self.version, asset.sha256))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenomeLandmarkCatalogue {
    pub schema_version: u32,
    pub bundles: Vec<GenomeLandmarkBundle>,
}

impl GenomeLandmarkCatalogue {
    pub fn read(path: impl AsRef<Path>) -> Result<Self, GenomeLandmarkCatalogueError> {
        let text = fs::read_to_string(path)?;
        let catalogue = serde_json::from_str::<Self>(&text)?;
        catalogue.validate()?;
        Ok(catalogue)
    }

    pub fn validate(&self) -> Result<(), GenomeLandmarkCatalogueError> {
        if self.schema_version != LANDMARK_CATALOGUE_SCHEMA_VERSION {
            return Err(GenomeLandmarkCatalogueError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        if self.bundles.is_empty() || self.bundles.len() > MAXIMUM_LANDMARK_BUNDLES {
            return Err(GenomeLandmarkCatalogueError::BundleLimit);
        }
        let mut bundle_ids = BTreeSet::new();
        let mut stable_ids = BTreeSet::new();
        let mut dictionary_hashes = BTreeSet::new();
        for bundle in &self.bundles {
            validate_short(&bundle.id)?;
            validate_short(&bundle.version)?;
            validate_short(&bundle.display_name)?;
            validate_short(&bundle.provider)?;
            validate_short(&bundle.release)?;
            validate_long(&bundle.selection_policy)?;
            if !bundle_ids.insert(bundle.id.clone()) {
                return Err(GenomeLandmarkCatalogueError::DuplicateBundle(
                    bundle.id.clone(),
                ));
            }
            validate_assembly(&bundle.assembly)?;
            if !stable_ids.insert(bundle.assembly.stable_id.to_ascii_lowercase()) {
                return Err(GenomeLandmarkCatalogueError::DuplicateAssembly(
                    bundle.assembly.stable_id.clone(),
                ));
            }
            for hash in std::iter::once(&bundle.assembly.dictionary_sha256)
                .chain(&bundle.assembly.compatible_dictionary_sha256)
            {
                validate_sha256(hash)?;
                if !dictionary_hashes.insert(hash.clone()) {
                    return Err(GenomeLandmarkCatalogueError::DuplicateAssembly(
                        hash.clone(),
                    ));
                }
            }
            if bundle.assets.is_empty() || bundle.assets.len() > MAXIMUM_LANDMARK_ASSETS {
                return Err(GenomeLandmarkCatalogueError::AssetLimit(bundle.id.clone()));
            }
            let mut roles = BTreeSet::new();
            for asset in &bundle.assets {
                validate_asset(asset)?;
                if !roles.insert(asset.role) {
                    return Err(GenomeLandmarkCatalogueError::DuplicateAssetRole {
                        bundle: bundle.id.clone(),
                        role: asset.role,
                    });
                }
            }
            for role in LandmarkAssetRole::REQUIRED {
                if !roles.contains(&role) {
                    return Err(GenomeLandmarkCatalogueError::MissingAssetRole {
                        bundle: bundle.id.clone(),
                        role,
                    });
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn bundle(&self, id: &str) -> Option<&GenomeLandmarkBundle> {
        self.bundles.iter().find(|bundle| bundle.id == id)
    }

    pub fn resolve_asset(
        &self,
        root: &Path,
        bundle_id: &str,
        role: LandmarkAssetRole,
    ) -> Result<PathBuf, GenomeLandmarkCatalogueError> {
        let bundle = self
            .bundle(bundle_id)
            .ok_or_else(|| GenomeLandmarkCatalogueError::UnknownBundle(bundle_id.to_owned()))?;
        let asset =
            bundle
                .asset(role)
                .ok_or_else(|| GenomeLandmarkCatalogueError::MissingAssetRole {
                    bundle: bundle_id.to_owned(),
                    role,
                })?;
        safe_relative_path(&asset.path)?;
        Ok(root.join(&asset.path))
    }

    pub fn verify_asset(
        &self,
        root: &Path,
        bundle_id: &str,
        role: LandmarkAssetRole,
    ) -> Result<PathBuf, GenomeLandmarkCatalogueError> {
        let bundle = self
            .bundle(bundle_id)
            .ok_or_else(|| GenomeLandmarkCatalogueError::UnknownBundle(bundle_id.to_owned()))?;
        let asset =
            bundle
                .asset(role)
                .ok_or_else(|| GenomeLandmarkCatalogueError::MissingAssetRole {
                    bundle: bundle_id.to_owned(),
                    role,
                })?;
        let path = self.resolve_asset(root, bundle_id, role)?;
        let canonical_root = fs::canonicalize(root)?;
        let canonical_path = fs::canonicalize(&path)?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err(GenomeLandmarkCatalogueError::UnsafePath(asset.path.clone()));
        }
        let metadata = fs::metadata(&canonical_path)?;
        if !metadata.is_file() || metadata.len() != asset.bytes {
            return Err(GenomeLandmarkCatalogueError::AssetSizeMismatch {
                path: asset.path.clone(),
                expected: asset.bytes,
                actual: metadata.len(),
            });
        }
        let mut file = fs::File::open(&canonical_path)?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        let actual = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if !actual.eq_ignore_ascii_case(&asset.sha256) {
            return Err(GenomeLandmarkCatalogueError::AssetHashMismatch {
                path: asset.path.clone(),
                expected: asset.sha256.clone(),
                actual,
            });
        }
        Ok(canonical_path)
    }

    #[must_use]
    pub fn resolve(&self, input: &AssemblyResolutionInput<'_>) -> AssemblyResolution {
        resolve_assembly(self, input)
    }
}

fn validate_assembly(
    assembly: &LandmarkAssemblyIdentity,
) -> Result<(), GenomeLandmarkCatalogueError> {
    validate_short(&assembly.stable_id)?;
    validate_short(&assembly.display_name)?;
    validate_sha256(&assembly.dictionary_sha256)?;
    if assembly.primary_contig_count == 0
        || assembly.contigs.is_empty()
        || assembly.contigs.len() > MAXIMUM_LANDMARK_CONTIGS
        || assembly.primary_contig_count
            != assembly
                .contigs
                .iter()
                .filter(|contig| contig.primary)
                .count()
    {
        return Err(GenomeLandmarkCatalogueError::InvalidAssembly(
            assembly.stable_id.clone(),
        ));
    }
    let mut names = BTreeSet::new();
    for compatible in &assembly.compatible_ids {
        validate_short(compatible)?;
    }
    for contig in &assembly.contigs {
        validate_short(&contig.name)?;
        if contig.length == 0 || !names.insert(contig.name.clone()) {
            return Err(GenomeLandmarkCatalogueError::InvalidAssembly(
                assembly.stable_id.clone(),
            ));
        }
        if let Some(md5) = &contig.md5
            && (md5.len() != 32 || !md5.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(GenomeLandmarkCatalogueError::InvalidAssembly(
                assembly.stable_id.clone(),
            ));
        }
    }
    Ok(())
}

fn validate_asset(asset: &GenomeLandmarkAsset) -> Result<(), GenomeLandmarkCatalogueError> {
    safe_relative_path(&asset.path)?;
    validate_sha256(&asset.sha256)?;
    if asset.bytes == 0 || !asset.format.supports(asset.role) {
        return Err(GenomeLandmarkCatalogueError::InvalidAsset(
            asset.path.clone(),
        ));
    }
    for value in [
        &asset.source_url,
        &asset.upstream_project,
        &asset.upstream_version,
        &asset.derivation,
        &asset.citation,
        &asset.license,
        &asset.license_url,
    ] {
        validate_long(value)?;
    }
    if asset.required_notice.len() > MAXIMUM_LANDMARK_STRING_BYTES {
        return Err(GenomeLandmarkCatalogueError::InvalidAsset(
            asset.path.clone(),
        ));
    }
    Ok(())
}

fn validate_short(value: &str) -> Result<(), GenomeLandmarkCatalogueError> {
    if value.trim().is_empty() || value.len() > 256 {
        return Err(GenomeLandmarkCatalogueError::InvalidString);
    }
    Ok(())
}

fn validate_long(value: &str) -> Result<(), GenomeLandmarkCatalogueError> {
    if value.trim().is_empty() || value.len() > MAXIMUM_LANDMARK_STRING_BYTES {
        return Err(GenomeLandmarkCatalogueError::InvalidString);
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), GenomeLandmarkCatalogueError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GenomeLandmarkCatalogueError::InvalidHash(value.to_owned()));
    }
    Ok(())
}

fn safe_relative_path(value: &str) -> Result<(), GenomeLandmarkCatalogueError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains(['\\', ':'])
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(GenomeLandmarkCatalogueError::UnsafePath(value.to_owned()));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum GenomeLandmarkCatalogueError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("landmark catalogue schema version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("landmark catalogue exceeds its bundle bound or is empty")]
    BundleLimit,
    #[error("landmark bundle {0} exceeds its asset bound or has no assets")]
    AssetLimit(String),
    #[error("duplicate landmark bundle ID {0}")]
    DuplicateBundle(String),
    #[error("duplicate landmark assembly claim {0}")]
    DuplicateAssembly(String),
    #[error("landmark bundle {bundle} has duplicate asset role {role:?}")]
    DuplicateAssetRole {
        bundle: String,
        role: LandmarkAssetRole,
    },
    #[error("landmark bundle {bundle} is missing required asset role {role:?}")]
    MissingAssetRole {
        bundle: String,
        role: LandmarkAssetRole,
    },
    #[error("unknown landmark bundle {0}")]
    UnknownBundle(String),
    #[error("invalid landmark assembly {0}")]
    InvalidAssembly(String),
    #[error("invalid landmark asset {0}")]
    InvalidAsset(String),
    #[error("unsafe landmark asset path {0}")]
    UnsafePath(String),
    #[error("invalid SHA-256 {0}")]
    InvalidHash(String),
    #[error("invalid or unbounded landmark catalogue string")]
    InvalidString,
    #[error("landmark asset {path} has {actual} bytes, expected {expected}")]
    AssetSizeMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    #[error("landmark asset {path} has SHA-256 {actual}, expected {expected}")]
    AssetHashMismatch {
        path: String,
        expected: String,
        actual: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssemblyResolutionInput<'a> {
    pub explicit_stable_id: Option<&'a str>,
    pub dictionary: &'a ReferenceDictionary,
    pub sequence_md5: &'a BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyResolutionStatus {
    Matched,
    Suggested,
    Ambiguous,
    Unknown,
    Incompatible,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyMatchEvidence {
    ExplicitIdentity,
    OrderedDictionarySha256,
    SequenceMd5,
    CompletePrimaryLengths,
    None,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssemblyResolution {
    pub status: AssemblyResolutionStatus,
    pub bundle_id: Option<String>,
    pub stable_id: Option<String>,
    pub display_name: Option<String>,
    pub evidence: AssemblyMatchEvidence,
    pub matched_contigs: usize,
    pub expected_contigs: usize,
    pub subset: bool,
    pub reason: String,
    #[serde(default)]
    pub rejected: Vec<String>,
    /// Dictionary sequences that are not part of the matched build (spike-ins,
    /// decoys, viral or unplaced sequences under other names). They never
    /// change the identity; zero when nothing matched.
    #[serde(default)]
    pub extra_contigs: usize,
}

impl AssemblyResolution {
    fn unknown(reason: impl Into<String>, rejected: Vec<String>) -> Self {
        Self {
            status: AssemblyResolutionStatus::Unknown,
            bundle_id: None,
            stable_id: None,
            display_name: None,
            evidence: AssemblyMatchEvidence::None,
            matched_contigs: 0,
            expected_contigs: 0,
            subset: false,
            reason: reason.into(),
            rejected,
            extra_contigs: 0,
        }
    }
}

#[must_use]
pub fn dictionary_sha256(dictionary: &ReferenceDictionary) -> String {
    let mut digest = Sha256::new();
    for contig in dictionary.iter() {
        digest.update(contig.name().as_bytes());
        digest.update(b"\t");
        digest.update(contig.length().to_string().as_bytes());
        digest.update(b"\n");
    }
    let digest = digest.finalize();
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[allow(clippy::too_many_lines)]
fn resolve_assembly(
    catalogue: &GenomeLandmarkCatalogue,
    input: &AssemblyResolutionInput<'_>,
) -> AssemblyResolution {
    let mut rejected = Vec::new();
    if let Some(explicit) = input.explicit_stable_id {
        let candidates = catalogue
            .bundles
            .iter()
            .filter(|bundle| identity_matches(&bundle.assembly, explicit))
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return AssemblyResolution::unknown(
                format!("explicit assembly identity {explicit} is not uniquely catalogued"),
                Vec::new(),
            );
        }
        let bundle = candidates[0];
        let (matched, conflicts) = compatible_contigs(bundle, input.dictionary);
        if !conflicts.is_empty() {
            return AssemblyResolution {
                status: AssemblyResolutionStatus::Incompatible,
                bundle_id: Some(bundle.id.clone()),
                stable_id: Some(bundle.assembly.stable_id.clone()),
                display_name: Some(bundle.assembly.display_name.clone()),
                evidence: AssemblyMatchEvidence::ExplicitIdentity,
                matched_contigs: matched,
                expected_contigs: bundle.assembly.primary_contig_count,
                subset: matched < bundle.assembly.primary_contig_count,
                reason: "explicit identity conflicts with the loaded sequence dictionary"
                    .to_owned(),
                rejected: conflicts,
                extra_contigs: 0,
            };
        }
        return matched_resolution(
            bundle,
            input.dictionary,
            AssemblyMatchEvidence::ExplicitIdentity,
            matched,
            "explicit assembly identity is compatible with the sequence dictionary",
        );
    }

    if !input.dictionary.is_empty() {
        let hash = dictionary_sha256(input.dictionary);
        let exact = catalogue
            .bundles
            .iter()
            .filter(|bundle| {
                bundle
                    .assembly
                    .dictionary_sha256
                    .eq_ignore_ascii_case(&hash)
                    || bundle
                        .assembly
                        .compatible_dictionary_sha256
                        .iter()
                        .any(|candidate| candidate.eq_ignore_ascii_case(&hash))
            })
            .collect::<Vec<_>>();
        if exact.len() == 1 {
            if let FingerprintMatch::Rejected(reason) =
                fingerprint_match(exact[0], input.dictionary, input.sequence_md5)
            {
                return AssemblyResolution {
                    status: AssemblyResolutionStatus::Incompatible,
                    bundle_id: Some(exact[0].id.clone()),
                    stable_id: Some(exact[0].assembly.stable_id.clone()),
                    display_name: Some(exact[0].assembly.display_name.clone()),
                    evidence: AssemblyMatchEvidence::OrderedDictionarySha256,
                    matched_contigs: input.dictionary.len(),
                    expected_contigs: exact[0].assembly.primary_contig_count,
                    subset: false,
                    reason: "sequence MD5 conflicts with an exact dictionary candidate".to_owned(),
                    rejected: vec![reason],
                    extra_contigs: 0,
                };
            }
            return matched_resolution(
                exact[0],
                input.dictionary,
                AssemblyMatchEvidence::OrderedDictionarySha256,
                input.dictionary.len(),
                "ordered sequence dictionary SHA-256 matched exactly",
            );
        }
    }

    let mut fingerprint_candidates = Vec::new();
    // Bundles whose sequence MD5s contradict the input are never offered by
    // the weaker primary-contig rule below.
    let mut md5_conflicts = BTreeSet::new();
    if !input.sequence_md5.is_empty() {
        for bundle in &catalogue.bundles {
            match fingerprint_match(bundle, input.dictionary, input.sequence_md5) {
                FingerprintMatch::Matched(count) => fingerprint_candidates.push((bundle, count)),
                FingerprintMatch::Rejected(reason) => {
                    md5_conflicts.insert(bundle.id.as_str());
                    rejected.push(format!("{}: {reason}", bundle.id));
                }
                FingerprintMatch::Insufficient => {}
            }
        }
    }
    if fingerprint_candidates.len() == 1 {
        let (bundle, count) = fingerprint_candidates[0];
        return AssemblyResolution {
            status: AssemblyResolutionStatus::Matched,
            bundle_id: Some(bundle.id.clone()),
            stable_id: Some(bundle.assembly.stable_id.clone()),
            display_name: Some(bundle.assembly.display_name.clone()),
            evidence: AssemblyMatchEvidence::SequenceMd5,
            matched_contigs: count,
            expected_contigs: bundle.assembly.primary_contig_count,
            subset: count < bundle.assembly.primary_contig_count,
            reason: format!("{count} sequence length and MD5 fingerprints matched"),
            rejected,
            extra_contigs: bind_primary_contigs(bundle, input.dictionary).extra_contigs,
        };
    }
    if fingerprint_candidates.len() > 1 {
        return AssemblyResolution {
            status: AssemblyResolutionStatus::Ambiguous,
            bundle_id: None,
            stable_id: None,
            display_name: None,
            evidence: AssemblyMatchEvidence::SequenceMd5,
            matched_contigs: 0,
            expected_contigs: 0,
            subset: true,
            reason: "sequence fingerprints match more than one landmark bundle".to_owned(),
            rejected,
            extra_contigs: 0,
        };
    }

    let mut primary_candidates = Vec::new();
    for bundle in catalogue
        .bundles
        .iter()
        .filter(|bundle| !md5_conflicts.contains(bundle.id.as_str()))
    {
        let binding = bind_primary_contigs(bundle, input.dictionary);
        match binding.refusal() {
            None => primary_candidates.push((bundle, binding)),
            // Report only near misses: a bundle of another species or build
            // would list every chromosome and explain nothing.
            Some(reason) if binding.matched_primary > 0 => {
                rejected.push(format!("{}: {reason}", bundle.id));
            }
            Some(_) => {}
        }
    }
    if primary_candidates.len() == 1 {
        let (bundle, binding) = primary_candidates.remove(0);
        let mut reason = format!(
            "inferred from primary contigs: all {} primary chromosomes match by name and length",
            binding.matched_primary
        );
        if binding.extra_contigs > 0 {
            let _ = write!(
                reason,
                "; {} extra contig{} (spike-ins, decoys or other sequences) do not change the identity",
                binding.extra_contigs,
                if binding.extra_contigs == 1 { "" } else { "s" }
            );
        }
        for note in &binding.notes {
            let _ = write!(reason, "; {note}");
        }
        return AssemblyResolution {
            status: AssemblyResolutionStatus::Matched,
            bundle_id: Some(bundle.id.clone()),
            stable_id: Some(bundle.assembly.stable_id.clone()),
            display_name: Some(bundle.assembly.display_name.clone()),
            evidence: AssemblyMatchEvidence::CompletePrimaryLengths,
            matched_contigs: binding.matched_primary,
            expected_contigs: bundle.assembly.primary_contig_count,
            subset: false,
            reason,
            rejected,
            extra_contigs: binding.extra_contigs,
        };
    }
    if primary_candidates.len() > 1 {
        return AssemblyResolution {
            status: AssemblyResolutionStatus::Ambiguous,
            bundle_id: None,
            stable_id: None,
            display_name: None,
            evidence: AssemblyMatchEvidence::CompletePrimaryLengths,
            matched_contigs: 0,
            expected_contigs: 0,
            subset: false,
            reason: format!(
                "primary-contig lengths match more than one landmark bundle ({})",
                primary_candidates
                    .iter()
                    .map(|(bundle, _)| bundle.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            rejected,
            extra_contigs: 0,
        };
    }
    AssemblyResolution::unknown(
        "no landmark bundle matches the sequence dictionary",
        rejected,
    )
}

fn identity_matches(assembly: &LandmarkAssemblyIdentity, requested: &str) -> bool {
    assembly.stable_id.eq_ignore_ascii_case(requested)
        || assembly
            .compatible_ids
            .iter()
            .any(|value| value.eq_ignore_ascii_case(requested))
}

fn compatible_contigs(
    bundle: &GenomeLandmarkBundle,
    dictionary: &ReferenceDictionary,
) -> (usize, Vec<String>) {
    let expected = bundle
        .assembly
        .contigs
        .iter()
        .map(|contig| (contig.name.as_str(), contig.length))
        .collect::<HashMap<_, _>>();
    let mut matched = 0;
    let mut conflicts = Vec::new();
    for contig in dictionary.iter() {
        if let Some(length) = expected.get(contig.name()) {
            if *length == contig.length() {
                matched += 1;
            } else {
                conflicts.push(format!(
                    "{} has length {}, expected {}",
                    contig.name(),
                    contig.length(),
                    length
                ));
            }
        }
    }
    (matched, conflicts)
}

fn matched_resolution(
    bundle: &GenomeLandmarkBundle,
    dictionary: &ReferenceDictionary,
    evidence: AssemblyMatchEvidence,
    matched: usize,
    reason: &str,
) -> AssemblyResolution {
    AssemblyResolution {
        status: AssemblyResolutionStatus::Matched,
        bundle_id: Some(bundle.id.clone()),
        stable_id: Some(bundle.assembly.stable_id.clone()),
        display_name: Some(bundle.assembly.display_name.clone()),
        evidence,
        matched_contigs: matched,
        expected_contigs: bundle.assembly.primary_contig_count,
        subset: matched < bundle.assembly.primary_contig_count,
        reason: reason.to_owned(),
        rejected: Vec::new(),
        extra_contigs: bind_primary_contigs(bundle, dictionary).extra_contigs,
    }
}

enum FingerprintMatch {
    Matched(usize),
    Rejected(String),
    Insufficient,
}

fn fingerprint_match(
    bundle: &GenomeLandmarkBundle,
    dictionary: &ReferenceDictionary,
    sequence_md5: &BTreeMap<String, String>,
) -> FingerprintMatch {
    let mut matched = 0;
    for contig in bundle
        .assembly
        .contigs
        .iter()
        .filter(|contig| contig.md5.is_some())
    {
        let Some(actual_md5) = sequence_md5.get(&contig.name) else {
            continue;
        };
        let Some(actual) = dictionary
            .id_by_name(&contig.name)
            .and_then(|id| dictionary.get(id))
        else {
            continue;
        };
        if actual.length() != contig.length
            || !actual_md5.eq_ignore_ascii_case(contig.md5.as_deref().unwrap_or_default())
        {
            return FingerprintMatch::Rejected(format!("{} length or MD5 conflicts", contig.name));
        }
        matched += 1;
    }
    if matched >= 3 {
        FingerprintMatch::Matched(matched)
    } else {
        FingerprintMatch::Insufficient
    }
}

/// How a reference dictionary lines up with one catalogued build, matching
/// each catalogued sequence under its own name or its `chr`-prefix and
/// mitochondrial spellings (`chr1`/`1`, `chrM`/`MT`).
#[derive(Debug, Default)]
struct PrimaryContigBinding {
    matched_primary: usize,
    /// Nuclear primary chromosomes absent from the dictionary.
    missing: Vec<String>,
    /// Catalogued sequences present under a matching name with another length.
    conflicts: Vec<String>,
    /// Catalogued sequences present more than once under different spellings.
    ambiguous: Vec<String>,
    /// Dictionary sequences that are not part of the catalogued build.
    extra_contigs: usize,
    notes: Vec<String>,
}

impl PrimaryContigBinding {
    fn refusal(&self) -> Option<String> {
        let mut reasons = Vec::new();
        if !self.missing.is_empty() {
            reasons.push(format!(
                "missing primary {}",
                summarize_contigs(&self.missing)
            ));
        }
        if !self.conflicts.is_empty() {
            reasons.push(format!("length mismatch {}", self.conflicts.join(", ")));
        }
        if !self.ambiguous.is_empty() {
            reasons.push(format!(
                "more than one spelling of {}",
                self.ambiguous.join(", ")
            ));
        }
        (!reasons.is_empty()).then(|| reasons.join("; "))
    }
}

fn summarize_contigs(names: &[String]) -> String {
    const SHOWN: usize = 6;
    if names.len() <= SHOWN {
        names.join(", ")
    } else {
        format!(
            "{} and {} more",
            names[..SHOWN].join(", "),
            names.len() - SHOWN
        )
    }
}

fn is_mitochondrial_name(name: &str) -> bool {
    let base = name
        .get(..3)
        .filter(|prefix| prefix.eq_ignore_ascii_case("chr"))
        .map_or(name, |_| &name[3..]);
    base.eq_ignore_ascii_case("M") || base.eq_ignore_ascii_case("MT")
}

/// Binds a dictionary to a build by its primary chromosomes. Every nuclear
/// primary chromosome must be present exactly once with exactly the catalogued
/// length. A catalogued non-primary sequence present with another length is a
/// conflict too (it names a different build). The mitochondrial genome is not
/// required, and a mitochondrial sequence of another length (for example the
/// rCRS `MT` of b37 against the hg19 `chrM`) is left unbound and reported
/// rather than refusing; the same exemption applies when its coordinates are
/// bound for the gene track. Sequences that are not catalogued at all are
/// counted as extra and never change the identity.
fn bind_primary_contigs(
    bundle: &GenomeLandmarkBundle,
    dictionary: &ReferenceDictionary,
) -> PrimaryContigBinding {
    let mut binding = PrimaryContigBinding::default();
    let mut catalogued = BTreeSet::new();
    for expected in &bundle.assembly.contigs {
        let mut candidates = crate::locus::contig_spellings(&expected.name)
            .iter()
            .filter_map(|name| dictionary.id_by_name(name))
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.dedup();
        let same_length = candidates
            .iter()
            .copied()
            .filter(|id| {
                dictionary
                    .get(*id)
                    .is_some_and(|contig| contig.length() == expected.length)
            })
            .collect::<Vec<_>>();
        let mitochondrial = is_mitochondrial_name(&expected.name);
        match same_length.as_slice() {
            [id] => {
                catalogued.insert(*id);
                if expected.primary {
                    binding.matched_primary += 1;
                }
            }
            [] if candidates.is_empty() => {
                if expected.primary && !mitochondrial {
                    binding.missing.push(expected.name.clone());
                }
            }
            [] if mitochondrial => {
                for id in &candidates {
                    if let Some(contig) = dictionary.get(*id) {
                        binding.notes.push(format!(
                            "mitochondrial {} has length {} (catalogued {} is {}), so it is not bound",
                            contig.name(),
                            contig.length(),
                            expected.name,
                            expected.length
                        ));
                    }
                }
            }
            [] => {
                for id in &candidates {
                    if let Some(contig) = dictionary.get(*id) {
                        binding.conflicts.push(format!(
                            "{} has length {}, expected {}",
                            contig.name(),
                            contig.length(),
                            expected.length
                        ));
                    }
                }
            }
            _ => binding.ambiguous.push(expected.name.clone()),
        }
    }
    binding.extra_contigs = dictionary.len().saturating_sub(catalogued.len());
    binding
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LandmarkSearchEntry {
    pub normalized_term: String,
    pub display_symbol: String,
    pub gene_id: String,
    pub transcript_id: String,
    pub contig: String,
    pub start: u64,
    pub end: u64,
    pub strand: char,
    pub synonyms: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LandmarkSearchIndex {
    pub entries: Vec<LandmarkSearchEntry>,
    by_transcript: HashMap<String, usize>,
    /// Every searchable term, ordered by its ASCII-lowercased bytes, so a
    /// prefix query is one binary search plus a walk over its own matches
    /// instead of a scan and sort of the whole index.
    sorted_terms: Vec<SearchTerm>,
    /// `entries.len()` when `sorted_terms` was built. `entries` is public, so a
    /// caller that resized it gets the exhaustive scan instead of stale hits.
    sorted_entry_count: usize,
}

/// One searchable term of one entry: its class decides the match rank.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SearchTerm {
    entry: u32,
    /// 0 symbol, 1 synonym, 2 gene id, 3 transcript id.
    class: u8,
    synonym: u32,
}

impl SearchTerm {
    fn text(self, entries: &[LandmarkSearchEntry]) -> &str {
        let entry = &entries[self.entry as usize];
        match self.class {
            0 => &entry.display_symbol,
            1 => &entry.synonyms[self.synonym as usize],
            2 => &entry.gene_id,
            _ => &entry.transcript_id,
        }
    }
}

fn lowercase_bytes(value: &str) -> impl Iterator<Item = u8> + '_ {
    value.bytes().map(|byte| byte.to_ascii_lowercase())
}

fn build_sorted_terms(entries: &[LandmarkSearchEntry]) -> Vec<SearchTerm> {
    let mut terms = Vec::with_capacity(entries.len().saturating_mul(3));
    for (index, entry) in entries.iter().enumerate() {
        let Ok(index) = u32::try_from(index) else {
            break;
        };
        let mut push = |class: u8, synonym: usize, text: &str| {
            if !text.is_empty() {
                terms.push(SearchTerm {
                    entry: index,
                    class,
                    synonym: u32::try_from(synonym).unwrap_or(u32::MAX),
                });
            }
        };
        push(0, 0, &entry.display_symbol);
        for (synonym, text) in entry.synonyms.iter().enumerate() {
            push(1, synonym, text);
        }
        push(2, 0, &entry.gene_id);
        push(3, 0, &entry.transcript_id);
    }
    terms.sort_unstable_by(|left, right| {
        lowercase_bytes(left.text(entries))
            .cmp(lowercase_bytes(right.text(entries)))
            .then_with(|| {
                (left.entry, left.class, left.synonym).cmp(&(
                    right.entry,
                    right.class,
                    right.synonym,
                ))
            })
    });
    terms
}

fn rank_search_matches(
    entries: &[LandmarkSearchEntry],
    mut matches: Vec<(u8, usize)>,
    maximum_hits: usize,
) -> Vec<&LandmarkSearchEntry> {
    matches.sort_by(|(left_rank, left_index), (right_rank, right_index)| {
        let (left, right) = (&entries[*left_index], &entries[*right_index]);
        left_rank
            .cmp(right_rank)
            .then_with(|| left.normalized_term.cmp(&right.normalized_term))
            .then_with(|| left.contig.cmp(&right.contig))
            .then_with(|| left.start.cmp(&right.start))
            .then_with(|| left.end.cmp(&right.end))
            .then_with(|| left.gene_id.cmp(&right.gene_id))
            .then_with(|| left.transcript_id.cmp(&right.transcript_id))
            .then_with(|| left_index.cmp(right_index))
    });
    matches
        .into_iter()
        .take(maximum_hits)
        .map(|(_, index)| &entries[index])
        .collect()
}

impl LandmarkSearchIndex {
    /// Returns the best `maximum_hits` entries whose symbol, synonym, gene id
    /// or transcript id starts with `query` (ASCII case-insensitive).
    #[must_use]
    pub fn search(&self, query: &str, maximum_hits: usize) -> Vec<&LandmarkSearchEntry> {
        let normalized = query.trim().to_ascii_lowercase();
        if normalized.is_empty() || maximum_hits == 0 {
            return Vec::new();
        }
        if self.sorted_entry_count != self.entries.len() {
            return self.search_by_scan(&normalized, maximum_hits);
        }
        let first = self.sorted_terms.partition_point(|term| {
            lowercase_bytes(term.text(&self.entries)).lt(normalized.bytes())
        });
        let mut best = HashMap::<u32, u8>::new();
        for term in &self.sorted_terms[first..] {
            let text = term.text(&self.entries);
            if !ascii_case_insensitive_prefix(text, &normalized) {
                break;
            }
            let rank = term.class * 2 + u8::from(text.len() != normalized.len());
            best.entry(term.entry)
                .and_modify(|current| *current = (*current).min(rank))
                .or_insert(rank);
        }
        rank_search_matches(
            &self.entries,
            best.into_iter()
                .map(|(entry, rank)| (rank, entry as usize))
                .collect(),
            maximum_hits,
        )
    }

    /// Exhaustive reference implementation of [`Self::search`].
    fn search_by_scan(&self, normalized: &str, maximum_hits: usize) -> Vec<&LandmarkSearchEntry> {
        rank_search_matches(
            &self.entries,
            self.entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    search_match_rank(entry, normalized).map(|rank| (rank, index))
                })
                .collect(),
            maximum_hits,
        )
    }

    #[must_use]
    pub fn matching_term<'a>(entry: &'a LandmarkSearchEntry, query: &str) -> Option<&'a str> {
        let normalized = query.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return None;
        }
        [entry.display_symbol.as_str()]
            .into_iter()
            .chain(entry.synonyms.iter().map(String::as_str))
            .chain([entry.gene_id.as_str(), entry.transcript_id.as_str()])
            .find(|term| term.to_ascii_lowercase().starts_with(&normalized))
    }

    #[must_use]
    pub fn feature_entry(
        &self,
        contig: &str,
        start: u64,
        end: u64,
        transcript_id: Option<&str>,
    ) -> Option<&LandmarkSearchEntry> {
        let matches = |entry: &&LandmarkSearchEntry| {
            entry.contig == contig
                && entry.start == start
                && entry.end == end
                && transcript_id.is_none_or(|id| id == entry.transcript_id)
        };
        if let Some(index) = transcript_id.and_then(|id| self.by_transcript.get(id))
            && let Some(entry) = self.entries.get(*index)
            && matches(&entry)
        {
            return Some(entry);
        }
        self.entries.iter().find(matches)
    }
}

fn search_match_rank(entry: &LandmarkSearchEntry, normalized_query: &str) -> Option<u8> {
    let match_rank = |term: &str, exact: u8, prefix: u8| {
        if term.eq_ignore_ascii_case(normalized_query) {
            Some(exact)
        } else if ascii_case_insensitive_prefix(term, normalized_query) {
            Some(prefix)
        } else {
            None
        }
    };
    match_rank(&entry.display_symbol, 0, 1)
        .or_else(|| {
            entry
                .synonyms
                .iter()
                .filter_map(|synonym| match_rank(synonym, 2, 3))
                .min()
        })
        .or_else(|| match_rank(&entry.gene_id, 4, 5))
        .or_else(|| match_rank(&entry.transcript_id, 6, 7))
}

fn ascii_case_insensitive_prefix(value: &str, normalized_prefix: &str) -> bool {
    value
        .get(..normalized_prefix.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(normalized_prefix))
}

pub fn parse_landmark_search_index(
    text: &str,
) -> Result<LandmarkSearchIndex, LandmarkSearchIndexError> {
    let mut lines = text.lines();
    if lines.next() != Some(LANDMARK_SEARCH_SCHEMA_VERSION) {
        return Err(LandmarkSearchIndexError::UnsupportedVersion);
    }
    let mut entries = Vec::new();
    for (line_index, line) in lines.enumerate() {
        if line.is_empty() {
            continue;
        }
        if entries.len() == MAXIMUM_LANDMARK_SEARCH_ENTRIES {
            return Err(LandmarkSearchIndexError::EntryLimit);
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 9 || fields.iter().any(|field| field.len() > 4_096) {
            return Err(LandmarkSearchIndexError::InvalidRow(line_index + 2));
        }
        let start = fields[5]
            .parse::<u64>()
            .map_err(|_| LandmarkSearchIndexError::InvalidRow(line_index + 2))?;
        let end = fields[6]
            .parse::<u64>()
            .map_err(|_| LandmarkSearchIndexError::InvalidRow(line_index + 2))?;
        let strand = match fields[7] {
            "+" => '+',
            "-" => '-',
            _ => return Err(LandmarkSearchIndexError::InvalidRow(line_index + 2)),
        };
        if fields[0].is_empty() || fields[1].is_empty() || fields[4].is_empty() || start >= end {
            return Err(LandmarkSearchIndexError::InvalidRow(line_index + 2));
        }
        entries.push(LandmarkSearchEntry {
            normalized_term: fields[0].to_owned(),
            display_symbol: fields[1].to_owned(),
            gene_id: fields[2].to_owned(),
            transcript_id: fields[3].to_owned(),
            contig: fields[4].to_owned(),
            start,
            end,
            strand,
            synonyms: fields[8]
                .split(',')
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        });
    }
    let by_transcript = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| !entry.transcript_id.is_empty())
        .map(|(index, entry)| (entry.transcript_id.clone(), index))
        .collect();
    let sorted_terms = build_sorted_terms(&entries);
    Ok(LandmarkSearchIndex {
        sorted_entry_count: entries.len(),
        entries,
        by_transcript,
        sorted_terms,
    })
}

pub fn parse_landmark_aliases(text: &str) -> Result<ContigAliases, LandmarkSearchIndexError> {
    let mut aliases = ContigAliases::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if aliases.len() == MAXIMUM_LANDMARK_ALIASES {
            return Err(LandmarkSearchIndexError::EntryLimit);
        }
        let mut fields = line.split('\t');
        let (Some(alias), Some(canonical), None) = (fields.next(), fields.next(), fields.next())
        else {
            return Err(LandmarkSearchIndexError::InvalidRow(line_index + 1));
        };
        if alias.is_empty()
            || canonical.is_empty()
            || aliases
                .insert(alias.to_owned(), canonical.to_owned())
                .is_some()
        {
            return Err(LandmarkSearchIndexError::InvalidRow(line_index + 1));
        }
    }
    Ok(aliases)
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LandmarkSearchIndexError {
    #[error("unsupported landmark search index version")]
    UnsupportedVersion,
    #[error("landmark index exceeds its entry bound")]
    EntryLimit,
    #[error("invalid landmark index row {0}")]
    InvalidRow(usize),
}

/// Where a landmark resource folder came from. Sources are consulted in the
/// order of the variants.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandmarkRootSource {
    /// `--landmark-resource-root`. When given, nothing else is consulted.
    CommandLine,
    /// The `RGV_LANDMARK_RESOURCE_ROOT` environment variable.
    Environment,
    /// The persisted `landmark_resource_root` desktop setting.
    Setting,
    /// The platform user data directory (for example
    /// `~/Library/Application Support/RGV/landmarks` on macOS).
    UserData,
    /// Development and packaged locations relative to the working directory
    /// or the executable.
    Bundled,
}

impl LandmarkRootSource {
    /// Whether the user named this location, so a location without a
    /// catalogue is a configuration error rather than an absent default.
    #[must_use]
    pub const fn is_user_configured(self) -> bool {
        matches!(self, Self::CommandLine | Self::Environment | Self::Setting)
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::CommandLine => "--landmark-resource-root",
            Self::Environment => "RGV_LANDMARK_RESOURCE_ROOT",
            Self::Setting => "landmark folder setting",
            Self::UserData => "user data directory",
            Self::Bundled => "bundled resources",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LandmarkRootCandidate {
    pub source: LandmarkRootSource,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LandmarkRootDiscovery {
    /// The first candidate holding a catalogue; its path is the catalogue
    /// directory itself.
    pub root: Option<LandmarkRootCandidate>,
    /// User-configured locations that hold no catalogue.
    pub unusable: Vec<LandmarkRootCandidate>,
}

impl LandmarkRootDiscovery {
    /// A configuration error worth reporting: a location the user named holds
    /// no catalogue. Finding nothing at the default locations is not an error;
    /// landmarks are then simply not configured.
    #[must_use]
    pub fn configuration_problem(&self) -> Option<String> {
        if self.unusable.is_empty() {
            return None;
        }
        Some(
            self.unusable
                .iter()
                .map(|candidate| {
                    format!(
                        "{} ({}) contains no catalog.json",
                        candidate.path.display(),
                        candidate.source.label()
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

/// The catalogue directory for a user-chosen folder: the folder itself when it
/// holds `catalog.json`, or its `igv` subfolder (the layout written by the
/// curation script under `landmarks/`).
#[must_use]
pub fn landmark_catalogue_directory(path: &Path) -> Option<PathBuf> {
    [path.to_path_buf(), path.join("igv")]
        .into_iter()
        .find(|directory| directory.join("catalog.json").is_file())
}

/// Picks the first candidate, in the given order, that holds a catalogue.
pub fn discover_landmark_root(
    candidates: impl IntoIterator<Item = LandmarkRootCandidate>,
) -> LandmarkRootDiscovery {
    let mut discovery = LandmarkRootDiscovery::default();
    for candidate in candidates {
        if let Some(directory) = landmark_catalogue_directory(&candidate.path) {
            discovery.root = Some(LandmarkRootCandidate {
                source: candidate.source,
                path: directory,
            });
            break;
        }
        if candidate.source.is_user_configured() {
            discovery.unusable.push(candidate);
        }
    }
    discovery
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(
        role: LandmarkAssetRole,
        format: LandmarkAssetFormat,
        path: &str,
    ) -> GenomeLandmarkAsset {
        GenomeLandmarkAsset {
            role,
            path: path.to_owned(),
            format,
            bytes: 1,
            sha256: "a".repeat(64),
            source_url: "https://example.test/source".to_owned(),
            upstream_project: "Example".to_owned(),
            upstream_version: "1".to_owned(),
            derivation: "copied".to_owned(),
            citation: "Example citation".to_owned(),
            license: "Example license".to_owned(),
            license_url: "https://example.test/license".to_owned(),
            required_notice: String::new(),
        }
    }

    fn catalogue() -> GenomeLandmarkCatalogue {
        let contigs = vec![
            LandmarkContigFingerprint {
                name: "chr1".to_owned(),
                length: 100,
                md5: Some("1".repeat(32)),
                primary: true,
            },
            LandmarkContigFingerprint {
                name: "chr2".to_owned(),
                length: 200,
                md5: Some("2".repeat(32)),
                primary: true,
            },
            LandmarkContigFingerprint {
                name: "chrM".to_owned(),
                length: 50,
                md5: Some("3".repeat(32)),
                primary: true,
            },
        ];
        let dictionary = ReferenceDictionary::from_contigs(
            contigs
                .iter()
                .map(|contig| (contig.name.clone(), contig.length)),
        );
        GenomeLandmarkCatalogue {
            schema_version: LANDMARK_CATALOGUE_SCHEMA_VERSION,
            bundles: vec![GenomeLandmarkBundle {
                id: "test".to_owned(),
                version: "1.0-rgv1".to_owned(),
                display_name: "Test genes".to_owned(),
                provider: "Example".to_owned(),
                release: "1".to_owned(),
                selection_policy: "one transcript per gene".to_owned(),
                assembly: LandmarkAssemblyIdentity {
                    stable_id: "TEST.1".to_owned(),
                    display_name: "Test assembly".to_owned(),
                    compatible_ids: vec!["test".to_owned()],
                    primary_contig_count: 3,
                    dictionary_sha256: dictionary_sha256(&dictionary),
                    compatible_dictionary_sha256: Vec::new(),
                    contigs,
                },
                assets: vec![
                    asset(
                        LandmarkAssetRole::RepresentativeGenes,
                        LandmarkAssetFormat::BigGenePred,
                        "test/genes.bb",
                    ),
                    asset(
                        LandmarkAssetRole::DetailedTranscripts,
                        LandmarkAssetFormat::BigGenePred,
                        "test/all.bb",
                    ),
                    asset(
                        LandmarkAssetRole::GeneSearch,
                        LandmarkAssetFormat::RgvGeneNamesV1Gzip,
                        "test/names.tsv.gz",
                    ),
                    asset(
                        LandmarkAssetRole::Cytobands,
                        LandmarkAssetFormat::UcscCytoband,
                        "test/cytobands.txt",
                    ),
                    asset(
                        LandmarkAssetRole::ContigAliases,
                        LandmarkAssetFormat::AliasTsv,
                        "test/aliases.tsv",
                    ),
                    asset(
                        LandmarkAssetRole::ChromosomeSizes,
                        LandmarkAssetFormat::ChromSizesTsv,
                        "test/chrom.sizes",
                    ),
                    asset(
                        LandmarkAssetRole::Notice,
                        LandmarkAssetFormat::Markdown,
                        "test/NOTICE.md",
                    ),
                ],
            }],
        }
    }

    fn sha256(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn catalogue_validation_rejects_unsafe_asset_paths() {
        let mut catalogue = catalogue();
        catalogue.bundles[0].assets[0].path = "../genes.bb".to_owned();
        assert!(matches!(
            catalogue.validate(),
            Err(GenomeLandmarkCatalogueError::UnsafePath(_))
        ));
    }

    #[test]
    fn catalogue_validation_rejects_duplicate_roles_and_wrong_formats() {
        let mut duplicate = catalogue();
        duplicate.bundles[0].assets[1].role = LandmarkAssetRole::RepresentativeGenes;
        assert!(matches!(
            duplicate.validate(),
            Err(GenomeLandmarkCatalogueError::DuplicateAssetRole { .. })
        ));

        let mut wrong_format = catalogue();
        wrong_format.bundles[0].assets[0].format = LandmarkAssetFormat::AliasTsv;
        assert!(matches!(
            wrong_format.validate(),
            Err(GenomeLandmarkCatalogueError::InvalidAsset(_))
        ));
    }

    #[test]
    fn asset_verification_rejects_corruption() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("test");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("genes.bb");
        fs::write(&path, b"x").unwrap();
        let mut catalogue = catalogue();
        catalogue.bundles[0].assets[0].sha256 = sha256(b"x");

        assert_eq!(
            catalogue
                .verify_asset(root.path(), "test", LandmarkAssetRole::RepresentativeGenes)
                .unwrap(),
            path.canonicalize().unwrap()
        );
        fs::write(&path, b"y").unwrap();
        assert!(matches!(
            catalogue.verify_asset(root.path(), "test", LandmarkAssetRole::RepresentativeGenes),
            Err(GenomeLandmarkCatalogueError::AssetHashMismatch { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn asset_verification_rejects_a_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), b"x").unwrap();
        let directory = root.path().join("test");
        fs::create_dir(&directory).unwrap();
        symlink(outside.path(), directory.join("genes.bb")).unwrap();
        let mut catalogue = catalogue();
        catalogue.bundles[0].assets[0].sha256 = sha256(b"x");

        assert!(matches!(
            catalogue.verify_asset(root.path(), "test", LandmarkAssetRole::RepresentativeGenes),
            Err(GenomeLandmarkCatalogueError::UnsafePath(_))
        ));
    }

    #[test]
    fn exact_dictionary_and_partial_fingerprints_resolve_confidently() {
        let catalogue = catalogue();
        catalogue.validate().unwrap();
        let all = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 100),
            ("chr2".to_owned(), 200),
            ("chrM".to_owned(), 50),
        ]);
        let exact = catalogue.resolve(&AssemblyResolutionInput {
            explicit_stable_id: None,
            dictionary: &all,
            sequence_md5: &BTreeMap::new(),
        });
        assert_eq!(exact.status, AssemblyResolutionStatus::Matched);
        assert_eq!(
            exact.evidence,
            AssemblyMatchEvidence::OrderedDictionarySha256
        );

        let md5 = BTreeMap::from([
            ("chr1".to_owned(), "1".repeat(32)),
            ("chr2".to_owned(), "2".repeat(32)),
            ("chrM".to_owned(), "3".repeat(32)),
        ]);
        let fingerprint = catalogue.resolve(&AssemblyResolutionInput {
            explicit_stable_id: None,
            dictionary: &all,
            sequence_md5: &md5,
        });
        assert_eq!(fingerprint.status, AssemblyResolutionStatus::Matched);
    }

    #[test]
    fn prefix_only_and_conflicting_md5_do_not_match() {
        let catalogue = catalogue();
        let prefix_only = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 101),
            ("chr2".to_owned(), 201),
            ("chrM".to_owned(), 51),
        ]);
        assert_eq!(
            catalogue
                .resolve(&AssemblyResolutionInput {
                    explicit_stable_id: None,
                    dictionary: &prefix_only,
                    sequence_md5: &BTreeMap::new()
                })
                .status,
            AssemblyResolutionStatus::Unknown
        );

        let dictionary = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 100),
            ("chr2".to_owned(), 200),
            ("chrM".to_owned(), 50),
        ]);
        let md5 = BTreeMap::from([
            ("chr1".to_owned(), "1".repeat(32)),
            ("chr2".to_owned(), "0".repeat(32)),
            ("chrM".to_owned(), "3".repeat(32)),
        ]);
        assert_ne!(
            catalogue
                .resolve(&AssemblyResolutionInput {
                    explicit_stable_id: None,
                    dictionary: &dictionary,
                    sequence_md5: &md5
                })
                .status,
            AssemblyResolutionStatus::Matched
        );
    }

    #[test]
    fn normalized_search_index_supports_exact_and_prefix_queries() {
        let text = "rgv_gene_names_v1\nbrca1\tBRCA1\tENSG1\tNM_1\tchr17\t10\t20\t-\tRNF53\n";
        let index = parse_landmark_search_index(text).unwrap();
        assert_eq!(index.search("BRC", 8)[0].display_symbol, "BRCA1");
        assert_eq!(index.search("rnf", 8)[0].gene_id, "ENSG1");
        assert_eq!(index.search("ensg1", 8)[0].display_symbol, "BRCA1");
        assert_eq!(index.search("nm_1", 8)[0].display_symbol, "BRCA1");
        assert_eq!(
            LandmarkSearchIndex::matching_term(&index.entries[0], "ensg"),
            Some("ENSG1")
        );
        assert_eq!(
            index
                .feature_entry("chr17", 10, 20, Some("NM_1"))
                .map(|entry| entry.display_symbol.as_str()),
            Some("BRCA1")
        );
    }

    #[test]
    fn landmark_search_ranks_symbols_then_aliases_then_stable_identifiers() {
        let text = concat!(
            "rgv_gene_names_v1\n",
            "zzz\tZZZ\tABCGENE\tNM_3\tchr3\t30\t40\t+\t\n",
            "aliasgene\tALIASGENE\tENSG2\tNM_2\tchr2\t20\t30\t+\tABC\n",
            "abcd\tABCD\tENSG1\tNM_1\tchr1\t10\t20\t+\t\n",
        );
        let index = parse_landmark_search_index(text).unwrap();
        assert_eq!(
            index
                .search("abc", 8)
                .iter()
                .map(|entry| entry.display_symbol.as_str())
                .collect::<Vec<_>>(),
            ["ABCD", "ALIASGENE", "ZZZ"]
        );
        assert_eq!(
            LandmarkSearchIndex::matching_term(&index.entries[1], "abc"),
            Some("ABC")
        );
    }

    #[test]
    fn sorted_landmark_search_matches_the_exhaustive_scan() {
        use std::fmt::Write as _;

        let mut text = String::from("rgv_gene_names_v1\n");
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for index in 0..600_u64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let symbol = format!(
                "{}{}",
                ["Ab", "aB", "abc", "B", "Zeta"][(state % 5) as usize],
                state % 17
            );
            let synonym = if state.is_multiple_of(3) {
                format!("AB{},b{}", state % 7, state % 5)
            } else {
                String::new()
            };
            let start = 5_000_000_000 + index;
            writeln!(
                text,
                "{}\t{symbol}\tabG{}\tNM_{index}\tchr{}\t{start}\t{}\t+\t{synonym}",
                symbol.to_ascii_lowercase(),
                state % 11,
                state % 4,
                start + 10,
            )
            .unwrap();
        }
        let index = parse_landmark_search_index(&text).unwrap();
        for query in [
            "a", "AB", "ab1", "abc", "b", "b4", "zeta16", "nm_5", "abg", "q", " Ab ",
        ] {
            for maximum in [1, 7, 10_000] {
                let normalized = query.trim().to_ascii_lowercase();
                assert_eq!(
                    index.search(query, maximum),
                    index.search_by_scan(&normalized, maximum),
                    "query {query:?} maximum {maximum}"
                );
            }
        }
        assert!(!index.search("ab", 10_000).is_empty());
        assert!(index.search("", 5).is_empty());

        // `entries` is public: a resized index falls back to the scan.
        let mut resized = index.clone();
        resized.entries.truncate(3);
        assert_eq!(resized.search("a", 100), resized.search_by_scan("a", 100));
    }

    #[test]
    fn all_supported_build_fixtures_resolve_by_exact_dictionary() {
        let builds = [
            ("hg38", "GRCh38.no_alt_analysis_set", 100_u64),
            ("hg19", "GRCh37.p13", 200_u64),
            ("mm10", "GRCm38.p6", 300_u64),
            ("mm39", "GRCm39", 400_u64),
            ("hs1", "T2T-CHM13v2.0", 500_u64),
        ];
        let mut template_catalogue = catalogue();
        let template = template_catalogue.bundles.remove(0);
        let bundles = builds
            .iter()
            .map(|(id, stable_id, base)| {
                let mut bundle = template.clone();
                bundle.id = (*id).to_owned();
                bundle.display_name = format!("{id} genes");
                bundle.assembly.stable_id = (*stable_id).to_owned();
                bundle.assembly.display_name = (*id).to_owned();
                bundle.assembly.compatible_ids = vec![(*id).to_owned()];
                bundle.assembly.contigs = vec![
                    LandmarkContigFingerprint {
                        name: "chr1".to_owned(),
                        length: *base + 1,
                        md5: None,
                        primary: true,
                    },
                    LandmarkContigFingerprint {
                        name: "chr2".to_owned(),
                        length: *base + 2,
                        md5: None,
                        primary: true,
                    },
                    LandmarkContigFingerprint {
                        name: "chrM".to_owned(),
                        length: *base + 3,
                        md5: None,
                        primary: true,
                    },
                ];
                let dictionary = ReferenceDictionary::from_contigs(
                    bundle
                        .assembly
                        .contigs
                        .iter()
                        .map(|contig| (contig.name.clone(), contig.length)),
                );
                bundle.assembly.dictionary_sha256 = dictionary_sha256(&dictionary);
                for asset in &mut bundle.assets {
                    asset.path = format!("{id}/{}", asset.path.rsplit('/').next().unwrap());
                }
                bundle
            })
            .collect::<Vec<_>>();
        let catalogue = GenomeLandmarkCatalogue {
            schema_version: LANDMARK_CATALOGUE_SCHEMA_VERSION,
            bundles,
        };
        catalogue.validate().unwrap();

        for (id, _, base) in builds {
            let dictionary = ReferenceDictionary::from_contigs([
                ("chr1".to_owned(), base + 1),
                ("chr2".to_owned(), base + 2),
                ("chrM".to_owned(), base + 3),
            ]);
            let resolution = catalogue.resolve(&AssemblyResolutionInput {
                explicit_stable_id: None,
                dictionary: &dictionary,
                sequence_md5: &BTreeMap::new(),
            });
            assert_eq!(resolution.status, AssemblyResolutionStatus::Matched);
            assert_eq!(resolution.bundle_id.as_deref(), Some(id));
        }
    }

    #[test]
    fn explicit_identity_rejects_a_conflicting_dictionary() {
        let catalogue = catalogue();
        let conflicting = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 101),
            ("chr2".to_owned(), 200),
            ("chrM".to_owned(), 50),
        ]);
        let resolution = catalogue.resolve(&AssemblyResolutionInput {
            explicit_stable_id: Some("TEST.1"),
            dictionary: &conflicting,
            sequence_md5: &BTreeMap::new(),
        });
        assert_eq!(resolution.status, AssemblyResolutionStatus::Incompatible);
        assert!(
            resolution
                .rejected
                .iter()
                .any(|reason| reason.contains("chr1"))
        );
    }

    fn resolve_dictionary(
        catalogue: &GenomeLandmarkCatalogue,
        contigs: &[(&str, u64)],
    ) -> AssemblyResolution {
        let dictionary = ReferenceDictionary::from_contigs(
            contigs
                .iter()
                .map(|(name, length)| ((*name).to_owned(), *length)),
        );
        catalogue.resolve(&AssemblyResolutionInput {
            explicit_stable_id: None,
            dictionary: &dictionary,
            sequence_md5: &BTreeMap::new(),
        })
    }

    #[test]
    fn primary_contigs_with_extra_sequences_are_recognised() {
        let catalogue = catalogue();
        let resolution = resolve_dictionary(
            &catalogue,
            &[
                ("chr1", 100),
                ("chr2", 200),
                ("chrM", 50),
                ("chrEBV", 171_823),
                ("ERCC-00002", 1_061),
                ("phiX174", 5_386),
            ],
        );
        assert_eq!(resolution.status, AssemblyResolutionStatus::Matched);
        assert_eq!(
            resolution.evidence,
            AssemblyMatchEvidence::CompletePrimaryLengths
        );
        assert_eq!(resolution.bundle_id.as_deref(), Some("test"));
        assert_eq!(resolution.extra_contigs, 3);
        assert_eq!(resolution.matched_contigs, 3);
        assert!(!resolution.subset);
        assert!(resolution.reason.contains("inferred from primary contigs"));
        assert!(resolution.reason.contains("3 extra contigs"));

        // Extra contigs never change the identity, wherever they sit.
        let reordered = resolve_dictionary(
            &catalogue,
            &[
                ("ERCC-00002", 1_061),
                ("chrM", 50),
                ("chr2", 200),
                ("decoy_hs37d5", 35_477_943),
                ("chr1", 100),
            ],
        );
        assert_eq!(reordered.status, AssemblyResolutionStatus::Matched);
        assert_eq!(reordered.extra_contigs, 2);

        // An exact dictionary has no extras.
        let exact = resolve_dictionary(&catalogue, &[("chr1", 100), ("chr2", 200), ("chrM", 50)]);
        assert_eq!(
            exact.evidence,
            AssemblyMatchEvidence::OrderedDictionarySha256
        );
        assert_eq!(exact.extra_contigs, 0);
    }

    #[test]
    fn a_missing_or_mismatched_primary_chromosome_is_refused() {
        let catalogue = catalogue();
        let missing = resolve_dictionary(&catalogue, &[("chr1", 100), ("chrM", 50), ("ERCC", 9)]);
        assert_eq!(missing.status, AssemblyResolutionStatus::Unknown);
        assert_eq!(missing.bundle_id, None);
        assert!(
            missing
                .rejected
                .iter()
                .any(|reason| reason.contains("missing primary chr2")),
            "{missing:?}"
        );

        let wrong_length = resolve_dictionary(
            &catalogue,
            &[("chr1", 100), ("chr2", 201), ("chrM", 50), ("ERCC", 9)],
        );
        assert_eq!(wrong_length.status, AssemblyResolutionStatus::Unknown);
        assert!(
            wrong_length
                .rejected
                .iter()
                .any(|reason| reason.contains("chr2 has length 201, expected 200"))
        );

        // Both spellings of one chromosome: which one is meant is a guess.
        let doubled = resolve_dictionary(
            &catalogue,
            &[("chr1", 100), ("1", 100), ("chr2", 200), ("chrM", 50)],
        );
        assert_ne!(doubled.status, AssemblyResolutionStatus::Matched);
    }

    #[test]
    fn a_catalogued_non_primary_sequence_of_another_length_is_refused() {
        let mut catalogue = catalogue();
        catalogue.bundles[0]
            .assembly
            .contigs
            .push(LandmarkContigFingerprint {
                name: "chrUn_KI270302v1".to_owned(),
                length: 2_274,
                md5: None,
                primary: false,
            });
        let matching = resolve_dictionary(
            &catalogue,
            &[
                ("chr1", 100),
                ("chr2", 200),
                ("chrM", 50),
                ("chrUn_KI270302v1", 2_274),
                ("ERCC", 9),
            ],
        );
        assert_eq!(matching.status, AssemblyResolutionStatus::Matched);
        // The catalogued unplaced sequence belongs to the build.
        assert_eq!(matching.extra_contigs, 1);

        let conflicting = resolve_dictionary(
            &catalogue,
            &[
                ("chr1", 100),
                ("chr2", 200),
                ("chrM", 50),
                ("chrUn_KI270302v1", 2_275),
            ],
        );
        assert_eq!(conflicting.status, AssemblyResolutionStatus::Unknown);
    }

    #[test]
    fn primary_contigs_match_under_chr_and_mitochondrial_aliases() {
        let catalogue = catalogue();
        let ensembl = resolve_dictionary(
            &catalogue,
            &[("1", 100), ("2", 200), ("MT", 50), ("GL000192.1", 547_496)],
        );
        assert_eq!(ensembl.status, AssemblyResolutionStatus::Matched);
        assert_eq!(ensembl.extra_contigs, 1);
        assert_eq!(ensembl.matched_contigs, 3);

        // A mitochondrial genome of another length (b37 rCRS against hg19
        // chrM) is left unbound and reported, and its absence is tolerated;
        // a nuclear chromosome never is.
        let other_mitochondrion =
            resolve_dictionary(&catalogue, &[("1", 100), ("2", 200), ("MT", 49)]);
        assert_eq!(
            other_mitochondrion.status,
            AssemblyResolutionStatus::Matched
        );
        assert_eq!(other_mitochondrion.extra_contigs, 1);
        assert!(other_mitochondrion.reason.contains("MT has length 49"));
        let no_mitochondrion = resolve_dictionary(&catalogue, &[("chr1", 100), ("chr2", 200)]);
        assert_eq!(no_mitochondrion.status, AssemblyResolutionStatus::Matched);
        assert_eq!(no_mitochondrion.matched_contigs, 2);
    }

    #[test]
    fn sequence_md5_conflicts_block_the_primary_contig_rule() {
        let catalogue = catalogue();
        let dictionary = ReferenceDictionary::from_contigs([
            ("chr1".to_owned(), 100),
            ("chr2".to_owned(), 200),
            ("chrM".to_owned(), 50),
            ("ERCC".to_owned(), 9),
        ]);
        let md5 = BTreeMap::from([("chr2".to_owned(), "0".repeat(32))]);
        let resolution = catalogue.resolve(&AssemblyResolutionInput {
            explicit_stable_id: None,
            dictionary: &dictionary,
            sequence_md5: &md5,
        });
        assert_ne!(resolution.status, AssemblyResolutionStatus::Matched);
    }

    /// Catalogue fixtures for the five supported builds, with distinct
    /// lengths and the mouse builds carrying 19 autosomes.
    fn supported_builds_catalogue() -> GenomeLandmarkCatalogue {
        let builds = [
            ("hg38", "GRCh38.no_alt_analysis_set", 1_000_u64, 22),
            ("hg19", "GRCh37.p13", 2_000_u64, 22),
            ("mm10", "GRCm38.p6", 3_000_u64, 19),
            ("mm39", "GRCm39", 4_000_u64, 19),
            ("hs1", "T2T-CHM13v2.0", 5_000_u64, 22),
        ];
        let mut template_catalogue = catalogue();
        let template = template_catalogue.bundles.remove(0);
        let bundles = builds
            .iter()
            .map(|(id, stable_id, base, autosomes)| {
                let mut bundle = template.clone();
                bundle.id = (*id).to_owned();
                bundle.assembly.stable_id = (*stable_id).to_owned();
                bundle.assembly.display_name = (*id).to_owned();
                bundle.assembly.compatible_ids = vec![(*id).to_owned()];
                bundle.assembly.contigs = (1..=*autosomes)
                    .map(|number| format!("chr{number}"))
                    .chain(["chrX".to_owned(), "chrY".to_owned(), "chrM".to_owned()])
                    .enumerate()
                    .map(|(index, name)| LandmarkContigFingerprint {
                        name,
                        length: base + index as u64,
                        md5: None,
                        primary: true,
                    })
                    .collect();
                bundle.assembly.primary_contig_count = bundle.assembly.contigs.len();
                let dictionary = ReferenceDictionary::from_contigs(
                    bundle
                        .assembly
                        .contigs
                        .iter()
                        .map(|contig| (contig.name.clone(), contig.length)),
                );
                bundle.assembly.dictionary_sha256 = dictionary_sha256(&dictionary);
                for asset in &mut bundle.assets {
                    asset.path = format!("{id}/{}", asset.path.rsplit('/').next().unwrap());
                }
                bundle
            })
            .collect();
        let catalogue = GenomeLandmarkCatalogue {
            schema_version: LANDMARK_CATALOGUE_SCHEMA_VERSION,
            bundles,
        };
        catalogue.validate().unwrap();
        catalogue
    }

    #[test]
    fn every_supported_build_is_recognised_beside_extra_contigs() {
        let catalogue = supported_builds_catalogue();
        for bundle in &catalogue.bundles {
            let mut contigs = bundle
                .assembly
                .contigs
                .iter()
                .map(|contig| {
                    // Ensembl-style names for the mouse builds.
                    let name = if bundle.id.starts_with("mm") {
                        contig.name.trim_start_matches("chr").replace('M', "MT")
                    } else {
                        contig.name.clone()
                    };
                    (name, contig.length)
                })
                .collect::<Vec<_>>();
            contigs.push(("ERCC-00002".to_owned(), 1_061));
            contigs.push(("phiX174".to_owned(), 5_386));
            let dictionary = ReferenceDictionary::from_contigs(contigs);
            let resolution = catalogue.resolve(&AssemblyResolutionInput {
                explicit_stable_id: None,
                dictionary: &dictionary,
                sequence_md5: &BTreeMap::new(),
            });
            assert_eq!(
                resolution.status,
                AssemblyResolutionStatus::Matched,
                "{}: {resolution:?}",
                bundle.id
            );
            assert_eq!(resolution.bundle_id.as_deref(), Some(bundle.id.as_str()));
            assert_eq!(resolution.extra_contigs, 2);
        }

        // A mouse chromosome set never matches a human build, nor the reverse.
        let human_on_mouse_names = resolve_dictionary(
            &catalogue,
            &[("chr1", 3_000), ("chr2", 3_001), ("chr20", 3_019)],
        );
        assert_ne!(
            human_on_mouse_names.status,
            AssemblyResolutionStatus::Matched
        );
    }

    #[test]
    fn two_builds_with_the_same_primary_contigs_are_ambiguous() {
        let mut catalogue = supported_builds_catalogue();
        let mut twin = catalogue.bundles[0].clone();
        twin.id = "hg38-twin".to_owned();
        twin.assembly.stable_id = "GRCh38-twin".to_owned();
        twin.assembly.compatible_ids = Vec::new();
        twin.assembly.dictionary_sha256 = "b".repeat(64);
        catalogue.bundles.push(twin);
        catalogue.validate().unwrap();
        let mut contigs = catalogue.bundles[0]
            .assembly
            .contigs
            .iter()
            .map(|contig| (contig.name.clone(), contig.length))
            .collect::<Vec<_>>();
        contigs.push(("ERCC-00002".to_owned(), 1_061));
        let dictionary = ReferenceDictionary::from_contigs(contigs);
        let resolution = catalogue.resolve(&AssemblyResolutionInput {
            explicit_stable_id: None,
            dictionary: &dictionary,
            sequence_md5: &BTreeMap::new(),
        });
        assert_eq!(resolution.status, AssemblyResolutionStatus::Ambiguous);
        assert_eq!(resolution.bundle_id, None);
    }

    #[test]
    fn landmark_root_discovery_takes_the_first_location_with_a_catalogue() {
        let temp = tempfile::tempdir().unwrap();
        let folder = |name: &str, catalogue: bool| {
            let path = temp.path().join(name);
            fs::create_dir_all(&path).unwrap();
            if catalogue {
                fs::write(path.join("catalog.json"), b"{}").unwrap();
            }
            path
        };
        let candidate = |source, path: &Path| LandmarkRootCandidate {
            source,
            path: path.to_path_buf(),
        };
        let environment = folder("environment", true);
        let setting = folder("setting", true);
        let user_data = folder("user", true);
        let bundled = folder("bundled", true);
        let ordered = |environment: &Path, setting: &Path, user_data: &Path| {
            discover_landmark_root([
                candidate(LandmarkRootSource::Environment, environment),
                candidate(LandmarkRootSource::Setting, setting),
                candidate(LandmarkRootSource::UserData, user_data),
                candidate(LandmarkRootSource::Bundled, &bundled),
            ])
        };

        let found = ordered(&environment, &setting, &user_data);
        assert_eq!(
            found.root,
            Some(candidate(LandmarkRootSource::Environment, &environment))
        );
        assert_eq!(found.configuration_problem(), None);

        fs::remove_file(environment.join("catalog.json")).unwrap();
        let found = ordered(&environment, &setting, &user_data);
        assert_eq!(
            found.root,
            Some(candidate(LandmarkRootSource::Setting, &setting))
        );
        // A named location without a catalogue is recorded, not hidden.
        assert_eq!(found.unusable.len(), 1);

        fs::remove_file(setting.join("catalog.json")).unwrap();
        let found = ordered(&environment, &setting, &user_data);
        assert_eq!(
            found.root,
            Some(candidate(LandmarkRootSource::UserData, &user_data))
        );

        fs::remove_file(user_data.join("catalog.json")).unwrap();
        let found = ordered(&environment, &setting, &user_data);
        assert_eq!(
            found.root,
            Some(candidate(LandmarkRootSource::Bundled, &bundled))
        );

        fs::remove_file(bundled.join("catalog.json")).unwrap();
        let found = ordered(&environment, &setting, &user_data);
        assert_eq!(found.root, None);
        let problem = found.configuration_problem().unwrap();
        assert!(problem.contains("RGV_LANDMARK_RESOURCE_ROOT"));
        assert!(problem.contains("landmark folder setting"));

        // Nothing configured and nothing bundled is "not configured", not an error.
        let empty = discover_landmark_root([candidate(LandmarkRootSource::UserData, &user_data)]);
        assert_eq!(empty, LandmarkRootDiscovery::default());
        assert_eq!(empty.configuration_problem(), None);

        // The parent `landmarks/` folder written by the curation script is accepted.
        let parent = folder("landmarks", false);
        let igv = folder("landmarks/igv", true);
        assert_eq!(
            discover_landmark_root([candidate(LandmarkRootSource::Setting, &parent)]).root,
            Some(candidate(LandmarkRootSource::Setting, &igv))
        );
    }
}
