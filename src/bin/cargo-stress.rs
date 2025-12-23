use clap::{Parser, Subcommand};
use anyhow::Result;
use std::path::PathBuf;

use cntryl_stress::{BenchRunner, BenchRunnerConfig, register_benchmarks};

#[derive(Debug, Parser)]
#[command(name = "cargo-stress", about = "Run cntryl-stress benchmarks via `cargo stress`")]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run registered benchmarks
    Run {
        /// Suite name
        #[arg(long, default_value = "my_suite")]
        suite: String,

        /// Number of measurement runs
        #[arg(long)]
        runs: Option<usize>,

        /// Number of warmup runs
        #[arg(long)]
        warmup: Option<usize>,

        /// Verbose output
        #[arg(long, default_value_t = false)]
        verbose: bool,

        /// Output directory for JSON results
        #[arg(long)]
        output_dir: Option<PathBuf>,

        /// Baseline JSON path for comparison
        #[arg(long)]
        baseline: Option<PathBuf>,

        /// Regression threshold (e.g., 0.05 for 5%)
        #[arg(long, default_value_t = 0.05)]
        threshold: f64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Commands::Run { suite, runs, warmup, verbose, output_dir, baseline, threshold } => {
            let mut cfg = BenchRunnerConfig::from_env();
            if let Some(r) = runs { cfg.runs = r; }
            if let Some(w) = warmup { cfg.warmup_runs = w; }
            if let Some(dir) = output_dir { cfg.output_dir = dir; }
            cfg.verbose = verbose;

            let mut runner = BenchRunner::with_config(&suite, cfg);

            // Register benchmarks (users can edit `src/benches/mod.rs` or call their own)
            register_benchmarks(&mut runner);

            if let Some(path) = baseline {
                let (_results, regressions) = runner.finish_with_baseline(path, threshold);
                if !regressions.is_empty() {
                    eprintln!("{} regressions found", regressions.len());
                    std::process::exit(1);
                }
            } else {
                let _results = runner.finish();
            }
        }
    }

    Ok(())
}
