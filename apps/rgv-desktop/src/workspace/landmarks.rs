use crate::automation::AutomationAuxiliaryOpenState;
use crate::sources::track_source;
use crate::{Viewer, ui};
use anyhow::{Context as _, Result};
use rgv_automation::{
    AutomationLandmarkIntegrityState, AutomationLandmarkSnapshot, AutomationLandmarkTrackState,
};
use rgv_core::{
    ContigAliases, FeatureDisplayMode, GenomeAssemblyIdentity, GenomeLandmarkCatalogue,
    LandmarkAssetRole, LandmarkSearchIndex, ReferenceDictionary, TrackConfig, TrackFormat, TrackId,
    TrackKind,
};
use rgv_engine::{TrackSearchHit, TrackSearchResults, TrackSpecKind};
use std::path::PathBuf;
use std::{collections::BTreeMap, io::Read, path::Path};

use flate2::read::MultiGzDecoder;

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
            self.session.tracks.tracks().iter().find(|track| {
                track.built_in.as_ref().is_some_and(|identity| {
                    identity.bundle_id == bundle.id
                        && identity.bundle_version == bundle.version
                        && identity.asset_role == LandmarkAssetRole::RepresentativeGenes
                })
            })
        });
        let automatic_track_loaded = representative_track.is_some();
        let automatic_track_visible = representative_track.is_some_and(|track| {
            self.session
                .track_presentation
                .track_content_exposed(ui::TrackPane::Annotation, track.id)
                .unwrap_or(false)
        });
        let suppressed = bundle.is_some_and(|bundle| {
            self.session
                .suppressed_builtin_gene_bundles
                .contains(&bundle.id)
        });
        let failed = self.landmark_catalogue_error.is_some();
        let matched = resolution.is_some_and(|resolution| {
            resolution.status == rgv_core::AssemblyResolutionStatus::Matched
        });
        let state = landmark_states(LandmarkStateInputs {
            resource_configured,
            catalogue_loaded: catalogue_schema_version.is_some(),
            failed,
            matched,
            suppressed,
            automatic_track_loaded,
            automatic_track_visible,
        });
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
            integrity: state.1,
            automatic_track_state: state.0,
            automatic_track_loaded,
            automatic_track_visible,
            suppressed,
            gene_asset_format: representative_asset.map(|asset| asset.format),
            gene_asset_sha256: representative_asset.map(|asset| asset.sha256.clone()),
            search_ready: self.landmark_search_index.is_some(),
            extra_contigs: resolution
                .filter(|_| matched)
                .map_or(0, |resolution| resolution.extra_contigs),
            resource_source: self.landmark_resource_source,
            catalogue_error: self.landmark_catalogue_error.clone(),
        }
    }

    pub(crate) fn enrich_builtin_feature_record(
        &self,
        track_id: TrackId,
        record: &mut rgv_hts::FeatureRecord,
    ) {
        let Some(identity) = self
            .session
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
            index.feature_entry(&record.contig, record.start, record.end, transcript_id)
        }) {
            apply_landmark_feature_identity(record, entry);
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

    #[cfg(any())]
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
            .session
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
                    rgv_core::AssemblyMatchEvidence::CompletePrimaryLengths => {
                        rgv_core::AssemblyIdentityConfidence::PrimaryContigs
                    }
                    rgv_core::AssemblyMatchEvidence::None => {
                        rgv_core::AssemblyIdentityConfidence::Unknown
                    }
                };
                let identity = GenomeAssemblyIdentity {
                    stable_id: stable_id.unwrap_or_default(),
                    display_name: assembly_display_name(
                        display_name.as_deref().unwrap_or_default(),
                        resolution.extra_contigs,
                    ),
                    confidence,
                    provenance: resolution.reason.clone(),
                    matched_fingerprints: resolution.matched_contigs,
                    expected_primary_contigs: resolution.expected_contigs,
                };
                self.session.assembly_identity = Some(identity.clone());
                self.session.assembly.identity = Some(identity);
                self.session.suggested_assembly_identity = None;
                if let Some(bundle_id) = resolution.bundle_id.as_deref()
                    && let Err(error) = self.install_builtin_landmark_context(bundle_id)
                {
                    let message = format!(
                        "Built-in landmark bundle {bundle_id} could not be installed: {error}"
                    );
                    self.landmark_catalogue_error = Some(message.clone());
                    self.report_error(message);
                }
            }
            rgv_core::AssemblyResolutionStatus::Suggested => {
                self.session.suggested_assembly_identity = Some(GenomeAssemblyIdentity {
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
                self.session.suggested_assembly_identity = None;
            }
        }
        self.landmark_resolution = Some(resolution);
    }

    fn remove_installed_landmark_context(&mut self, bundle_id: Option<&str>) -> Result<()> {
        let ids = self
            .session
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
        self.session.contig_aliases.clear();
        self.session.assembly.cytobands.clear();
        self.session.cytoband_path = None;
        self.session.assembly.default_annotation_count = 0;
        self.landmark_search_index = None;
        Ok(())
    }

    /// Starts installing a built-in landmark bundle. Verifying (SHA-256),
    /// reading and decoding its assets runs on a worker (APP-3); the context
    /// is applied by [`Self::finish_landmark_install`] when that completes.
    pub(crate) fn install_builtin_landmark_context(&mut self, bundle_id: &str) -> Result<()> {
        let root = self
            .landmark_resource_root
            .as_deref()
            .context("built-in landmark resource root is unavailable")?
            .to_path_buf();
        let catalogue = self
            .landmark_catalogue
            .as_ref()
            .context("built-in landmark catalogue is unavailable")?
            .clone();
        anyhow::ensure!(
            catalogue.bundle(bundle_id).is_some(),
            "landmark bundle {bundle_id} is not installed"
        );
        self.landmark_install_ticket = self.landmark_install_ticket.saturating_add(1);
        let request = LandmarkInstallRequest {
            ticket: self.landmark_install_ticket,
            bundle_id: bundle_id.to_owned(),
            dictionary: self.session.assembly.dictionary.clone(),
        };
        let worker = std::thread::Builder::new()
            .name("rgv-landmark-install".to_owned())
            .spawn({
                let request = request.clone();
                move || {
                    prepare_landmark_context(
                        &root,
                        &catalogue,
                        &request.bundle_id,
                        &request.dictionary,
                    )
                    .map(|prepared| {
                        crate::workspace::genomes::GenomeTaskOutcome::Landmark(Box::new(prepared))
                    })
                    .map_err(|error| format!("{error:#}"))
                }
            })
            .context("could not start the landmark install worker")?;
        self.genome_tasks
            .push(crate::workspace::genomes::GenomeTask {
                label: format!("landmark bundle {bundle_id}"),
                is_list: false,
                landmark: Some(request),
                worker,
            });
        Ok(())
    }

    /// Applies or discards a finished landmark preparation.
    pub(crate) fn finish_landmark_install(
        &mut self,
        request: &LandmarkInstallRequest,
        outcome: std::result::Result<Box<PreparedLandmarkContext>, String>,
    ) {
        let matched_bundle = self
            .landmark_resolution
            .as_ref()
            .filter(|resolution| resolution.status == rgv_core::AssemblyResolutionStatus::Matched)
            .and_then(|resolution| resolution.bundle_id.as_deref());
        if !landmark_install_is_current(
            request,
            self.landmark_install_ticket,
            matched_bundle,
            &self.session.assembly.dictionary,
        ) {
            tracing::debug!(
                bundle = %request.bundle_id,
                "discarding a landmark context prepared for a superseded assembly"
            );
            return;
        }
        let bundle_id = &request.bundle_id;
        if let Err(error) = outcome
            .map_err(anyhow::Error::msg)
            .and_then(|prepared| self.apply_prepared_landmark_context(*prepared))
        {
            let message =
                format!("Built-in landmark bundle {bundle_id} could not be installed: {error}");
            self.landmark_catalogue_error = Some(message.clone());
            self.report_error(message);
        }
    }

    fn apply_prepared_landmark_context(&mut self, prepared: PreparedLandmarkContext) -> Result<()> {
        let PreparedLandmarkContext {
            bundle,
            genes_path,
            cytobands_path,
            contig_bindings,
            cytobands,
            search_index,
            search_error,
        } = prepared;
        let bundle_id = bundle.id.as_str();

        let has_equivalent = self.session.tracks.tracks().iter().any(|track| {
            track.built_in.as_ref().is_some_and(|identity| {
                identity.bundle_id == bundle.id
                    && identity.bundle_version == bundle.version
                    && identity.asset_role == LandmarkAssetRole::RepresentativeGenes
            })
        });
        let has_explicit_gene_track = self
            .session
            .tracks
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::GeneModel && track.built_in.is_none());
        let suppressed = self
            .session
            .suppressed_builtin_gene_bundles
            .contains(bundle_id);
        if let Some(engine) = &self.session.track_engine {
            for track in self.session.tracks.tracks().iter().filter(|track| {
                track
                    .built_in
                    .as_ref()
                    .is_some_and(|identity| identity.bundle_id == bundle.id)
            }) {
                engine.try_set_query_contig_aliases(
                    track.id,
                    contig_bindings.query_contig_aliases.clone(),
                )?;
            }
        }
        let mut added_track = None;
        if !has_equivalent && !has_explicit_gene_track && !suppressed {
            let asset = bundle
                .asset(LandmarkAssetRole::RepresentativeGenes)
                .context("representative gene asset is absent")?;
            let next_id = self.allocate_track_id();
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
            // Representative models are the compact, symbol-bearing context
            // lane. Two expanded rows leave enough vertical space for the
            // adaptive label band; the all-transcript lane remains squished.
            config.feature.display_mode = FeatureDisplayMode::Expanded;
            config.height = 48.0;
            self.add_preconfigured_track_source_with_query_contig_aliases(
                &genes_path,
                TrackSpecKind::GenePred,
                config,
                contig_bindings.query_contig_aliases.clone(),
            )?;
            self.automation_auxiliary_open_state
                .insert(next_id, AutomationAuxiliaryOpenState::Pending);
            added_track = Some(next_id);
        }

        // Publish the already-verified compact context only after any required
        // track-engine insertion succeeds, so a frame cannot observe a partial
        // bundle installation.
        self.session.contig_aliases = contig_bindings.workspace_aliases;
        self.session.assembly.cytobands = cytobands;
        self.session.cytoband_path = Some(cytobands_path);
        self.session.assembly.default_annotation_count = 1;
        self.landmark_search_index = if has_equivalent || added_track.is_some() {
            search_index
        } else {
            None
        };
        self.refresh_active_panel_context();
        self.retry_imported_feature_search();
        if added_track.is_some() {
            self.request_all_panel_queries();
        } else {
            self.mark_display_dirty("explicit display invalidation");
        }
        self.landmark_catalogue_error = None;
        if added_track.is_some() {
            self.session.data_status = format!(
                "Loaded Genes for {} · {}",
                bundle.assembly.display_name, bundle.release
            );
        }
        if let Some(error) = search_error {
            self.report_landmark_search_error(bundle_id, &error);
        }
        Ok(())
    }

    fn report_landmark_search_error(&mut self, bundle_id: &str, error: &str) {
        let message = format!(
            "Gene-name search for landmark bundle {bundle_id} is unavailable: the index {error}"
        );
        self.landmark_catalogue_error = Some(message.clone());
        self.report_error(message);
    }

    pub(crate) fn merge_landmark_search_results(
        &self,
        query: &str,
        results: &mut TrackSearchResults,
    ) {
        let Some(index) = &self.landmark_search_index else {
            return;
        };
        let Some(track) = self.session.tracks.tracks().iter().find(|track| {
            track.built_in.as_ref().is_some_and(|identity| {
                identity.asset_role == LandmarkAssetRole::RepresentativeGenes
                    && !self
                        .session
                        .suppressed_builtin_gene_bundles
                        .contains(&identity.bundle_id)
            })
        }) else {
            return;
        };
        let normalized_query = query.trim().to_ascii_lowercase();
        for entry in index.search(query, 50) {
            let matched_term = LandmarkSearchIndex::matching_term(entry, query)
                .unwrap_or(&entry.display_symbol)
                .to_ascii_lowercase();
            if let Some(hit) = results
                .hits
                .iter_mut()
                .find(|hit| search_hit_matches_landmark_entry(hit, track.id, entry))
            {
                apply_landmark_feature_identity(&mut hit.record, entry);
                if hit.matched_term != normalized_query && matched_term == normalized_query {
                    hit.matched_term = matched_term;
                }
                continue;
            }
            results.hits.push(TrackSearchHit {
                track_id: track.id,
                track_name: track.name.clone(),
                matched_term,
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
            annotation_search_rank(left, &normalized_query)
                .cmp(&annotation_search_rank(right, &normalized_query))
                .then_with(|| left.matched_term.cmp(&right.matched_term))
                .then_with(|| left.record.contig.cmp(&right.record.contig))
                .then_with(|| left.record.start.cmp(&right.record.start))
                .then_with(|| left.record.end.cmp(&right.record.end))
                .then_with(|| left.track_id.cmp(&right.track_id))
        });
        results
            .hits
            .dedup_by(|left, right| search_hits_share_stable_feature_identity(left, right));
        results.hits.truncate(50);
    }

    pub(crate) fn restore_default_genes(&mut self) -> Result<()> {
        let bundle_id = self
            .landmark_resolution
            .as_ref()
            .filter(|resolution| resolution.status == rgv_core::AssemblyResolutionStatus::Matched)
            .and_then(|resolution| resolution.bundle_id.clone())
            .context("no confidently matched landmark bundle is available")?;
        self.session
            .suppressed_builtin_gene_bundles
            .remove(&bundle_id);
        self.install_builtin_landmark_context(&bundle_id)
    }

    pub(crate) fn show_all_transcripts(&mut self, bundle_id: &str) -> Result<()> {
        if self.session.tracks.tracks().iter().any(|track| {
            track.built_in.as_ref().is_some_and(|identity| {
                identity.bundle_id == bundle_id
                    && identity.asset_role == LandmarkAssetRole::DetailedTranscripts
            })
        }) {
            "All transcripts are already open".clone_into(&mut self.session.data_status);
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
        let aliases_path =
            catalogue.verify_asset(&root, bundle_id, LandmarkAssetRole::ContigAliases)?;
        let alias_text = std::fs::read_to_string(&aliases_path)
            .with_context(|| format!("failed to read {}", aliases_path.display()))?;
        let parsed_aliases = rgv_core::parse_landmark_aliases(&alias_text)?;
        let contig_bindings =
            landmark_contig_bindings(&bundle, &self.session.assembly.dictionary, &parsed_aliases)?;
        let (spec_kind, format) = match asset.format {
            rgv_core::LandmarkAssetFormat::BigGenePred => {
                (TrackSpecKind::BigGenePred, Some(TrackFormat::BigGenePred))
            }
            rgv_core::LandmarkAssetFormat::GenePredExtGzip => (TrackSpecKind::GenePred, None),
            _ => anyhow::bail!("detailed transcript asset has an incompatible format"),
        };
        let next_id = self.allocate_track_id();
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
        // The user asked for every transcript; one per gene would contradict the track's name.
        config.feature.transcripts = rgv_core::TranscriptDisplay::All;
        self.add_preconfigured_track_source_with_query_contig_aliases(
            &path,
            spec_kind,
            config,
            contig_bindings.query_contig_aliases,
        )?;
        self.automation_auxiliary_open_state
            .insert(next_id, AutomationAuxiliaryOpenState::Pending);
        "Opening all transcripts…".clone_into(&mut self.session.data_status);
        Ok(())
    }

    pub(crate) fn return_to_representative_genes(&mut self, bundle_id: &str) -> Result<()> {
        let detail_ids = self
            .session
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

    /// Reads the catalogue in a folder the user chose, off the event loop. It
    /// becomes the landmark folder setting once it has been read and validated.
    pub(crate) fn start_landmark_catalogue_load(&mut self, folder: PathBuf) {
        let label = folder.display().to_string();
        let spawned = std::thread::Builder::new()
            .name("rgv-landmark-catalogue".to_owned())
            .spawn(move || {
                let loaded = rgv_core::landmark_catalogue_directory(&folder)
                    .ok_or_else(|| {
                        format!(
                            "{} contains no catalog.json (nor igv/catalog.json)",
                            folder.display()
                        )
                    })
                    .and_then(|root| match crate::launch::read_landmark_catalogue(&root) {
                        (root, Some(catalogue), _) => Ok(LoadedLandmarkCatalogue {
                            folder,
                            root,
                            catalogue,
                        }),
                        (_, None, error) => Err(error.unwrap_or_else(|| {
                            "the landmark catalogue could not be read".to_owned()
                        })),
                    });
                Ok(
                    crate::workspace::genomes::GenomeTaskOutcome::LandmarkCatalogue(Box::new(
                        loaded,
                    )),
                )
            });
        match spawned {
            Ok(worker) => {
                self.set_busy_status(format!("Reading landmark catalogue in {label}…"));
                self.genome_tasks
                    .push(crate::workspace::genomes::GenomeTask {
                        label: format!("landmark folder {label}"),
                        is_list: false,
                        landmark: None,
                        worker,
                    });
            }
            Err(error) => {
                self.report_error(format!(
                    "Could not start reading the landmark folder: {error}"
                ));
            }
        }
        self.mark_display_dirty("explicit display invalidation");
    }

    /// Adopts a catalogue read by [`Self::start_landmark_catalogue_load`]:
    /// saves the folder setting and re-resolves the open reference against it,
    /// installing the matched bundle's Genes track and name search.
    pub(crate) fn finish_landmark_catalogue_load(
        &mut self,
        loaded: std::result::Result<LoadedLandmarkCatalogue, String>,
    ) {
        let loaded = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.report_error(format!("Landmark folder not used: {error}"));
                return;
            }
        };
        let had_context = self.landmark_resolution.as_ref().is_some_and(|resolution| {
            resolution.status == rgv_core::AssemblyResolutionStatus::Matched
                && resolution.bundle_id.is_some()
        });
        if had_context && let Err(error) = self.remove_installed_landmark_context(None) {
            self.report_error(format!(
                "Could not retire the previous landmark context: {error}"
            ));
            return;
        }
        // Any install still verifying the previous catalogue's assets is stale.
        self.landmark_install_ticket = self.landmark_install_ticket.saturating_add(1);
        self.landmark_resource_root = Some(loaded.root.clone());
        self.landmark_resource_source = Some(rgv_core::LandmarkRootSource::Setting);
        self.landmark_catalogue = Some(loaded.catalogue);
        self.landmark_catalogue_error = None;
        self.landmark_resolution = None;
        self.session.settings.landmark_resource_root = Some(loaded.folder);
        self.persist_settings();
        self.session.data_status = format!("Using landmark catalogue at {}", loaded.root.display());
        let dictionary = self.session.assembly.dictionary.clone();
        if !dictionary.is_empty() {
            self.update_landmark_resolution_from_dictionary(&dictionary);
        }
        self.mark_display_dirty("explicit display invalidation");
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
        self.session.data_status = format!("Opened landmark resources at {}", root.display());
        Ok(())
    }
}

