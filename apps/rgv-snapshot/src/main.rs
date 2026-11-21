//! `rgv-snapshot`: render an RGV view to PNG or SVG with no window and no GPU.

use anyhow::{Result, bail};
use clap::{Parser, ValueEnum};
use rgv_snapshot::{SnapshotFormat, SnapshotRequest, snapshot};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Format {
    Png,
    Svg,
}

/// Render a genomic view headlessly: no window, no GPU device.
#[derive(Debug, Parser)]
#[command(name = "rgv-snapshot", version)]
struct Arguments {
    /// An RGV session file to open (its reference, tracks and locus).
    #[arg(long)]
    session: Option<PathBuf>,
    /// Reference FASTA (indexed).
    #[arg(long)]
    reference: Option<PathBuf>,
    /// A track to add: BAM/CRAM alignments or an annotation file (BED, VCF, GFF, ...).
    /// Repeat for several tracks.
    #[arg(long = "track")]
    tracks: Vec<PathBuf>,
    /// Locus to show, for example chr1:900001-920000.
    #[arg(long)]
    locus: Option<String>,
    /// Canvas width in logical points.
    #[arg(long, default_value_t = 1200)]
    width: u32,
    /// Canvas height in logical points.
    #[arg(long, default_value_t = 800)]
    height: u32,
    /// Pixels per point for PNG output.
    #[arg(long, default_value_t = 1.0)]
    scale: f32,
    /// Output format; inferred from the output extension when omitted.
    #[arg(long, value_enum)]
    format: Option<Format>,
    /// Seconds to wait for every panel to settle before failing.
    #[arg(long, default_value_t = 60.0)]
    timeout: f64,
    /// Output image path.
    #[arg(long)]
    out: PathBuf,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let format = match arguments.format {
        Some(Format::Png) => SnapshotFormat::Png,
        Some(Format::Svg) => SnapshotFormat::Svg,
        None => match arguments
            .out
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("svg") => SnapshotFormat::Svg,
            Some("png") => SnapshotFormat::Png,
            _ => bail!("pass --format png|svg or an output ending in .png or .svg"),
        },
    };
    if arguments.session.is_none() && arguments.tracks.is_empty() && arguments.reference.is_none() {
        bail!("nothing to show: pass --session, or --reference and/or --track");
    }
    if !arguments.timeout.is_finite() || arguments.timeout <= 0.0 {
        bail!("--timeout must be a positive number of seconds");
    }
    let request = SnapshotRequest {
        session_file: arguments.session,
        reference: arguments.reference,
        tracks: arguments.tracks,
        locus: arguments.locus,
        width: arguments.width,
        height: arguments.height,
    };
    let host = snapshot(
        &request,
        format,
        arguments.scale,
        &arguments.out,
        Duration::from_secs_f64(arguments.timeout),
    )?;
    let status = &host.session.data_status;
    println!(
        "wrote {}{}",
        arguments.out.display(),
        if status.is_empty() {
            String::new()
        } else {
            format!(" ({status})")
        }
    );
    Ok(())
}
