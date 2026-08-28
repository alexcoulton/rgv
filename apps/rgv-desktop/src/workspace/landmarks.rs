use std::{collections::BTreeMap, io::Read, path::Path};

use flate2::read::MultiGzDecoder;

use super::super::*;

const MAXIMUM_LANDMARK_SEARCH_BYTES: u64 = 64 * 1024 * 1024;

impl Viewer {
    pub(crate) fn automation_landmark_snapshot(&self) -> AutomationLandmarkSnapshot {
        let resource_configured = self.landmark_resource_root.is_some();
        let catalogue_schema_version = self
            .landmark_catalogue
            .as_ref()
            .map(|catalogue| catalogue.schema_version);
        let resolution = self.landmark_resolution.as_ref();
        let bundle = resolution
            .and_then(|resolution| resolution.bundle_id.as_deref())
            .and_then(|bundle_id| {
                self.landmark_catalogue
                    .as_ref()
                    .and_then(|catalogue| catalogue.bundle(bundle_id))
            });
        let representative_track = bundle.and_then(|bundle| {
            self.tracks.tracks().iter().find(|track| {
                track.built_in.as_ref().is_some_and(|identity| {
                    identity.bundle_id == bundle.id
                        && identity.bundle_version == bundle.version
                        && identity.asset_role == LandmarkAssetRole::RepresentativeGenes
                })
            })
        });
        let automatic_track_loaded = representative_track.is_some();
        let automatic_track_visible = representative_track.is_some_and(|track| {
            self.track_presentation
                .track_content_exposed(ui::TrackPane::Annotation, track.id)
                .unwrap_or(false)
        });
        let suppressed =
            bundle.is_some_and(|bundle| self.suppressed_builtin_gene_bundles.contains(&bundle.id));
        let failed = self.landmark_catalogue_error.is_some();
        let matched = resolution.is_some_and(|resolution| {
            resolution.status == rgv_core::AssemblyResolutionStatus::Matched
        });
        let automatic_track_state = if failed {
            AutomationLandmarkTrackState::Failed
        } else if suppressed {
            AutomationLandmarkTrackState::Suppressed
        } else if automatic_track_visible {
            AutomationLandmarkTrackState::Visible
        } else if automatic_track_loaded {
            AutomationLandmarkTrackState::Loaded
        } else if matched {
            AutomationLandmarkTrackState::Matched
        } else if resource_configured && catalogue_schema_version.is_some() {
            AutomationLandmarkTrackState::Configured
        } else {
            AutomationLandmarkTrackState::Unconfigured
        };
        let integrity = if failed {
            AutomationLandmarkIntegrityState::Failed
        } else if matched {
            AutomationLandmarkIntegrityState::Verified
        } else if resource_configured && catalogue_schema_version.is_some() {
            AutomationLandmarkIntegrityState::Unchecked
        } else {
            AutomationLandmarkIntegrityState::Unavailable
        };
        let representative_asset =
            bundle.and_then(|bundle| bundle.asset(LandmarkAssetRole::RepresentativeGenes));
        AutomationLandmarkSnapshot {
            catalogue_schema_version,
            resource_configured,
            resolution_status: resolution.map(|resolution| resolution.status),
            evidence: resolution.map(|resolution| resolution.evidence),
            subset: resolution.is_some_and(|resolution| resolution.subset),
            matched_bundle_id: bundle.map(|bundle| bundle.id.clone()),
            matched_bundle_version: bundle.map(|bundle| bundle.version.clone()),
            integrity,
            automatic_track_state,
            automatic_track_loaded,
            automatic_track_visible,
            suppressed,
            gene_asset_format: representative_asset.map(|asset| asset.format),
            gene_asset_sha256: representative_asset.map(|asset| asset.sha256.clone()),
            search_ready: self.landmark_search_index.is_some(),
        }
    }