/// The assembly name shown for a recognised reference, with the sequences
/// outside the build counted honestly: `GRCh38 (+ 98 extra contigs)`.
pub(crate) fn assembly_display_name(name: &str, extra_contigs: usize) -> String {
    match extra_contigs {
        0 => name.to_owned(),
        1 => format!("{name} (+ 1 extra contig)"),
        count => format!("{name} (+ {count} extra contigs)"),
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LandmarkStateInputs {
    pub(crate) resource_configured: bool,
    pub(crate) catalogue_loaded: bool,
    pub(crate) failed: bool,
    pub(crate) matched: bool,
    pub(crate) suppressed: bool,
    pub(crate) automatic_track_loaded: bool,
    pub(crate) automatic_track_visible: bool,
}

/// The automatic-track and integrity states reported for automation. No
/// configured catalogue is "unconfigured"/"not configured", never "failed":
/// failure is reserved for a catalogue, integrity or install error. An error
/// that left the Genes track loaded (for example a gene-name index that failed
/// verification) still reports integrity as failed, but not the track.
pub(crate) fn landmark_states(
    inputs: LandmarkStateInputs,
) -> (
    AutomationLandmarkTrackState,
    AutomationLandmarkIntegrityState,
) {
    let catalogue_ready = inputs.resource_configured && inputs.catalogue_loaded;
    let track = if inputs.failed && !inputs.automatic_track_loaded {
        AutomationLandmarkTrackState::Failed
    } else if inputs.suppressed {
        AutomationLandmarkTrackState::Suppressed
    } else if inputs.automatic_track_visible {
        AutomationLandmarkTrackState::Visible
    } else if inputs.automatic_track_loaded {
        AutomationLandmarkTrackState::Loaded
    } else if inputs.matched {
        AutomationLandmarkTrackState::Matched
    } else if catalogue_ready {
        AutomationLandmarkTrackState::Configured
    } else {
        AutomationLandmarkTrackState::Unconfigured
    };
    let integrity = if inputs.failed {
        AutomationLandmarkIntegrityState::Failed
    } else if !inputs.resource_configured {
        AutomationLandmarkIntegrityState::NotConfigured
    } else if inputs.matched {
        AutomationLandmarkIntegrityState::Verified
    } else if catalogue_ready {
        AutomationLandmarkIntegrityState::Unchecked
    } else {
        AutomationLandmarkIntegrityState::Unavailable
    };
    (track, integrity)
}

/// A landmark catalogue read from a folder the user chose.
#[derive(Debug)]
pub(crate) struct LoadedLandmarkCatalogue {
    /// The folder as chosen; persisted as the setting.
    folder: PathBuf,
    /// The directory holding `catalog.json` (the folder or its `igv` child).
    root: PathBuf,
    catalogue: GenomeLandmarkCatalogue,
}

/// Identifies what a landmark preparation was computed for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LandmarkInstallRequest {
    pub(crate) ticket: u64,
    pub(crate) bundle_id: String,
    /// The workspace dictionary the contig bindings and cytobands were bound to.
    pub(crate) dictionary: ReferenceDictionary,
}

/// Whether a finished preparation may still be applied. It is stale when a
/// later install was requested, when the workspace no longer matches that
/// bundle, or when the assembly dictionary it was bound to has been replaced:
/// its aliases and cytobands would then name another assembly's contigs.
pub(crate) fn landmark_install_is_current(
    request: &LandmarkInstallRequest,
    latest_ticket: u64,
    matched_bundle: Option<&str>,
    dictionary: &ReferenceDictionary,
) -> bool {
    request.ticket == latest_ticket
        && matched_bundle == Some(request.bundle_id.as_str())
        && *dictionary == request.dictionary
}

/// A verified, fully decoded landmark bundle, ready to publish.
#[derive(Debug)]
pub(crate) struct PreparedLandmarkContext {
    bundle: rgv_core::GenomeLandmarkBundle,
    genes_path: PathBuf,
    cytobands_path: PathBuf,
    contig_bindings: LandmarkContigBindings,
    cytobands: Vec<rgv_core::Cytoband>,
    search_index: Option<LandmarkSearchIndex>,
    /// Why the gene-name index is unavailable: its integrity check or decoding
    /// failed. Reported, never silently dropped.
    search_error: Option<String>,
}

/// The blocking half of a landmark install: integrity checks, file reads and
/// the gzip-compressed search index. Touches no workspace state.
fn prepare_landmark_context(
    root: &Path,
    catalogue: &GenomeLandmarkCatalogue,
    bundle_id: &str,
    dictionary: &ReferenceDictionary,
) -> Result<PreparedLandmarkContext> {
    let bundle = catalogue
        .bundle(bundle_id)
        .with_context(|| format!("landmark bundle {bundle_id} is not installed"))?
        .clone();

    // Verify the complete compact context before publishing any of it to
    // the workspace. The optional search index remains failure-isolated.
    let genes_path =
        catalogue.verify_asset(root, bundle_id, LandmarkAssetRole::RepresentativeGenes)?;
    let aliases_path = catalogue.verify_asset(root, bundle_id, LandmarkAssetRole::ContigAliases)?;
    let cytobands_path = catalogue.verify_asset(root, bundle_id, LandmarkAssetRole::Cytobands)?;

    let alias_text = std::fs::read_to_string(&aliases_path)
        .with_context(|| format!("failed to read {}", aliases_path.display()))?;
    let parsed_aliases = rgv_core::parse_landmark_aliases(&alias_text)?;
    let contig_bindings = landmark_contig_bindings(&bundle, dictionary, &parsed_aliases)?;
    let cytoband_text = std::fs::read_to_string(&cytobands_path)
        .with_context(|| format!("failed to read {}", cytobands_path.display()))?;
    let cytobands = rgv_core::parse_ucsc_cytobands_with_aliases(
        &cytoband_text,
        dictionary,
        &contig_bindings.workspace_aliases,
    )?;

    let (search_index, search_error) =
        match catalogue.verify_asset(root, bundle_id, LandmarkAssetRole::GeneSearch) {
            Ok(path) => match read_landmark_search(&path) {
                Ok(index) => (Some(index), None),
                Err(error) => (None, Some(format!("could not be read: {error:#}"))),
            },
            Err(error) => (
                None,
                Some(format!("failed integrity verification: {error}")),
            ),
        };
    if let Some(error) = &search_error {
        tracing::warn!(bundle = bundle_id, %error, "landmark search index unavailable");
    }
    Ok(PreparedLandmarkContext {
        bundle,
        genes_path,
        cytobands_path,
        contig_bindings,
        cytobands,
        search_index,
        search_error,
    })
}

#[derive(Debug, Default, Eq, PartialEq)]
struct LandmarkContigBindings {
    /// Source spellings and their synonyms resolved onto the workspace's
    /// canonical dictionary names. This drives search/navigation/cytobands.
    workspace_aliases: ContigAliases,
    /// Workspace canonical names translated back into the exact vocabulary
    /// used by the bundled annotation source. This drives track queries.
    query_contig_aliases: BTreeMap<String, String>,
}

fn landmark_contig_bindings(
    bundle: &rgv_core::GenomeLandmarkBundle,
    dictionary: &ReferenceDictionary,
    bundled_aliases: &ContigAliases,
) -> Result<LandmarkContigBindings> {
    let mut source_to_workspace = BTreeMap::new();
    let mut missing_nuclear_primary = Vec::new();

    for source_contig in &bundle.assembly.contigs {
        let exact = dictionary
            .id_by_name(&source_contig.name)
            .and_then(|id| dictionary.get(id))
            .filter(|contig| contig.length() == source_contig.length)
            .map(|contig| contig.name().to_owned());
        let workspace_contig = if let Some(exact) = exact {
            Some(exact)
        } else {
            let mut candidates = bundled_aliases
                .iter()
                .filter(|(_, canonical)| *canonical == &source_contig.name)
                .filter_map(|(alias, _)| {
                    dictionary
                        .id_by_name(alias)
                        .and_then(|id| dictionary.get(id))
                        .filter(|contig| contig.length() == source_contig.length)
                        .map(|contig| contig.name().to_owned())
                })
                .collect::<Vec<_>>();
            candidates.sort();
            candidates.dedup();
            anyhow::ensure!(
                candidates.len() <= 1,
                "landmark contig {} maps to multiple workspace sequences: {}",
                source_contig.name,
                candidates.join(", ")
            );
            candidates.pop()
        };

        let Some(workspace_contig) = workspace_contig else {
            if source_contig.primary && !is_mitochondrial_contig(&source_contig.name) {
                missing_nuclear_primary.push(source_contig.name.clone());
            }
            continue;
        };
        source_to_workspace.insert(source_contig.name.clone(), workspace_contig);
    }

    anyhow::ensure!(
        missing_nuclear_primary.is_empty(),
        "landmark bundle {} is missing compatible nuclear contigs: {}",
        bundle.id,
        missing_nuclear_primary.join(", ")
    );

    let mut bindings = LandmarkContigBindings::default();
    for (source, workspace) in &source_to_workspace {
        if source != workspace {
            insert_unique_contig_binding(
                &mut bindings.workspace_aliases,
                source,
                workspace,
                "workspace alias",
            )?;
            insert_unique_contig_binding(
                &mut bindings.query_contig_aliases,
                workspace,
                source,
                "track query alias",
            )?;
        }
    }
    for (alias, source) in bundled_aliases {
        let Some(workspace) = source_to_workspace.get(source) else {
            continue;
        };
        if alias == workspace {
            continue;
        }
        if let Some(id) = dictionary.id_by_name(alias) {
            let workspace_id = dictionary
                .id_by_name(workspace)
                .expect("a bound workspace contig came from the dictionary");
            if id != workspace_id {
                continue;
            }
        }
        insert_unique_contig_binding(
            &mut bindings.workspace_aliases,
            alias,
            workspace,
            "workspace alias",
        )?;
    }
    Ok(bindings)
}

fn insert_unique_contig_binding(
    bindings: &mut BTreeMap<String, String>,
    alias: &str,
    target: &str,
    label: &str,
) -> Result<()> {
    if let Some(previous) = bindings.insert(alias.to_owned(), target.to_owned()) {
        anyhow::ensure!(
            previous == target,
            "{label} {alias} targets both {previous} and {target}"
        );
    }
    Ok(())
}

fn is_mitochondrial_contig(name: &str) -> bool {
    matches!(
        name.strip_prefix("chr")
            .unwrap_or(name)
            .to_ascii_uppercase()
            .as_str(),
        "M" | "MT"
    )
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

fn set_feature_attribute(record: &mut rgv_hts::FeatureRecord, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some((_, existing)) = record
        .attributes
        .iter_mut()
        .find(|(existing, _)| existing == key)
    {
        value.clone_into(existing);
    } else {
        record.attributes.push((key.to_owned(), value.to_owned()));
    }
}

fn feature_attribute<'a>(record: &'a rgv_hts::FeatureRecord, key: &str) -> Option<&'a str> {
    record
        .attributes
        .iter()
        .find(|(candidate, value)| candidate == key && !value.is_empty())
        .map(|(_, value)| value.as_str())
}

