//! cargo-stress: Cargo subcommand for running system-level stress tests.
//!
//! ## Design Philosophy
//!
//! This tool follows the same model as `cargo test`:
//!
//! 1. **No runtime magic**: Stress tests are compiled as normal Rust binaries
//! 2. **Cargo does the building**: We invoke `cargo build --release` for each binary
//! 3. **Orchestration only**: cargo-stress discovers files, invokes Cargo, runs binaries
//! 4. **Always optimized**: Stress tests run in release mode by default
//!
//! ## Why This Model?
//!
//! - Deterministic: Same binary every time
//! - Debuggable: Standard Rust compilation, no proc-macro trickery at runtime
//! - CI-friendly: Clear pass/fail semantics with proper exit codes
//! - Mirrors user expectations from `cargo test`
//!
//! ## Expected Project Structure
//!
//! ```text
//! my-project/
//!   Cargo.toml          # Must declare [[bin]] targets for stress files
//!   src/lib.rs
//!   stress/
//!     fsync.rs          # -> binary: stress_fsync
//!     compaction.rs     # -> binary: stress_compaction
//!     recovery.rs       # -> binary: stress_recovery
//! ```
//!
//! Each stress file must:
//! - Contain functions annotated with `#[stress_test]`
//! - End with `cntryl_stress::stress_main!()`

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

// ============================================================================
// CLI Definition
// ============================================================================

#[derive(Debug, Parser)]
#[command(
    name = "cargo-stress",
    bin_name = "cargo",
    about = "Run stress benchmarks via `cargo stress`",
    long_about = "
cargo-stress is a Cargo subcommand for running system-level stress tests.

Each .rs file in the stress/ directory is compiled as a separate binary
in release mode. Tests are defined with #[stress_test] and discovered
automatically at runtime by each binary.

Example:
    cargo stress                        # Run all stress tests
    cargo stress --workload 'fsync*'    # Filter by pattern
    cargo stress --runs 5               # Multiple measurement runs
    cargo stress --list                 # List available tests
"
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
    // ========================================================================
    // Test Selection
    // ========================================================================
    /// Filter benchmarks by glob pattern (e.g., "database*", "*insert*")
    /// Passed through to each stress binary.
    #[arg(long)]
    workload: Option<String>,

    /// Include ignored benchmarks (those marked with #[stress_test(ignore)])
    #[arg(long)]
    include_ignored: bool,

    /// List all registered benchmarks without running them
    #[arg(long)]
    list: bool,

    /// Run only a specific stress binary (filename without .rs extension)
    #[arg(long)]
    bin: Option<String>,

    // ========================================================================
    // Execution Options
    // ========================================================================
    /// Number of measurement runs per benchmark (reports median)
    #[arg(long, default_value_t = 1)]
    runs: usize,

    /// Number of warmup runs (discarded, not reported)
    #[arg(long, default_value_t = 0)]
    warmup: usize,

    // ========================================================================
    // Output Control
    // ========================================================================
    /// Verbose output
    #[arg(long, short = 'v')]
    verbose: bool,

    /// Quiet mode (minimal output, only errors)
    #[arg(long, short = 'q')]
    quiet: bool,

    /// Output directory for JSON results
    #[arg(long)]
    output_dir: Option<PathBuf>,

    // ========================================================================
    // Regression Detection
    // ========================================================================
    /// Baseline JSON file for regression comparison
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Regression threshold percentage (default: 5%)
    #[arg(long, default_value_t = 0.05)]
    threshold: f64,

    // ========================================================================
    // Build Options
    // ========================================================================
    /// Run in debug mode instead of release mode (not recommended for benchmarks)
    #[arg(long)]
    dev: bool,

    /// Additional arguments to pass to cargo build
    #[arg(long)]
    cargo_args: Option<String>,

    /// Package to run stress tests for (in a workspace)
    #[arg(long, short = 'p')]
    package: Option<String>,

    /// Path to Cargo.toml
    #[arg(long)]
    manifest_path: Option<PathBuf>,

    // ========================================================================
    // Advanced
    // ========================================================================
    /// Don't rebuild binaries, just run existing ones
    #[arg(long)]
    no_build: bool,

    /// Keep going even if a stress binary fails
    #[arg(long)]
    no_fail_fast: bool,
}

