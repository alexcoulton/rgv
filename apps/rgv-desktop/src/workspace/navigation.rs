//! Locus resolution lives in `rgv-session`; this module keeps the launch-option glue.

pub(crate) use rgv_session::navigation::*;

use crate::launch::LaunchOptions;
use anyhow::Result;
use rgv_core::ContigAliases;

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
