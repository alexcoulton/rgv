//! How the session opens sources: the remote transfer configuration shared by every reader,
//! and the open options of alignment and reference sources.

use anyhow::Result;
use rgv_core::{
    SessionAlignmentTrack, SessionDocument, TrackConfig, TrackFormat, TrackId, TrackSource,
};
use rgv_engine::{TrackEngine, TrackSpec, TrackSpecKind};
use rgv_hts::{AlignmentOpenOptions, FastaOpenOptions, HttpRangeConfig, RemoteAuthentication};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

pub fn primary_alignment_track(session: &SessionDocument) -> Option<&SessionAlignmentTrack> {
    session.alignment_tracks.first()
}

pub fn alignment_open_options(
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

pub fn reference_open_options(remote_index_url: Option<String>) -> FastaOpenOptions {
    FastaOpenOptions {
        remote_index_url,
        remote_range: remote_range_config(),
        ..FastaOpenOptions::default()
    }
}

/// Starts the annotation-track engine with the desktop's remote configuration, so remote
/// bigWig, bigBed, TDF and text tracks get the same bearer authentication, cancellation and
/// budgets as remote alignments instead of anonymous defaults.
pub fn spawn_track_engine(
    specifications: Vec<(TrackSpec, BTreeMap<String, String>)>,
) -> TrackEngine {
    TrackEngine::spawn_with_remote_config(specifications, 8, remote_range_config())
}

/// [`spawn_track_engine`] for tracks that need no query-contig aliases.
pub fn spawn_track_engine_without_aliases(specifications: Vec<TrackSpec>) -> TrackEngine {
    spawn_track_engine(
        specifications
            .into_iter()
            .map(|specification| (specification, BTreeMap::new()))
            .collect(),
    )
}

pub fn remote_range_config() -> HttpRangeConfig {
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

/// The path or URL a source is opened from.
pub fn source_path(source: &TrackSource) -> PathBuf {
    match source {
        TrackSource::Local { path } => PathBuf::from(path),
        TrackSource::Remote { url } => PathBuf::from(url),
    }
}

pub fn tdf_channel_display_name(base_name: &str, embedded_name: &str, index: usize) -> String {
    let embedded_name = embedded_name.trim();
    if embedded_name.is_empty() {
        format!("{base_name} · channel {}", index + 1)
    } else {
        embedded_name.to_owned()
    }
}

pub struct TdfChannelExpansion {
    pub selected_name: String,
    pub specifications: Vec<TrackSpec>,
    pub children: Vec<TrackConfig>,
}

pub fn build_tdf_channel_expansion(
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