    pub(crate) fn enrich_builtin_feature_record(
        &self,
        track_id: TrackId,
        record: &mut rgv_hts::FeatureRecord,
    ) {
        let Some(identity) = self
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .and_then(|track| track.built_in.as_ref())
        else {
            return;
        };
        let Some(bundle) = self
            .landmark_catalogue
            .as_ref()
            .and_then(|catalogue| catalogue.bundle(&identity.bundle_id))
        else {
            return;
        };
        let Some(asset) = bundle.asset(identity.asset_role) else {
            return;
        };

        let transcript_id = record
            .attributes
            .iter()
            .find(|(key, _)| key == "transcript_id")
            .map(|(_, value)| value.as_str());
        if let Some(entry) = self.landmark_search_index.as_ref().and_then(|index| {
            index.entries.iter().find(|entry| {
                entry.contig == record.contig
                    && entry.start == record.start
                    && entry.end == record.end
                    && transcript_id.is_none_or(|id| id == entry.transcript_id)
            })
        }) {
            insert_feature_attribute(record, "gene_id", &entry.gene_id);
            insert_feature_attribute(record, "gene_name", &entry.display_symbol);
            insert_feature_attribute(record, "transcript_id", &entry.transcript_id);
        }

        let inferred_biotype = if record.coding_interval.is_some() {
            "protein_coding"
        } else {
            "non_coding"
        };
        insert_feature_attribute(record, "gene_biotype", inferred_biotype);
        insert_feature_attribute(record, "landmark_bundle_id", &bundle.id);
        insert_feature_attribute(record, "landmark_bundle_version", &bundle.version);
        insert_feature_attribute(record, "landmark_assembly", &bundle.assembly.display_name);
        insert_feature_attribute(record, "landmark_provider", &bundle.provider);
        insert_feature_attribute(record, "landmark_release", &bundle.release);
        insert_feature_attribute(
            record,
            "landmark_selection_policy",
            &bundle.selection_policy,
        );
        insert_feature_attribute(
            record,
            "landmark_asset_role",
            landmark_asset_role_name(identity.asset_role),
        );
        insert_feature_attribute(
            record,
            "landmark_asset_format",
            landmark_asset_format_name(asset.format),
        );
        insert_feature_attribute(record, "landmark_asset_sha256", &asset.sha256);
        insert_feature_attribute(record, "landmark_citation", &asset.citation);
        insert_feature_attribute(record, "landmark_license", &asset.license);
        insert_feature_attribute(record, "landmark_license_url", &asset.license_url);
    }

    pub(crate) fn update_landmark_resolution_from_dictionary(
        &mut self,
        dictionary: &ReferenceDictionary,
    ) {
        self.resolve_landmark_assembly(dictionary, &BTreeMap::new());
    }

    pub(crate) fn update_landmark_resolution_from_header(
        &mut self,
        header: &rgv_hts::AlignmentHeaderMetadata,
    ) {
        if header.reference_sequences.is_empty() {
            return;
        }
        let Some(catalogue) = self.landmark_catalogue.as_ref() else {
            return;
        };
        let resolution = rgv_hts::resolve_assembly_from_header(catalogue, header);
        if self.landmark_resolution.as_ref() != Some(&resolution) {
            self.apply_landmark_resolution(resolution);
        }
    }

    fn resolve_landmark_assembly(
        &mut self,
        dictionary: &ReferenceDictionary,
        sequence_md5: &BTreeMap<String, String>,
    ) {
        let Some(catalogue) = self.landmark_catalogue.as_ref() else {
            return;
        };
        let explicit = self
            .assembly_identity
            .as_ref()
            .filter(|identity| {
                identity.confidence == rgv_core::AssemblyIdentityConfidence::Explicit
            })
            .map(|identity| identity.stable_id.as_str());
        let resolution = catalogue.resolve(&rgv_core::AssemblyResolutionInput {
            explicit_stable_id: explicit,
            dictionary: &dictionary,
            sequence_md5,
        });
        if self.landmark_resolution.as_ref() == Some(&resolution) {
            return;
        }
        self.apply_landmark_resolution(resolution);
    }