// ============================================================================
// Discovered Stress File
// ============================================================================

/// Represents a discovered stress test file in the stress/ directory.
#[derive(Debug, Clone)]
struct StressFile {
    /// Path to the .rs file (e.g., "stress/fsync.rs")
    #[allow(dead_code)]
    path: PathBuf,
    /// Stem of the filename (e.g., "fsync")
    stem: String,
    /// Derived binary name (e.g., "stress_fsync") when built as a bin
    binary_name: String,
}

impl StressFile {
    fn from_path(path: PathBuf) -> Option<Self> {
        let stem = path.file_stem()?.to_str()?.to_string();
        // Binary name: prefix with "stress_" to avoid conflicts
        let binary_name = format!("stress_{}", stem);
        Some(Self {
            path,
            stem,
            binary_name,
        })
    }
}

/// Create a temporary workspace member (package) containing the `src/bin/` files copied
/// from the project's `stress/` directory so we can build them without requiring
/// modifications to the user's `Cargo.toml`.
fn create_temp_workspace(files: &[StressFile], project_root: &Path) -> Result<(PathBuf, PathBuf)> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let temp_root = project_root
        .join("target")
        .join("cargo-stress-temp")
        .join(format!("run-{}", ts));

    // Paths
    let manifest_path = temp_root.join("Cargo.toml");
    let src_bin_dir = temp_root.join("src").join("bin");
    let target_dir = temp_root.join("target");

    // Create directories
    fs::create_dir_all(&src_bin_dir).context("Failed to create temp workspace src/bin")?;

    // Read original Cargo.toml to get package name and check if cntryl-stress is already a dep
    let root_manifest = project_root.join("Cargo.toml");
    let manifest_text =
        fs::read_to_string(&root_manifest).context("Failed to read root Cargo.toml")?;
    let pkg_name = manifest_text
        .lines()
        .find_map(|l| {
            let t = l.trim();
            if t.starts_with("name") {
                if let Some(eq) = t.find('=') {
                    let v = t[eq + 1..].trim();
                    return v.trim_matches('"').trim().to_string().into();
                }
            }
            None
        })
        .unwrap_or_else(|| "unknown".to_string());

    // Convert path to use forward slashes for TOML compatibility
    let project_root_str = project_root.display().to_string().replace('\\', "/");

    // Check if this IS the cntryl-stress repo itself
    let is_stress_repo = pkg_name == "cntryl-stress";

    // Write Cargo.toml for temp package
    let manifest_contents = if is_stress_repo {
        // Building stress tests for cntryl-stress itself
        format!(
            r#"[package]
name = "cargo-stress-temp-{ts}"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
cntryl-stress = {{ path = "{project_root_str}" }}
"#
        )
    } else {
        // Building stress tests for a user project — need both cntryl-stress AND their crate
        format!(
            r#"[package]
name = "cargo-stress-temp-{ts}"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
cntryl-stress = "0.1"
{pkg_name} = {{ path = "{project_root_str}" }}
"#
        )
    };

    fs::write(&manifest_path, manifest_contents).context("Failed to write temp Cargo.toml")?;

    // Copy stress files into src/bin, appending stress_main!() if not present
    for file in files {
        let dest = src_bin_dir.join(format!("{}.rs", file.stem));
        let content = fs::read_to_string(&file.path)
            .with_context(|| format!("Failed to read {}", file.path.display()))?;

        // Append stress_main!() if not already present
        let final_content = if content.contains("stress_main!") {
            content
        } else {
            format!("{}\n\ncntryl_stress::stress_main!();\n", content.trim_end())
        };

        fs::write(&dest, final_content)
            .with_context(|| format!("Failed to write {}", dest.display()))?;
    }

    Ok((manifest_path, target_dir))
}

// ============================================================================
// Execution Result
// ============================================================================

/// Result of running a single stress binary.
#[derive(Debug)]
struct StressRunResult {
    file: StressFile,
    status: ExitStatus,
    duration: Duration,
    stdout: String,
    stderr: String,
}