fn apply_landmark_feature_identity(
    record: &mut rgv_hts::FeatureRecord,
    entry: &rgv_core::LandmarkSearchEntry,
) {
    record.name = Some(entry.display_symbol.clone());
    set_feature_attribute(record, "gene_id", &entry.gene_id);
    set_feature_attribute(record, "gene_name", &entry.display_symbol);
    set_feature_attribute(record, "transcript_id", &entry.transcript_id);
}

fn search_hit_matches_landmark_entry(
    hit: &TrackSearchHit,
    track_id: TrackId,
    entry: &rgv_core::LandmarkSearchEntry,
) -> bool {
    hit.track_id == track_id
        && hit.record.contig == entry.contig
        && hit.record.start == entry.start
        && hit.record.end == entry.end
        && feature_attribute(&hit.record, "transcript_id")
            .is_none_or(|transcript| transcript == entry.transcript_id)
}

fn search_hits_share_stable_feature_identity(
    left: &TrackSearchHit,
    right: &TrackSearchHit,
) -> bool {
    if left.track_id != right.track_id
        || left.record.contig != right.record.contig
        || left.record.start != right.record.start
        || left.record.end != right.record.end
    {
        return false;
    }
    match (
        feature_attribute(&left.record, "transcript_id"),
        feature_attribute(&right.record, "transcript_id"),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => left.record.display_label() == right.record.display_label(),
    }
}