    fn apply_landmark_resolution(&mut self, resolution: rgv_core::AssemblyResolution) {
        let previous_bundle = self
            .landmark_resolution
            .as_ref()
            .and_then(|previous| previous.bundle_id.clone());
        let next_bundle = resolution.bundle_id.clone();
        if previous_bundle.is_some() && previous_bundle != next_bundle {
            if let Err(error) = self.remove_installed_landmark_context(previous_bundle.as_deref()) {
                self.landmark_catalogue_error = Some(format!(
                    "Could not retire the previous landmark context: {error}"
                ));
                return;
            }
        }
        let stable_id = resolution.stable_id.clone();
        let display_name = resolution.display_name.clone();
        match resolution.status {
            rgv_core::AssemblyResolutionStatus::Matched => {
                let confidence = match resolution.evidence {
                    rgv_core::AssemblyMatchEvidence::ExplicitIdentity => {
                        rgv_core::AssemblyIdentityConfidence::Explicit
                    }
                    rgv_core::AssemblyMatchEvidence::OrderedDictionarySha256 => {
                        rgv_core::AssemblyIdentityConfidence::Dictionary
                    }
                    rgv_core::AssemblyMatchEvidence::SequenceMd5 => {
                        rgv_core::AssemblyIdentityConfidence::Fingerprint
                    }
                    rgv_core::AssemblyMatchEvidence::CompletePrimaryLengths
                    | rgv_core::AssemblyMatchEvidence::None => {
                        rgv_core::AssemblyIdentityConfidence::Unknown
                    }
                };
                let identity = GenomeAssemblyIdentity {
                    stable_id: stable_id.unwrap_or_default(),
                    display_name: display_name.unwrap_or_default(),
                    confidence,
                    provenance: resolution.reason.clone(),
                    matched_fingerprints: resolution.matched_contigs,
                    expected_primary_contigs: resolution.expected_contigs,
                };
                self.assembly_identity = Some(identity.clone());
                self.assembly.identity = Some(identity);
                self.suggested_assembly_identity = None;
                if let Some(bundle_id) = resolution.bundle_id.as_deref()
                    && let Err(error) = self.install_builtin_landmark_context(bundle_id)
                {
                    let message = format!(
                        "Built-in landmark bundle {bundle_id} could not be installed: {error}"
                    );
                    self.landmark_catalogue_error = Some(message.clone());
                    self.data_status = message;
                }
            }
            rgv_core::AssemblyResolutionStatus::Suggested => {
                self.suggested_assembly_identity = Some(GenomeAssemblyIdentity {
                    stable_id: stable_id.unwrap_or_default(),
                    display_name: display_name.unwrap_or_default(),
                    confidence: rgv_core::AssemblyIdentityConfidence::Unknown,
                    provenance: resolution.reason.clone(),
                    matched_fingerprints: resolution.matched_contigs,
                    expected_primary_contigs: resolution.expected_contigs,
                });
            }
            rgv_core::AssemblyResolutionStatus::Ambiguous
            | rgv_core::AssemblyResolutionStatus::Unknown
            | rgv_core::AssemblyResolutionStatus::Incompatible => {
                self.suggested_assembly_identity = None;
            }
        }
        self.landmark_resolution = Some(resolution);
    }

