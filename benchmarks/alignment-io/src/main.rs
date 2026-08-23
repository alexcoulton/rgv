use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(about = "Reproducible alignment I/O benchmark for RGV")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate the deterministic mixed-alignment BAM corpus and BAI.
    Generate {
        #[arg(default_value = "benchmark-data/mixed.bam")]
        output: PathBuf,
    },
    /// Generate a small visual truth set containing SNVs and small/large indels.
    GenerateVariants {
        #[arg(default_value = "benchmark-data/variants.bam")]
        output: PathBuf,
        #[arg(long, default_value = "benchmark-data/variants.fa")]
        reference: PathBuf,
    },
    /// Run all backend workloads and verify that their checksums agree.
    Run {
        #[arg(default_value = "benchmark-data/mixed.bam")]
        input: PathBuf,
        #[arg(long, default_value_t = 5)]
        iterations: usize,
        /// Artificial latency applied to each HTTP request in the remote benchmark.
        #[arg(long, default_value_t = 2)]
        remote_latency_ms: u64,
        #[arg(long, default_value = "benchmark-results/latest.json")]
        output: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Generate { output } => generate::run(&output),
        Command::GenerateVariants { output, reference } => {
            generate::run_variants(&output, &reference)
        }
        Command::Run {
            input,
            iterations,
            remote_latency_ms,
            output,
        } => runner::run(&input, iterations, remote_latency_ms, &output),
    }
}

mod generate;
mod remote;
mod runner;