impl StressRunResult {
    fn success(&self) -> bool {
        self.status.success()
    }
}

// ============================================================================
// Main Entry Point
// ============================================================================

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Commands::Stress(args) => run_stress(args),
    }
}

fn run_stress(args: StressArgs) -> Result<()> {
    let verbosity = Verbosity::from_args(&args);

    // Step 1: Locate project root
    let manifest_path = find_manifest(&args)?;
    let project_root = manifest_path
        .parent()
        .context("Cargo.toml has no parent directory")?;

    if verbosity.is_verbose() {
        eprintln!("📁 Project root: {}", project_root.display());
    }

    // Step 2: Discover stress files
    let stress_dir = project_root.join("stress");
    let stress_files = discover_stress_files(&stress_dir, &args)?;

    if stress_files.is_empty() {
        if verbosity.is_normal() {
            eprintln!("⚠️  No stress test files found in {}", stress_dir.display());
            eprintln!("   Create .rs files in stress/ with #[stress_test] functions");
        }
        return Ok(());
    }

    // Create temporary workspace so users don't need to update Cargo.toml
    let (temp_manifest, temp_target_dir) = create_temp_workspace(&stress_files, project_root)?;

    if verbosity.is_verbose() {
        eprintln!(
            "🔍 Found {} stress file(s): {}",
            stress_files.len(),
            stress_files
                .iter()
                .map(|f| f.stem.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // Step 3: Build stress binaries (unless --no-build)
    if !args.no_build {
        build_stress_binaries(
            &stress_files,
            &args,
            &temp_manifest,
            verbosity,
            Some(&temp_target_dir),
        )?;
    }

    // Step 4: Run stress binaries
    let results = run_stress_binaries(&stress_files, &args, &temp_target_dir, verbosity)?;

    // Step 5: Report results
    report_results(&results, verbosity)?;

    // Step 6: Exit with appropriate code
    let failed_count = results.iter().filter(|r| !r.success()).count();
    if failed_count > 0 {
        if verbosity.is_normal() {
            eprintln!(
                "\n❌ {} of {} stress test(s) failed",
                failed_count,
                results.len()
            );
        }
        std::process::exit(1);
    }

    if verbosity.is_normal() {
        eprintln!("\n✅ All {} stress test(s) passed", results.len());
    }

    // Cleanup temp workspace on success (leave on failure for debugging)
    let temp_root = temp_manifest.parent().unwrap();
    if let Err(e) = fs::remove_dir_all(temp_root) {
        if verbosity.is_verbose() {
            eprintln!(
                "⚠️  Failed to clean up temp workspace {}: {}",
                temp_root.display(),
                e
            );
        }
    }

    Ok(())
}

// ============================================================================
// Verbosity Control
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

impl Verbosity {
    fn from_args(args: &StressArgs) -> Self {
        if args.quiet {
            Verbosity::Quiet
        } else if args.verbose {
            Verbosity::Verbose
        } else {
            Verbosity::Normal
        }
    }

    fn is_quiet(&self) -> bool {
        *self == Verbosity::Quiet
    }

    fn is_normal(&self) -> bool {
        !self.is_quiet()
    }

    fn is_verbose(&self) -> bool {
        *self == Verbosity::Verbose
    }
}

// ============================================================================
// Manifest Discovery
// ============================================================================

/// Find the Cargo.toml, either from --manifest-path or by walking up from CWD.
fn find_manifest(args: &StressArgs) -> Result<PathBuf> {
    if let Some(ref path) = args.manifest_path {
        if path.exists() {
            return Ok(path.clone());
        }
        bail!("Specified manifest path does not exist: {}", path.display());
    }

    // Walk up from current directory
    let mut dir = std::env::current_dir().context("Failed to get current directory")?;
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.exists() {
            return Ok(candidate);
        }
        if !dir.pop() {
            bail!(
                "Could not find Cargo.toml in {} or any parent directory",
                std::env::current_dir()?.display()
            );
        }
    }
}

// ============================================================================
// Stress File Discovery
// ============================================================================

/// Discover all .rs files in the stress/ directory.
fn discover_stress_files(stress_dir: &Path, args: &StressArgs) -> Result<Vec<StressFile>> {
    if !stress_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();

    for entry in fs::read_dir(stress_dir).context("Failed to read stress/ directory")? {
        let entry = entry?;
        let path = entry.path();

        // Only .rs files
        if path.extension() != Some(OsStr::new("rs")) {
            continue;
        }

        // Skip if not a file
        if !path.is_file() {
            continue;
        }

        if let Some(stress_file) = StressFile::from_path(path) {
            // Filter by --bin if specified
            if let Some(ref bin_filter) = args.bin {
                if stress_file.stem != *bin_filter && stress_file.binary_name != *bin_filter {
                    continue;
                }
            }
            files.push(stress_file);
        }
    }

    // Sort for deterministic ordering
    files.sort_by(|a, b| a.stem.cmp(&b.stem));

    Ok(files)
}

// ============================================================================
// Build Phase
// ============================================================================

/// Build all stress binaries using Cargo.
fn build_stress_binaries(
    files: &[StressFile],
    args: &StressArgs,
    manifest_path: &Path,
    verbosity: Verbosity,
    target_dir_override: Option<&Path>,
) -> Result<()> {
    if verbosity.is_normal() {
        eprintln!(
            "🔨 Building {} stress binary(ies) in {} mode...",
            files.len(),
            if args.dev { "debug" } else { "release" }
        );
    }

    // Build all binaries in one cargo invocation for efficiency
    let mut cmd = Command::new("cargo");
    cmd.arg("build");

    // Release mode by default (stress tests should always be optimized)
    if !args.dev {
        cmd.arg("--release");
    }

    // Specify manifest path
    cmd.arg("--manifest-path").arg(manifest_path);

    // Add package if in workspace
    if let Some(ref package) = args.package {
        cmd.arg("--package").arg(package);
    }

    // Override target-dir if requested (used for temp workspace builds)
    if let Some(td) = target_dir_override {
        cmd.arg("--target-dir").arg(td);
    }

    // Add each binary target (in temp workspace the bin name is the file stem)
    for file in files {
        cmd.arg("--bin").arg(&file.stem);
    }

    // Additional cargo args
    if let Some(ref extra) = args.cargo_args {
        for arg in extra.split_whitespace() {
            cmd.arg(arg);
        }
    }

    if verbosity.is_verbose() {
        eprintln!("   Running: {:?}", cmd);
    }

    let output = cmd
        .stdout(if verbosity.is_verbose() {
            Stdio::inherit()
        } else {
            Stdio::piped()
        })
        .stderr(if verbosity.is_verbose() {
            Stdio::inherit()
        } else {
            Stdio::piped()
        })
        .output()
        .context("Failed to run cargo build")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        eprintln!("\n❌ Build failed!");
        if !stdout.is_empty() {
            eprintln!("{}", stdout);
        }
        if !stderr.is_empty() {
            eprintln!("{}", stderr);
        }

        bail!(
            "Cargo build failed with exit code: {:?}",
            output.status.code()
        );
    }

    if verbosity.is_normal() {
        eprintln!("   Build complete.");
    }

    if verbosity.is_normal() {
        eprintln!("   Build complete.");
    }

    Ok(())
}

// ============================================================================
// Execution Phase
// ============================================================================

/// Run all stress binaries and collect results.
fn run_stress_binaries(
    files: &[StressFile],
    args: &StressArgs,
    target_dir_parent: &Path,
    verbosity: Verbosity,
) -> Result<Vec<StressRunResult>> {
    let target_dir = target_dir_parent.join(if args.dev { "debug" } else { "release" });

    let mut results = Vec::new();

    for file in files {
        let binary_path = target_dir.join(&file.stem);

        // On Windows, add .exe extension
        #[cfg(windows)]
        let binary_path = binary_path.with_extension("exe");

        if !binary_path.exists() {
            if verbosity.is_normal() {
                eprintln!(
                    "⚠️  Binary not found: {} (expected at {})",
                    file.stem,
                    binary_path.display()
                );
            }
            continue;
        }

        let result = run_single_binary(file, &binary_path, args, verbosity)?;

        let failed = !result.success();
        results.push(result);

        // Fail fast unless --no-fail-fast
        if failed && !args.no_fail_fast {
            break;
        }
    }

    Ok(results)
}

/// Run a single stress binary and capture output.
fn run_single_binary(
    file: &StressFile,
    binary_path: &Path,
    args: &StressArgs,
    verbosity: Verbosity,
) -> Result<StressRunResult> {
    if verbosity.is_normal() {
        eprintln!("\n🏃 Running stress test: {}", file.stem);
    }

    let mut cmd = Command::new(binary_path);

    // Pass through all relevant arguments to the stress binary
    // These are handled by the stress_main!() macro via clap parsing
    build_passthrough_args(&mut cmd, args);

    if verbosity.is_verbose() {
        eprintln!("   Executing: {:?}", cmd);
    }

    let start = Instant::now();

    // Run with inherited stdio for real-time output in verbose mode,
    // or capture for summary in normal mode
    let output = if verbosity.is_verbose() {
        // In verbose mode, let output stream through
        let status = cmd
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .with_context(|| format!("Failed to execute {}", binary_path.display()))?;

        StressRunResult {
            file: file.clone(),
            status,
            duration: start.elapsed(),
            stdout: String::new(),
            stderr: String::new(),
        }
    } else {
        // Capture output for summary
        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to execute {}", binary_path.display()))?;

        StressRunResult {
            file: file.clone(),
            status: output.status,
            duration: start.elapsed(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    };

    // Print captured output in normal mode
    if !verbosity.is_verbose() && verbosity.is_normal() {
        if !output.stdout.is_empty() {
            print!("{}", output.stdout);
        }
        if !output.stderr.is_empty() {
            eprint!("{}", output.stderr);
        }
    }

    if verbosity.is_verbose() {
        eprintln!(
            "   Completed in {:.2}s with exit code: {:?}",
            output.duration.as_secs_f64(),
            output.status.code()
        );
    }

    Ok(output)
}

/// Build arguments to pass through to the stress binary.
fn build_passthrough_args(cmd: &mut Command, args: &StressArgs) {
    // Workload filter
    if let Some(ref workload) = args.workload {
        cmd.arg("--workload").arg(workload);
    }

    // Runs
    if args.runs != 1 {
        cmd.arg("--runs").arg(args.runs.to_string());
    }

    // Warmup
    if args.warmup != 0 {
        cmd.arg("--warmup").arg(args.warmup.to_string());
    }

    // Verbosity
    if args.verbose {
        cmd.arg("--verbose");
    }
    if args.quiet {
        cmd.arg("--quiet");
    }

    // Include ignored
    if args.include_ignored {
        cmd.arg("--include-ignored");
    }

    // List mode
    if args.list {
        cmd.arg("--list");
    }

    // Output directory
    if let Some(ref dir) = args.output_dir {
        cmd.arg("--output-dir").arg(dir);
    }

    // Baseline comparison
    if let Some(ref baseline) = args.baseline {
        cmd.arg("--baseline").arg(baseline);
    }

    // Threshold
    if (args.threshold - 0.05).abs() > f64::EPSILON {
        cmd.arg("--threshold").arg(args.threshold.to_string());
    }
}

// ============================================================================
// Results Reporting
// ============================================================================

/// Print summary of all stress test results.
fn report_results(results: &[StressRunResult], verbosity: Verbosity) -> Result<()> {
    if results.is_empty() || verbosity.is_quiet() {
        return Ok(());
    }

    eprintln!("Summary:");

    let total_duration: Duration = results.iter().map(|r| r.duration).sum();

    for result in results {
        let status = if result.success() { "✓" } else { "✗" };
        let exit_info = result
            .status
            .code()
            .map(|c| format!("exit {}", c))
            .unwrap_or_else(|| "signal".to_string());

        eprintln!(
            "  {} {} ({:.2}s, {})",
            status,
            result.file.stem,
            result.duration.as_secs_f64(),
            exit_info
        );
    }

    eprintln!("Total time: {:.2}s", total_duration.as_secs_f64());

    Ok(())
}