    fn remove_installed_landmark_context(&mut self, bundle_id: Option<&str>) -> Result<()> {
        let ids = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| {
                track
                    .built_in
                    .as_ref()
                    .is_some_and(|identity| bundle_id.is_none_or(|id| identity.bundle_id == id))
            })
            .map(|track| track.id)
            .collect::<Vec<_>>();
        self.remove_auxiliary_tracks_with_suppression(&ids, false)?;
        self.contig_aliases.clear();
        self.assembly.cytobands.clear();
        self.cytoband_path = None;
        self.assembly.default_annotation_count = 0;
        self.landmark_search_index = None;
        Ok(())
    }

    pub(crate) fn install_builtin_landmark_context(&mut self, bundle_id: &str) -> Result<()> {
        let root = self
            .landmark_resource_root
            .as_deref()
            .context("built-in landmark resource root is unavailable")?
            .to_path_buf();
        let catalogue = self
            .landmark_catalogue
            .as_ref()
            .context("built-in landmark catalogue is unavailable")?;
        let bundle = catalogue
            .bundle(bundle_id)
            .with_context(|| format!("landmark bundle {bundle_id} is not installed"))?
            .clone();

        // Verify the complete compact context before publishing any of it to
        // the workspace. The optional search index remains failure-isolated.
        let genes_path =
            catalogue.verify_asset(&root, bundle_id, LandmarkAssetRole::RepresentativeGenes)?;
        let aliases_path =
            catalogue.verify_asset(&root, bundle_id, LandmarkAssetRole::ContigAliases)?;
        let cytobands_path =
            catalogue.verify_asset(&root, bundle_id, LandmarkAssetRole::Cytobands)?;

        let alias_text = std::fs::read_to_string(&aliases_path)
            .with_context(|| format!("failed to read {}", aliases_path.display()))?;
        let parsed_aliases = rgv_core::parse_landmark_aliases(&alias_text)?;
        let aliases =
            parsed_aliases
                .into_iter()
                .filter(|(alias, canonical)| {
                    self.assembly.dictionary.id_by_name(canonical).is_some()
                        && self.assembly.dictionary.id_by_name(alias).is_none_or(|id| {
                            self.assembly.dictionary.id_by_name(canonical) == Some(id)
                        })
                })
                .collect::<ContigAliases>();
        let cytoband_text = std::fs::read_to_string(&cytobands_path)
            .with_context(|| format!("failed to read {}", cytobands_path.display()))?;
        let cytobands = parse_ucsc_cytobands(&cytoband_text, &self.assembly.dictionary)?;

        let search_index = match catalogue.verify_asset(
            &root,
            bundle_id,
            LandmarkAssetRole::GeneSearch,
        ) {
            Ok(path) => match read_landmark_search(&path) {
                Ok(index) => Some(index),
                Err(error) => {
                    tracing::warn!(bundle = bundle_id, %error, "landmark search index unavailable");
                    None
                }
            },
            Err(error) => {
                tracing::warn!(bundle = bundle_id, %error, "landmark search index unavailable");
                None
            }
        };

        let has_equivalent = self.tracks.tracks().iter().any(|track| {
            track.built_in.as_ref().is_some_and(|identity| {
                identity.bundle_id == bundle.id
                    && identity.bundle_version == bundle.version
                    && identity.asset_role == LandmarkAssetRole::RepresentativeGenes
            })
        });
        let has_explicit_gene_track = self
            .tracks
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::GeneModel && track.built_in.is_none());
        let suppressed = self.suppressed_builtin_gene_bundles.contains(bundle_id);
        let mut added_track = None;
        if !has_equivalent && !has_explicit_gene_track && !suppressed {
            let asset = bundle
                .asset(LandmarkAssetRole::RepresentativeGenes)
                .context("representative gene asset is absent")?;
            let next_id = TrackId::new(
                self.tracks
                    .tracks()
                    .iter()
                    .map(|track| track.id.get())
                    .chain(
                        self.alignment_tracks
                            .iter()
                            .map(|track| track.config.id.get()),
                    )
                    .max()
                    .unwrap_or_default()
                    .saturating_add(1),
            );
            let mut config = TrackConfig::new(
                next_id,
                "Genes".to_owned(),
                TrackKind::GeneModel,
                track_source(&genes_path),
            )
            .with_built_in(rgv_core::BuiltInTrackIdentity {
                bundle_id: bundle.id.clone(),
                bundle_version: bundle.version.clone(),
                asset_role: LandmarkAssetRole::RepresentativeGenes,
                sha256: asset.sha256.clone(),
            });
            config.feature.display_mode = FeatureDisplayMode::Squished;
            config.height = 48.0;
            self.add_preconfigured_track_source(&genes_path, TrackSpecKind::GenePred, config)?;
            self.automation_auxiliary_open_state
                .insert(next_id, AutomationAuxiliaryOpenState::Pending);
            added_track = Some(next_id);
        }

        // Publish the already-verified compact context only after any required
        // track-engine insertion succeeds, so a frame cannot observe a partial
        // bundle installation.
        self.contig_aliases = aliases;
        self.assembly.cytobands = cytobands;
        self.cytoband_path = Some(cytobands_path);
        self.assembly.default_annotation_count = 1;
        self.landmark_search_index = if has_equivalent || added_track.is_some() {
            search_index
        } else {
            None
        };
        self.refresh_active_panel_context();
        if added_track.is_some() {
            self.request_all_panel_queries();
        } else {
            self.display_dirty = true;
        }
        self.landmark_catalogue_error = None;
        if added_track.is_some() {
            self.data_status = format!(
                "Loaded Genes for {} · {}",
                bundle.assembly.display_name, bundle.release
            );
        }
        Ok(())
    }

    pub(crate) fn merge_landmark_search_results(
        &self,
        query: &str,
        results: &mut TrackSearchResults,
    ) {
        let Some(index) = &self.landmark_search_index else {
            return;
        };
        let Some(track) = self.tracks.tracks().iter().find(|track| {
            track.built_in.as_ref().is_some_and(|identity| {
                identity.asset_role == LandmarkAssetRole::RepresentativeGenes
                    && !self
                        .suppressed_builtin_gene_bundles
                        .contains(&identity.bundle_id)
            })
        }) else {
            return;
        };
        let normalized_query = query.trim().to_ascii_lowercase();
        for entry in index.search(query, 50) {
            let already_present = results.hits.iter().any(|hit| {
                hit.track_id == track.id
                    && hit.record.contig == entry.contig
                    && hit.record.start == entry.start
                    && hit.record.end == entry.end
            });
            if already_present {
                continue;
            }
            results.hits.push(TrackSearchHit {
                track_id: track.id,
                track_name: track.name.clone(),
                matched_term: entry.normalized_term.clone(),
                record: rgv_hts::FeatureRecord {
                    contig: entry.contig.clone(),
                    start: entry.start,
                    end: entry.end,
                    name: Some(entry.display_symbol.clone()),
                    feature_type: Some("representative transcript".to_owned()),
                    score: None,
                    strand: match entry.strand {
                        '+' => rgv_hts::Strand::Forward,
                        '-' => rgv_hts::Strand::Reverse,
                        _ => rgv_hts::Strand::Unstranded,
                    },
                    blocks: Vec::new(),
                    coding_interval: None,
                    peak: None,
                    attributes: vec![
                        ("gene_id".to_owned(), entry.gene_id.clone()),
                        ("gene_name".to_owned(), entry.display_symbol.clone()),
                        ("transcript_id".to_owned(), entry.transcript_id.clone()),
                        ("source_format".to_owned(), "rgv_gene_names_v1".to_owned()),
                    ],
                },
            });
        }
        results.hits.sort_by(|left, right| {
            let left_exact = left.matched_term.eq_ignore_ascii_case(&normalized_query);
            let right_exact = right.matched_term.eq_ignore_ascii_case(&normalized_query);
            right_exact
                .cmp(&left_exact)
                .then_with(|| left.matched_term.cmp(&right.matched_term))
                .then_with(|| left.record.contig.cmp(&right.record.contig))
                .then_with(|| left.record.start.cmp(&right.record.start))
                .then_with(|| left.record.end.cmp(&right.record.end))
        });
        results.hits.truncate(50);
    }

    pub(crate) fn restore_default_genes(&mut self) -> Result<()> {
        let bundle_id = self
            .landmark_resolution
            .as_ref()
            .filter(|resolution| resolution.status == rgv_core::AssemblyResolutionStatus::Matched)
            .and_then(|resolution| resolution.bundle_id.clone())
            .context("no confidently matched landmark bundle is available")?;
        self.suppressed_builtin_gene_bundles.remove(&bundle_id);
        self.install_builtin_landmark_context(&bundle_id)
    }

    pub(crate) fn show_all_transcripts(&mut self, bundle_id: &str) -> Result<()> {
        if self.tracks.tracks().iter().any(|track| {
            track.built_in.as_ref().is_some_and(|identity| {
                identity.bundle_id == bundle_id
                    && identity.asset_role == LandmarkAssetRole::DetailedTranscripts
            })
        }) {
            self.data_status = "All transcripts are already open".to_owned();
            return Ok(());
        }
        let root = self
            .landmark_resource_root
            .as_deref()
            .context("built-in landmark resource root is unavailable")?
            .to_path_buf();
        let catalogue = self
            .landmark_catalogue
            .as_ref()
            .context("built-in landmark catalogue is unavailable")?;
        let bundle = catalogue
            .bundle(bundle_id)
            .with_context(|| format!("landmark bundle {bundle_id} is not installed"))?
            .clone();
        let asset = bundle
            .asset(LandmarkAssetRole::DetailedTranscripts)
            .context("detailed transcript asset is absent")?;
        let path =
            catalogue.verify_asset(&root, bundle_id, LandmarkAssetRole::DetailedTranscripts)?;
        let (spec_kind, format) = match asset.format {
            rgv_core::LandmarkAssetFormat::BigGenePred => {
                (TrackSpecKind::BigGenePred, Some(TrackFormat::BigGenePred))
            }
            rgv_core::LandmarkAssetFormat::GenePredExtGzip => (TrackSpecKind::GenePred, None),
            _ => anyhow::bail!("detailed transcript asset has an incompatible format"),
        };
        let next_id = TrackId::new(
            self.tracks
                .tracks()
                .iter()
                .map(|track| track.id.get())
                .chain(
                    self.alignment_tracks
                        .iter()
                        .map(|track| track.config.id.get()),
                )
                .max()
                .unwrap_or_default()
                .saturating_add(1),
        );
        let mut config = TrackConfig::new(
            next_id,
            format!("All transcripts · {}", bundle.provider),
            TrackKind::GeneModel,
            track_source(&path),
        )
        .with_built_in(rgv_core::BuiltInTrackIdentity {
            bundle_id: bundle.id.clone(),
            bundle_version: bundle.version.clone(),
            asset_role: LandmarkAssetRole::DetailedTranscripts,
            sha256: asset.sha256.clone(),
        });
        config.format = format;
        config.feature.display_mode = FeatureDisplayMode::Squished;
        self.add_preconfigured_track_source(&path, spec_kind, config)?;
        self.automation_auxiliary_open_state
            .insert(next_id, AutomationAuxiliaryOpenState::Pending);
        self.data_status = "Opening all transcripts…".to_owned();
        Ok(())
    }

    pub(crate) fn return_to_representative_genes(&mut self, bundle_id: &str) -> Result<()> {
        let detail_ids = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| {
                track.built_in.as_ref().is_some_and(|identity| {
                    identity.bundle_id == bundle_id
                        && identity.asset_role == LandmarkAssetRole::DetailedTranscripts
                })
            })
            .map(|track| track.id)
            .collect::<Vec<_>>();
        self.remove_auxiliary_tracks(&detail_ids)?;
        self.restore_default_genes()
    }

    pub(crate) fn open_landmark_resource_folder(&mut self) -> Result<()> {
        let root = self
            .landmark_resource_root
            .as_ref()
            .context("built-in landmark resource root is unavailable")?;
        #[cfg(target_os = "macos")]
        let status = std::process::Command::new("open").arg(root).status()?;
        #[cfg(target_os = "windows")]
        let status = std::process::Command::new("explorer").arg(root).status()?;
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let status = std::process::Command::new("xdg-open").arg(root).status()?;
        anyhow::ensure!(status.success(), "could not open landmark resource folder");
        self.data_status = format!("Opened landmark resources at {}", root.display());
        Ok(())
    }
}

