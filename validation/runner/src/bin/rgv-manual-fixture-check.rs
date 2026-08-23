use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use rgv_validation_runner::{
    DEFAULT_SUITE_ROOT, DEFAULT_VALIDATION_ROOT, summarize, verify_manual_suite, workspace_root,
};

#[derive(Debug, Parser)]
#[command(about = "Verify the deterministic RGV manual fixture suite")]
struct Arguments {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(long, default_value = DEFAULT_SUITE_ROOT)]
    suite_root: PathBuf,
    #[arg(long, default_value = DEFAULT_VALIDATION_ROOT)]
    validation_root: PathBuf,
}

#[derive(Debug, Subcommand)]
enum Command {
    Verify {
        /// Reject coverage entries that have not passed their semantic close gate.
        #[arg(long)]
        require_complete: bool,
    },
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let root = workspace_root();
    let Command::Verify { require_complete } = arguments.command.unwrap_or(Command::Verify {
        require_complete: false,
    });
    let summary = verify_manual_suite(
        &root,
        &root.join(arguments.suite_root).join("manifest.json"),
        &root
            .join(arguments.validation_root)
            .join("coverage-catalog.json"),
        require_complete,
    )?;
    println!("{}", summarize(&summary));
    if !summary.incomplete_coverage_entries.is_empty() {
        println!(
            "planned coverage: {}",
            summary.incomplete_coverage_entries.join(", ")
        );
    }
    Ok(())
}
