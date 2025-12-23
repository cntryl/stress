use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use cntryl_stress::{run_with_options, StressRunnerOptions};

#[derive(Debug, Parser)]
#[command(
    name = "cargo-stress",
    bin_name = "cargo",
    about = "Run stress benchmarks via `cargo stress`"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run stress benchmarks
    #[command(name = "stress")]
    Stress(StressArgs),
}

#[derive(Debug, Parser)]
struct StressArgs {
    /// Filter benchmarks by glob pattern (e.g., "database*", "*insert*")
    #[arg(long)]
    workload: Option<String>,

    /// Number of measurement runs (reports median)
    #[arg(long, default_value_t = 1)]
    runs: usize,

    /// Number of warmup runs (discarded)
    #[arg(long, default_value_t = 0)]
    warmup: usize,

    /// Verbose output
    #[arg(long, short = 'v')]
    verbose: bool,

    /// Quiet mode (minimal output)
    #[arg(long, short = 'q')]
    quiet: bool,

    /// Include ignored benchmarks
    #[arg(long)]
    include_ignored: bool,

    /// Output directory for JSON results
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Baseline JSON file for regression comparison
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Regression threshold percentage (default: 5%)
    #[arg(long, default_value_t = 0.05)]
    threshold: f64,

    /// List all registered benchmarks without running them
    #[arg(long)]
    list: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Commands::Stress(args) => {
            if args.list {
                let benchmarks = cntryl_stress::list_benchmarks();
                if benchmarks.is_empty() {
                    println!("No benchmarks registered.");
                    println!("Add #[stress_test] to your benchmark functions.");
                } else {
                    println!("Registered benchmarks ({}):", benchmarks.len());
                    for name in benchmarks {
                        println!("  {}", name);
                    }
                }
                return Ok(());
            }

            let verbose = if args.quiet {
                false
            } else {
                args.verbose || !args.quiet
            };

            let mut opts = StressRunnerOptions::new()
                .runs(args.runs)
                .warmup(args.warmup)
                .verbose(verbose)
                .include_ignored(args.include_ignored)
                .threshold(args.threshold);

            if let Some(pattern) = args.workload {
                opts = opts.workload(pattern);
            }

            if let Some(baseline) = args.baseline {
                opts = opts.baseline(baseline);
            }

            run_with_options(opts);
        }
    }

    Ok(())
}