fn insert_feature_attribute(record: &mut rgv_hts::FeatureRecord, key: &str, value: &str) {
    if !value.is_empty()
        && !record
            .attributes
            .iter()
            .any(|(candidate, _)| candidate == key)
    {
        record.attributes.push((key.to_owned(), value.to_owned()));
    }
}

const fn landmark_asset_role_name(role: LandmarkAssetRole) -> &'static str {
    match role {
        LandmarkAssetRole::RepresentativeGenes => "representative_genes",
        LandmarkAssetRole::DetailedTranscripts => "detailed_transcripts",
        LandmarkAssetRole::GeneSearch => "gene_search",
        LandmarkAssetRole::Cytobands => "cytobands",
        LandmarkAssetRole::ContigAliases => "contig_aliases",
        LandmarkAssetRole::ChromosomeSizes => "chromosome_sizes",
        LandmarkAssetRole::Notice => "notice",
    }
}

const fn landmark_asset_format_name(format: rgv_core::LandmarkAssetFormat) -> &'static str {
    match format {
        rgv_core::LandmarkAssetFormat::BigGenePred => "big_gene_pred",
        rgv_core::LandmarkAssetFormat::GenePredExtGzip => "gene_pred_ext_gzip",
        rgv_core::LandmarkAssetFormat::RgvGeneNamesV1Gzip => "rgv_gene_names_v1_gzip",
        rgv_core::LandmarkAssetFormat::UcscCytoband => "ucsc_cytoband",
        rgv_core::LandmarkAssetFormat::AliasTsv => "alias_tsv",
        rgv_core::LandmarkAssetFormat::ChromSizesTsv => "chrom_sizes_tsv",
        rgv_core::LandmarkAssetFormat::Markdown => "markdown",
    }
}

fn read_landmark_search(path: &Path) -> Result<LandmarkSearchIndex> {
    let file = std::fs::File::open(path)?;
    let mut decoder = MultiGzDecoder::new(file).take(MAXIMUM_LANDMARK_SEARCH_BYTES + 1);
    let mut text = String::new();
    decoder.read_to_string(&mut text)?;
    anyhow::ensure!(
        text.len() as u64 <= MAXIMUM_LANDMARK_SEARCH_BYTES,
        "landmark search index exceeds {MAXIMUM_LANDMARK_SEARCH_BYTES} decompressed bytes"
    );
    rgv_core::parse_landmark_search_index(&text).map_err(Into::into)
}