fn annotation_search_rank(hit: &TrackSearchHit, normalized_query: &str) -> u8 {
    let identity = hit.record.presentation_identity();
    let display = identity
        .display_symbol
        .or(identity.record_name)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let gene_id = identity.gene_id.unwrap_or_default().to_ascii_lowercase();
    let transcript_id = identity
        .transcript_id
        .unwrap_or_default()
        .to_ascii_lowercase();
    if display == normalized_query {
        0
    } else if display.starts_with(normalized_query) {
        1
    } else if !hit.matched_term.eq_ignore_ascii_case(&gene_id)
        && !hit.matched_term.eq_ignore_ascii_case(&transcript_id)
        && hit.matched_term.eq_ignore_ascii_case(normalized_query)
    {
        2
    } else if !hit.matched_term.eq_ignore_ascii_case(&gene_id)
        && !hit.matched_term.eq_ignore_ascii_case(&transcript_id)
        && hit.matched_term.starts_with(normalized_query)
    {
        3
    } else if gene_id == normalized_query {
        4
    } else if gene_id.starts_with(normalized_query) {
        5
    } else if transcript_id == normalized_query {
        6
    } else if transcript_id.starts_with(normalized_query) {
        7
    } else {
        8
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

#[cfg(test)]
mod tests {
    #[test]
    fn landmark_preparations_for_a_superseded_assembly_are_discarded() {
        let grch38 = ReferenceDictionary::from_contigs([("chr1".to_owned(), 248_956_422)]);
        let grch37 = ReferenceDictionary::from_contigs([("1".to_owned(), 249_250_621)]);
        let request = LandmarkInstallRequest {
            ticket: 3,
            bundle_id: "grch38-genes".to_owned(),
            dictionary: grch38.clone(),
        };
        assert!(landmark_install_is_current(
            &request,
            3,
            Some("grch38-genes"),
            &grch38
        ));
        // The assembly changed while the bundle was being verified: its
        // aliases and cytobands are bound to the old dictionary.
        assert!(!landmark_install_is_current(
            &request,
            3,
            Some("grch38-genes"),
            &grch37
        ));
        // The workspace now matches another bundle, or none.
        assert!(!landmark_install_is_current(
            &request,
            3,
            Some("grch37-genes"),
            &grch38
        ));
        assert!(!landmark_install_is_current(&request, 3, None, &grch38));
        // A later request for the same bundle supersedes this one, so two
        // completions cannot both add a Genes track.
        assert!(!landmark_install_is_current(
            &request,
            4,
            Some("grch38-genes"),
            &grch38
        ));
    }

    use super::*;

    fn raw_hit() -> TrackSearchHit {
        TrackSearchHit {
            track_id: TrackId::new(7),
            track_name: "Genes".to_owned(),
            matched_term: "actb".to_owned(),
            record: rgv_hts::FeatureRecord {
                contig: "chr7".to_owned(),
                start: 100,
                end: 200,
                name: Some("ENST000001".to_owned()),
                feature_type: Some("transcript".to_owned()),
                score: None,
                strand: rgv_hts::Strand::Forward,
                blocks: vec![(100, 200)],
                coding_interval: None,
                peak: None,
                attributes: vec![("transcript_id".to_owned(), "ENST000001".to_owned())],
            },
        }
    }

    fn entry() -> rgv_core::LandmarkSearchEntry {
        rgv_core::LandmarkSearchEntry {
            normalized_term: "actb".to_owned(),
            display_symbol: "ACTB".to_owned(),
            gene_id: "ENSG000001".to_owned(),
            transcript_id: "ENST000001".to_owned(),
            contig: "chr7".to_owned(),
            start: 100,
            end: 200,
            strand: '+',
            synonyms: Vec::new(),
        }
    }

    #[test]
    fn duplicate_raw_landmark_hit_is_upgraded_to_symbol_identity() {
        let mut hit = raw_hit();
        let entry = entry();
        assert!(search_hit_matches_landmark_entry(
            &hit,
            TrackId::new(7),
            &entry
        ));
        apply_landmark_feature_identity(&mut hit.record, &entry);
        assert_eq!(hit.record.display_label(), Some("ACTB"));
        assert_eq!(
            feature_attribute(&hit.record, "gene_id"),
            Some("ENSG000001")
        );
        assert_eq!(annotation_search_rank(&hit, "actb"), 0);
        assert_eq!(annotation_search_rank(&hit, "act"), 1);
        hit.matched_term = "ensg000001".to_owned();
        assert_eq!(annotation_search_rank(&hit, "ensg000001"), 4);
        assert_eq!(annotation_search_rank(&hit, "ensg"), 5);
    }

    #[test]
    fn landmark_contigs_bind_ucsc_sources_to_b37_workspace_names() {
        let bundle = rgv_core::GenomeLandmarkBundle {
            id: "hg19".to_owned(),
            version: "1".to_owned(),
            display_name: "Genes".to_owned(),
            provider: "fixture".to_owned(),
            release: "fixture".to_owned(),
            selection_policy: "fixture".to_owned(),
            assembly: rgv_core::LandmarkAssemblyIdentity {
                stable_id: "GRCh37.p13".to_owned(),
                display_name: "GRCh37".to_owned(),
                compatible_ids: vec!["GRCh37".to_owned()],
                primary_contig_count: 3,
                dictionary_sha256: "a".repeat(64),
                compatible_dictionary_sha256: Vec::new(),
                contigs: vec![
                    rgv_core::LandmarkContigFingerprint {
                        name: "chr1".to_owned(),
                        length: 100,
                        md5: None,
                        primary: true,
                    },
                    rgv_core::LandmarkContigFingerprint {
                        name: "chr2".to_owned(),
                        length: 200,
                        md5: None,
                        primary: true,
                    },
                    rgv_core::LandmarkContigFingerprint {
                        name: "chrM".to_owned(),
                        length: 50,
                        md5: None,
                        primary: true,
                    },
                ],
            },
            assets: Vec::new(),
        };
        let dictionary = ReferenceDictionary::from_contigs([
            ("1".to_owned(), 100),
            ("2".to_owned(), 200),
            // The b37 and UCSC mitochondrial sequences are known variants;
            // an unequal sequence must not receive a coordinate alias.
            ("MT".to_owned(), 49),
            ("decoy".to_owned(), 1_000),
        ]);
        let aliases = ContigAliases::from([
            ("1".to_owned(), "chr1".to_owned()),
            ("CM000001.1".to_owned(), "chr1".to_owned()),
            ("2".to_owned(), "chr2".to_owned()),
            ("MT".to_owned(), "chrM".to_owned()),
        ]);

        let bindings = landmark_contig_bindings(&bundle, &dictionary, &aliases).unwrap();

        assert_eq!(
            bindings.query_contig_aliases,
            BTreeMap::from([
                ("1".to_owned(), "chr1".to_owned()),
                ("2".to_owned(), "chr2".to_owned()),
            ])
        );
        assert_eq!(
            bindings.workspace_aliases.get("chr1").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            bindings
                .workspace_aliases
                .get("CM000001.1")
                .map(String::as_str),
            Some("1")
        );
        assert!(!bindings.workspace_aliases.contains_key("chrM"));
        assert!(!bindings.query_contig_aliases.contains_key("MT"));
    }

    #[test]
    fn no_configured_catalogue_is_reported_as_not_configured_rather_than_failed() {
        let (track, integrity) = landmark_states(LandmarkStateInputs::default());
        assert_eq!(track, AutomationLandmarkTrackState::Unconfigured);
        assert_eq!(integrity, AutomationLandmarkIntegrityState::NotConfigured);

        // A real failure (invalid catalogue, unusable configured folder,
        // asset integrity) is still a failure.
        let (track, integrity) = landmark_states(LandmarkStateInputs {
            failed: true,
            ..LandmarkStateInputs::default()
        });
        assert_eq!(track, AutomationLandmarkTrackState::Failed);
        assert_eq!(integrity, AutomationLandmarkIntegrityState::Failed);

        let configured = LandmarkStateInputs {
            resource_configured: true,
            catalogue_loaded: true,
            ..LandmarkStateInputs::default()
        };
        assert_eq!(
            landmark_states(configured),
            (
                AutomationLandmarkTrackState::Configured,
                AutomationLandmarkIntegrityState::Unchecked
            )
        );
        assert_eq!(
            landmark_states(LandmarkStateInputs {
                matched: true,
                automatic_track_loaded: true,
                automatic_track_visible: true,
                ..configured
            }),
            (
                AutomationLandmarkTrackState::Visible,
                AutomationLandmarkIntegrityState::Verified
            )
        );
        // A gene-name index that failed verification leaves the Genes track
        // in place but integrity reported as failed.
        assert_eq!(
            landmark_states(LandmarkStateInputs {
                matched: true,
                failed: true,
                automatic_track_loaded: true,
                ..configured
            }),
            (
                AutomationLandmarkTrackState::Loaded,
                AutomationLandmarkIntegrityState::Failed
            )
        );
    }

    #[test]
    fn recognised_assemblies_name_their_extra_contigs() {
        assert_eq!(assembly_display_name("GRCh38", 0), "GRCh38");
        assert_eq!(
            assembly_display_name("GRCh38", 1),
            "GRCh38 (+ 1 extra contig)"
        );
        assert_eq!(
            assembly_display_name("GRCh38", 98),
            "GRCh38 (+ 98 extra contigs)"
        );
    }

    fn gzipped(text: &str) -> Vec<u8> {
        use flate2::{Compression, write::GzEncoder};
        use std::io::Write as _;

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap()
    }

    /// A one-chromosome bundle whose assets are written to `root` with their
    /// real sizes and SHA-256 digests.
    fn written_bundle(root: &Path) -> GenomeLandmarkCatalogue {
        written_bundle_with(
            root,
            b"genes".to_vec(),
            "rgv_gene_names_v1\nactb\tACTB\tENSG1\tNM_1\tchr1\t10\t20\t+\t\n",
        )
    }

    fn written_bundle_with(root: &Path, genes: Vec<u8>, names: &str) -> GenomeLandmarkCatalogue {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;

        let files: [(
            LandmarkAssetRole,
            rgv_core::LandmarkAssetFormat,
            &str,
            Vec<u8>,
        ); 4] = [
            (
                LandmarkAssetRole::RepresentativeGenes,
                rgv_core::LandmarkAssetFormat::GenePredExtGzip,
                "fixture/genes.genePred.gz",
                genes,
            ),
            (
                LandmarkAssetRole::ContigAliases,
                rgv_core::LandmarkAssetFormat::AliasTsv,
                "fixture/aliases.tsv",
                b"1\tchr1\n".to_vec(),
            ),
            (
                LandmarkAssetRole::Cytobands,
                rgv_core::LandmarkAssetFormat::UcscCytoband,
                "fixture/cytobands.txt",
                b"chr1\t0\t100\tp1\tgneg\n".to_vec(),
            ),
            (
                LandmarkAssetRole::GeneSearch,
                rgv_core::LandmarkAssetFormat::RgvGeneNamesV1Gzip,
                "fixture/names.tsv.gz",
                gzipped(names),
            ),
        ];
        std::fs::create_dir_all(root.join("fixture")).unwrap();
        let assets = files
            .into_iter()
            .map(|(role, format, path, bytes)| {
                std::fs::write(root.join(path), &bytes).unwrap();
                rgv_core::GenomeLandmarkAsset {
                    role,
                    path: path.to_owned(),
                    format,
                    bytes: bytes.len() as u64,
                    sha256: Sha256::digest(&bytes)
                        .iter()
                        .fold(String::new(), |mut hex, byte| {
                            let _ = write!(hex, "{byte:02x}");
                            hex
                        }),
                    source_url: "https://example.test".to_owned(),
                    upstream_project: "fixture".to_owned(),
                    upstream_version: "1".to_owned(),
                    derivation: "fixture".to_owned(),
                    citation: "fixture".to_owned(),
                    license: "fixture".to_owned(),
                    license_url: "https://example.test".to_owned(),
                    required_notice: String::new(),
                }
            })
            .collect();
        GenomeLandmarkCatalogue {
            schema_version: rgv_core::LANDMARK_CATALOGUE_SCHEMA_VERSION,
            bundles: vec![rgv_core::GenomeLandmarkBundle {
                id: "fixture".to_owned(),
                version: "1".to_owned(),
                display_name: "Genes".to_owned(),
                provider: "fixture".to_owned(),
                release: "fixture".to_owned(),
                selection_policy: "fixture".to_owned(),
                assembly: rgv_core::LandmarkAssemblyIdentity {
                    stable_id: "FIXTURE.1".to_owned(),
                    display_name: "Fixture".to_owned(),
                    compatible_ids: Vec::new(),
                    primary_contig_count: 1,
                    dictionary_sha256: "a".repeat(64),
                    compatible_dictionary_sha256: Vec::new(),
                    contigs: vec![rgv_core::LandmarkContigFingerprint {
                        name: "chr1".to_owned(),
                        length: 100,
                        md5: None,
                        primary: true,
                    }],
                },
                assets,
            }],
        }
    }

    #[test]
    fn landmark_integrity_failures_are_reported_not_silently_dropped() {
        let root = tempfile::tempdir().unwrap();
        let catalogue = written_bundle(root.path());
        let dictionary =
            ReferenceDictionary::from_contigs([("chr1".to_owned(), 100), ("ERCC".to_owned(), 9)]);

        let prepared =
            prepare_landmark_context(root.path(), &catalogue, "fixture", &dictionary).unwrap();
        assert!(prepared.search_error.is_none());
        let index = prepared
            .search_index
            .expect("verified names are searchable");
        assert_eq!(index.search("ACT", 5)[0].display_symbol, "ACTB");

        // A damaged gene-name index keeps the verified Genes track but says why
        // search is unavailable.
        std::fs::write(root.path().join("fixture/names.tsv.gz"), b"tampered").unwrap();
        let prepared =
            prepare_landmark_context(root.path(), &catalogue, "fixture", &dictionary).unwrap();
        assert!(prepared.search_index.is_none());
        assert!(
            prepared
                .search_error
                .as_deref()
                .is_some_and(|error| error.contains("failed integrity verification")),
            "{:?}",
            prepared.search_error
        );

        // A damaged gene model refuses the whole context.
        std::fs::write(root.path().join("fixture/genes.genePred.gz"), b"GENES").unwrap();
        let error = prepare_landmark_context(root.path(), &catalogue, "fixture", &dictionary)
            .unwrap_err()
            .to_string();
        assert!(error.contains("SHA-256"), "{error}");
    }

    /// The curated bundles list every gene of IGV's default `RefSeq` track, not
    /// only MANE: a gene outside MANE (PBRM1 here) is searchable, and every
    /// hit names a model the Genes track draws.
    #[test]
    fn genes_outside_mane_are_searchable_and_drawn() {
        let genes = [
            "NM_001101.5\tchr1\t-\t5\t30\t6\t29\t1\t5,\t30,\t0\tACTB\tcmpl\tcmpl\t0,",
            "NM_001297719.2\tchr1\t+\t32\t40\t32\t40\t1\t32,\t40,\t0\tBMAL1\tcmpl\tcmpl\t0,",
            "NM_001405600.1\tchr1\t-\t45\t90\t50\t80\t2\t45,70,\t60,90,\t0\tPBRM1\tcmpl\tcmpl\t0,0,",
        ];
        let names = "rgv_gene_names_v1\n\
            actb\tACTB\tENSG00000075624.17\tNM_001101.5\tchr1\t5\t30\t-\tENST00000646664.1\n\
            bmal1\tBMAL1\tENSG00000133794.20\tNM_001297719.2\tchr1\t32\t40\t+\tARNTL,ENST00000403290.6\n\
            pbrm1\tPBRM1\tPBRM1\tNM_001405600.1\tchr1\t45\t90\t-\t\n";
        let root = tempfile::tempdir().unwrap();
        let catalogue =
            written_bundle_with(root.path(), gzipped(&(genes.join("\n") + "\n")), names);
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 100)]);

        let prepared =
            prepare_landmark_context(root.path(), &catalogue, "fixture", &dictionary).unwrap();
        assert_eq!(prepared.search_error, None);
        let index = prepared
            .search_index
            .expect("verified names are searchable");

        let drawn = {
            let mut text = String::new();
            MultiGzDecoder::new(std::fs::File::open(&prepared.genes_path).unwrap())
                .read_to_string(&mut text)
                .unwrap();
            text
        };
        let lands_on_drawn_model = |entry: &rgv_core::LandmarkSearchEntry| {
            drawn.lines().any(|line| {
                let fields = line.split('\t').collect::<Vec<_>>();
                fields[0] == entry.transcript_id
                    && fields[1] == entry.contig
                    && fields[3] == entry.start.to_string()
                    && fields[4] == entry.end.to_string()
                    && fields[11] == entry.display_symbol
            })
        };

        let pbrm1 = index.search("pbrm1", 5);
        assert_eq!(pbrm1.len(), 1);
        assert_eq!(
            (
                pbrm1[0].display_symbol.as_str(),
                pbrm1[0].gene_id.as_str(),
                pbrm1[0].contig.as_str(),
                pbrm1[0].start,
                pbrm1[0].end
            ),
            ("PBRM1", "PBRM1", "chr1", 45, 90)
        );
        // A former MANE symbol and the MANE Ensembl transcript still find
        // their gene, and every hit is a model the Genes track draws.
        assert_eq!(index.search("ARNTL", 5)[0].display_symbol, "BMAL1");
        assert_eq!(index.search("ENST00000646664", 5)[0].display_symbol, "ACTB");
        for query in ["ACTB", "BMAL1", "PBRM1", "ARNTL", "ENSG00000075624"] {
            let hits = index.search(query, 5);
            assert!(!hits.is_empty(), "{query}");
            assert!(hits.iter().all(|hit| lands_on_drawn_model(hit)), "{query}");
        }
    }
}
