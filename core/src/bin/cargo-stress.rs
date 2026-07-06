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
//!   benches/
//!     fsync.rs          # -> binary: stress_fsync
//!     compaction.rs     # -> binary: stress_compaction
//!     recovery.rs       # -> binary: stress_recovery
//! ```
//!
//! Each stress file must:
//! - Contain functions annotated with `#[stress]`
//! - End with `cntryl_stress::stress_main!()`

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use cntryl_stress::{artifact::StressRun, reporting::format_console_runs};
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

Each .rs file in the benches/ directory is compiled as a separate binary
in release mode. Tests are defined with #[stress] and discovered
automatically at runtime by each binary.

Example:
    cargo stress                        # Run all stress tests with the trustworthy default gate
    cargo stress --workload 'fsync*'    # Filter by pattern
    cargo stress --baseline latest.json # Compare against a stress baseline
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
#[allow(clippy::struct_excessive_bools)]
struct StressArgs {
    // ========================================================================
    // Test Selection
    // ========================================================================
    /// Filter benchmarks by glob pattern (e.g., "database*", "*insert*")
    /// Passed through to each stress binary.
    #[arg(long)]
    workload: Option<String>,

    /// Include ignored benchmarks (falls back to `STRESS_INCLUDE_IGNORED`)
    #[arg(long)]
    include_ignored: bool,

    /// List all registered benchmarks without running them
    #[arg(long)]
    list: bool,

    /// Print resolved runner config without running benchmarks
    #[arg(long)]
    print_config: bool,

    /// Run only a specific stress binary (filename without .rs extension)
    #[arg(long)]
    bin: Option<String>,

    // ========================================================================
    // Execution Options
    // ========================================================================
    /// Optional profile override: default, smoke, lab, or release (falls back to `STRESS_PROFILE`)
    #[arg(long)]
    profile: Option<String>,

    /// Run only one numeric benchmark tier
    #[arg(long)]
    tier: Option<u32>,

    /// Number of measured samples per benchmark (falls back to `STRESS_SAMPLES`)
    #[arg(long)]
    samples: Option<usize>,

    /// Number of warmup samples (falls back to `STRESS_WARMUP_SAMPLES`)
    #[arg(long)]
    warmup_samples: Option<usize>,

    /// Number of cooldown samples (falls back to `STRESS_COOLDOWN_SAMPLES`)
    #[arg(long)]
    cooldown_samples: Option<usize>,

    // ========================================================================
    // Output Control
    // ========================================================================
    /// Quiet mode (minimal output, only errors)
    #[arg(long, short = 'q')]
    quiet: bool,

    /// Print machine-readable JSON to stdout
    #[arg(long)]
    json: bool,

    /// Output directory for artifacts (falls back to `STRESS_OUTPUT_DIR`)
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Human console benchmark-name mode: compact or full
    #[arg(long)]
    names: Option<String>,

    /// Disable stderr progress from child stress binaries
    #[arg(long)]
    no_progress: bool,

    // ========================================================================
    // Regression Detection
    // ========================================================================
    /// Baseline JSON file for regression comparison (falls back to `STRESS_BASELINE`)
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Baseline directory for latest/save conventions
    #[arg(long)]
    baseline_dir: Option<PathBuf>,

    /// Save passed runs as baselines
    #[arg(long)]
    save_baseline: bool,

    /// Regression threshold percentage (falls back to `STRESS_THRESHOLD`, then 5%)
    #[arg(long)]
    threshold: Option<f64>,

    /// Fail on warning-or-error diagnostics
    #[arg(long)]
    fail_on_issues: bool,

    /// Fail on diagnostics at info, warning, or error
    #[arg(long)]
    deny_diagnostics: Option<String>,

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

/// Represents a discovered stress test file in the benches/ directory.
#[derive(Debug, Clone)]
struct StressFile {
    /// Path to the .rs file (e.g., "benches/fsync.rs")
    #[allow(dead_code)]
    path: PathBuf,
    /// Stem of the filename (e.g., "fsync")
    stem: String,
    /// Derived binary name (e.g., "`stress_fsync`") when built as a bin
    binary_name: String,
}

impl StressFile {
    fn from_path(path: PathBuf) -> Option<Self> {
        let stem = path.file_stem()?.to_str()?.to_string();
        // Binary name: prefix with "stress_" to avoid conflicts
        let binary_name = format!("stress_{stem}");
        Some(Self {
            path,
            stem,
            binary_name,
        })
    }
}

/// Create a temporary workspace member (package) containing the `src/bin/` files copied
/// from the project's `benches/` directory so we can build them without requiring
/// modifications to the user's `Cargo.toml`.
fn create_temp_workspace(files: &[StressFile], project_root: &Path) -> Result<(PathBuf, PathBuf)> {
    let temp_root = project_root.join("target").join("cargo-stress-temp");

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
    let stress_version = env!("CARGO_PKG_VERSION");
    let stress_dependency = stress_dependency_spec(&manifest_text, project_root, stress_version);

    // Check if this IS the cntryl-stress repo itself
    let is_stress_repo = pkg_name == "cntryl-stress";

    // Write Cargo.toml for temp package
    let manifest_contents = if is_stress_repo {
        // Building stress tests for cntryl-stress itself
        format!(
            r#"[workspace]

[package]
name = "cargo-stress-temp"
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
            r#"[workspace]

[package]
name = "cargo-stress-temp"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
{stress_dependency}
{pkg_name} = {{ path = "{project_root_str}" }}
"#
        )
    };

    fs::write(&manifest_path, manifest_contents).context("Failed to write temp Cargo.toml")?;

    // Copy stress files into src/bin
    for file in files {
        let dest = src_bin_dir.join(format!("{}.rs", file.stem));
        let content = fs::read_to_string(&file.path)
            .with_context(|| format!("Failed to read {}", file.path.display()))?;

        fs::write(&dest, content).with_context(|| format!("Failed to write {}", dest.display()))?;
    }

    Ok((manifest_path, target_dir))
}

fn stress_dependency_spec(
    manifest_text: &str,
    project_root: &Path,
    stress_version: &str,
) -> String {
    manifest_text
        .lines()
        .find_map(|line| local_stress_dependency_path(line, project_root))
        .map_or_else(
            || format!(r#"cntryl-stress = "{stress_version}""#),
            |path| format!(r#"cntryl-stress = {{ path = "{path}" }}"#),
        )
}

fn local_stress_dependency_path(line: &str, project_root: &Path) -> Option<String> {
    let trimmed = line.trim();
    let (name, rest) = trimmed.split_once('=')?;
    if name.trim() != "cntryl-stress" || !rest.contains("path") {
        return None;
    }
    let path = extract_inline_toml_string(rest, "path")?;
    let absolute = project_root.join(path).canonicalize().ok()?;
    Some(absolute.display().to_string().replace('\\', "/"))
}

fn extract_inline_toml_string(input: &str, key: &str) -> Option<String> {
    let start = input.find(key)?;
    let after_key = &input[start + key.len()..];
    let equals = after_key.find('=')?;
    let after_equals = after_key[equals + 1..].trim_start();
    let quoted = after_equals.strip_prefix('"')?;
    let end = quoted.find('"')?;
    Some(quoted[..end].to_string())
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
    run: Option<StressRun>,
    parse_error: Option<String>,
}

impl StressRunResult {
    fn success(&self) -> bool {
        self.status.success() && self.parse_error.is_none()
    }
}

// ============================================================================
// Main Entry Point
// ============================================================================

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Commands::Stress(args) => run_stress(&args),
    }
}

fn run_stress(args: &StressArgs) -> Result<()> {
    let verbosity = Verbosity::from_args(args);
    let passthrough_json = !args.list && !args.print_config;

    // Step 1: Locate project root
    let manifest_path = find_manifest(args)?
        .canonicalize()
        .context("Failed to resolve manifest path")?;
    let project_root = manifest_path
        .parent()
        .context("Cargo.toml has no parent directory")?;

    // Step 2: Discover stress files
    let benches_dir = project_root.join("benches");
    let stress_files = discover_stress_files(&benches_dir, args)?;

    if stress_files.is_empty() {
        if verbosity.is_normal() {
            println!("No stress test files found in {}", benches_dir.display());
            println!("   Create .rs files in benches/ that include cntryl_stress::stress_main!()");
        }
        return Ok(());
    }

    // Create temporary workspace so users don't need to update Cargo.toml
    let (temp_manifest, temp_target_dir) = create_temp_workspace(&stress_files, project_root)?;

    // Step 3: Build stress binaries (unless --no-build)
    if !args.no_build {
        build_stress_binaries(
            &stress_files,
            args,
            &temp_manifest,
            verbosity,
            Some(&temp_target_dir),
        )?;
    }

    // Step 4: Run stress binaries
    let run_id = shared_run_id();
    let results = run_stress_binaries(
        &stress_files,
        args,
        &temp_target_dir,
        verbosity,
        passthrough_json,
        &run_id,
    )?;

    // Step 5: Report results
    report_results(&results, args.json, verbosity, passthrough_json)?;

    // Step 6: Exit with appropriate code
    let failed_count = results.iter().filter(|r| !r.success()).count();
    if failed_count > 0 {
        std::process::exit(1);
    }

    // Cleanup temp workspace on success (leave on failure for debugging)
    let temp_root = temp_manifest.parent().unwrap();
    if let Err(error) = fs::remove_dir_all(temp_root) {
        if verbosity.is_normal() {
            eprintln!(
                "Warning: failed to clean up temp workspace {}: {}",
                temp_root.display(),
                error
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
}

impl Verbosity {
    fn from_args(args: &StressArgs) -> Self {
        if args.quiet {
            Verbosity::Quiet
        } else {
            Verbosity::Normal
        }
    }

    fn is_quiet(self) -> bool {
        self == Verbosity::Quiet
    }

    fn is_normal(self) -> bool {
        !self.is_quiet()
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

/// Discover all .rs files in the benches/ directory.
fn discover_stress_files(benches_dir: &Path, args: &StressArgs) -> Result<Vec<StressFile>> {
    if !benches_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();

    for entry in fs::read_dir(benches_dir).context("Failed to read benches/ directory")? {
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

        // Only treat this as a stress file if it declares a stress_main!() entrypoint.
        // This prevents accidental conflicts with other benchmark harnesses.
        // that also live under benches/.
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if !content.contains("stress_main!") {
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
    _verbosity: Verbosity,
    target_dir_override: Option<&Path>,
) -> Result<()> {
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

    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("Failed to run cargo build")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        eprintln!("Build failed.");
        if !stdout.is_empty() {
            eprintln!("{stdout}");
        }
        if !stderr.is_empty() {
            eprintln!("{stderr}");
        }

        bail!(
            "Cargo build failed with exit code: {:?}",
            output.status.code()
        );
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
    passthrough_json: bool,
    run_id: &str,
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
                    "Binary not found: {} (expected at {})",
                    file.stem,
                    binary_path.display()
                );
            }
            continue;
        }

        let result = run_single_binary(file, &binary_path, args, passthrough_json, run_id)?;

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
    passthrough_json: bool,
    run_id: &str,
) -> Result<StressRunResult> {
    let mut cmd = Command::new(binary_path);
    apply_run_id_env(&mut cmd, run_id);

    // Pass through all relevant arguments to the stress binary
    // These are handled by the stress_main!() macro via clap parsing
    build_passthrough_args(&mut cmd, args, passthrough_json);

    let start = Instant::now();

    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to execute {}", binary_path.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let (run, parse_error) = if passthrough_json {
        match serde_json::from_str::<StressRun>(&stdout) {
            Ok(run) => (Some(run), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (None, None)
    };

    let output = StressRunResult {
        file: file.clone(),
        status: output.status,
        duration: start.elapsed(),
        stdout,
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        run,
        parse_error,
    };

    Ok(output)
}

fn shared_run_id() -> String {
    std::env::var("STRESS_RUN_ID")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(generate_run_id)
}

fn generate_run_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("stress-{millis}-{}", std::process::id())
}

fn apply_run_id_env(cmd: &mut Command, run_id: &str) {
    cmd.env("STRESS_RUN_ID", run_id);
}

/// Build arguments to pass through to the stress binary.
fn build_passthrough_args(cmd: &mut Command, args: &StressArgs, passthrough_json: bool) {
    // Workload filter
    if let Some(ref workload) = args.workload {
        cmd.arg("--workload").arg(workload);
    }

    if let Some(profile) = &args.profile {
        cmd.arg("--profile").arg(profile);
    }

    if let Some(tier) = args.tier {
        cmd.arg("--tier").arg(tier.to_string());
    }

    if let Some(samples) = args.samples {
        cmd.arg("--samples").arg(samples.to_string());
    }

    if let Some(warmup_samples) = args.warmup_samples {
        cmd.arg("--warmup-samples").arg(warmup_samples.to_string());
    }

    if let Some(cooldown_samples) = args.cooldown_samples {
        cmd.arg("--cooldown-samples")
            .arg(cooldown_samples.to_string());
    }

    if passthrough_json || args.json {
        cmd.arg("--json");
    }

    // Include ignored
    if args.include_ignored {
        cmd.arg("--include-ignored");
    }

    // List mode
    if args.list {
        cmd.arg("--list");
    }

    if args.print_config {
        cmd.arg("--print-config");
    }

    // Output directory
    if let Some(ref dir) = args.output_dir {
        cmd.arg("--output-dir").arg(dir);
    }

    if let Some(ref names) = args.names {
        cmd.arg("--names").arg(names);
    }

    if args.no_progress {
        cmd.arg("--no-progress");
    }

    // Baseline comparison
    if let Some(ref baseline) = args.baseline {
        cmd.arg("--baseline").arg(baseline);
    }

    if let Some(ref baseline_dir) = args.baseline_dir {
        cmd.arg("--baseline-dir").arg(baseline_dir);
    }

    if args.save_baseline {
        cmd.arg("--save-baseline");
    }

    // Threshold
    if let Some(threshold) = args.threshold {
        cmd.arg("--threshold").arg(threshold.to_string());
    }

    if args.fail_on_issues {
        cmd.arg("--fail-on-issues");
    }

    if let Some(ref deny_diagnostics) = args.deny_diagnostics {
        cmd.arg("--deny-diagnostics").arg(deny_diagnostics);
    }
}

// ============================================================================
// Results Reporting
// ============================================================================

/// Print consolidated stress test results.
fn report_results(
    results: &[StressRunResult],
    json_stdout: bool,
    verbosity: Verbosity,
    passthrough_json: bool,
) -> Result<()> {
    if results.is_empty() {
        return Ok(());
    }

    if !passthrough_json {
        report_passthrough_results(results, verbosity);
        return Ok(());
    }

    let output = if json_stdout {
        consolidated_json_output(results)?
    } else {
        consolidated_output(results)?
    };
    if !verbosity.is_quiet() {
        println!("{output}");
    }
    report_child_failures(results);
    Ok(())
}

fn consolidated_output(results: &[StressRunResult]) -> Result<String> {
    report_parse_errors(results)?;
    let runs = results
        .iter()
        .filter_map(|result| result.run.clone())
        .collect::<Vec<_>>();
    Ok(format_console_runs(&runs))
}

fn consolidated_json_output(results: &[StressRunResult]) -> Result<String> {
    report_parse_errors(results)?;
    let runs = results
        .iter()
        .filter_map(|result| result.run.clone())
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&runs).context("failed to serialize consolidated stress JSON")
}

fn report_passthrough_results(results: &[StressRunResult], verbosity: Verbosity) {
    if verbosity.is_quiet() {
        return;
    }
    for result in results {
        if !result.stdout.is_empty() {
            print!("{}", result.stdout);
        }
        if !result.stderr.is_empty() {
            eprint!("{}", result.stderr);
        }
    }
}

fn report_parse_errors(results: &[StressRunResult]) -> Result<()> {
    let failed = results
        .iter()
        .filter_map(|result| {
            result
                .parse_error
                .as_ref()
                .map(|error| (result, error.as_str()))
        })
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return Ok(());
    }

    for (result, error) in failed {
        eprintln!(
            "Failed to parse stress JSON from {} after {:.2}s: {}",
            result.file.stem,
            result.duration.as_secs_f64(),
            error
        );
        if !result.stdout.trim().is_empty() {
            eprintln!("stdout:\n{}", result.stdout);
        }
        if !result.stderr.trim().is_empty() {
            eprintln!("stderr:\n{}", result.stderr);
        }
    }
    bail!("one or more stress binaries did not emit parseable JSON")
}

fn report_child_failures(results: &[StressRunResult]) {
    for result in results.iter().filter(|result| !result.status.success()) {
        let exit_info = result
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |code| format!("exit {code}"));
        eprintln!(
            "Stress binary {} failed after {:.2}s ({exit_info})",
            result.file.stem,
            result.duration.as_secs_f64()
        );
        if !result.stderr.trim().is_empty() {
            eprint!("{}", result.stderr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cntryl_stress::artifact::{
        BenchmarkBudgets, BenchmarkMode, BenchmarkSpec, BenchmarkSummary, ConsoleNameMode,
        CorrectnessCounters, CorrectnessSummary, EnvironmentInfo, MeasurementIntent, PrimaryMetric,
        ProfileConfig, QualityClass, RunProfile, Sample, SamplePhase, SummaryStats, SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;

    fn stress_file(stem: &str) -> StressFile {
        StressFile {
            path: PathBuf::from(format!("benches/{stem}.rs")),
            stem: stem.to_string(),
            binary_name: format!("stress_{stem}"),
        }
    }

    fn result_for(run: StressRun) -> StressRunResult {
        StressRunResult {
            file: stress_file(&run.suite),
            status: success_status(),
            duration: Duration::from_millis(1),
            stdout: String::new(),
            stderr: String::new(),
            run: Some(run),
            parse_error: None,
        }
    }

    fn stress_args() -> StressArgs {
        StressArgs {
            workload: None,
            include_ignored: false,
            list: false,
            print_config: false,
            bin: None,
            profile: None,
            tier: None,
            samples: None,
            warmup_samples: None,
            cooldown_samples: None,
            quiet: false,
            json: false,
            output_dir: None,
            names: None,
            no_progress: false,
            baseline: None,
            baseline_dir: None,
            save_baseline: false,
            threshold: None,
            fail_on_issues: false,
            deny_diagnostics: None,
            dev: false,
            cargo_args: None,
            package: None,
            manifest_path: None,
            no_build: false,
            no_fail_fast: false,
        }
    }

    #[test]
    fn applies_shared_run_id_to_child_command() {
        let mut cmd = Command::new("stress-child");

        apply_run_id_env(&mut cmd, "shared-run");

        assert!(cmd.get_envs().any(|(key, value)| {
            key == "STRESS_RUN_ID" && value == Some(std::ffi::OsStr::new("shared-run"))
        }));
    }

    #[cfg(unix)]
    fn success_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn success_status() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }

    fn summary(name: &str, quality: QualityClass) -> BenchmarkSummary {
        BenchmarkSummary {
            benchmark_id: name.to_string(),
            name: name.to_string(),
            tier: 2,
            intent: MeasurementIntent::General,
            primary_metric: PrimaryMetric::Throughput,
            measured_samples: 10,
            warmup_samples: 1,
            cooldown_samples: 0,
            stats: SummaryStats::from_values(&[100.0, 101.0]),
            wall_clock: SummaryStats::from_values(&[1_000_000.0]),
            total_wall_clock_ns: 1_000_000,
            ns_per_op: None,
            gross_ns_per_op: None,
            overhead_ns_per_op: None,
            allocs_per_op: None,
            bytes_per_op: None,
            quality,
            budgets: BenchmarkBudgets::default(),
            budget_results: Vec::new(),
            diagnostics: Vec::new(),
            correctness: CorrectnessSummary {
                passed: true,
                counters: CorrectnessCounters {
                    attempted: 10,
                    completed: 10,
                    ..CorrectnessCounters::default()
                },
                errors: Vec::new(),
            },
            parameters: BTreeMap::new(),
            metadata: BTreeMap::new(),
        }
    }

    fn run(suite: &str, summaries: Vec<BenchmarkSummary>) -> StressRun {
        let profile_config = ProfileConfig {
            profile: RunProfile::Release,
            measured_samples: 10,
            warmup_samples: 1,
            cooldown_samples: 0,
            min_quality: QualityClass::Acceptable,
            fail_on_quality: true,
            fail_on_regression: true,
            deny_diagnostics: None,
            regression_threshold: 0.05,
            sample_duration: Duration::from_secs(1),
            operations_per_sample: 1,
            micro_sample_duration: Duration::from_millis(100),
            report_depth: "gated".to_string(),
            console_names: ConsoleNameMode::Compact,
            progress: true,
        };
        StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: "0.3.0".to_string(),
            suite: suite.to_string(),
            run_profile: RunProfile::Release,
            environment: EnvironmentInfo::unknown(profile_config.clone()),
            benchmark_specs: vec![BenchmarkSpec {
                id: format!("{suite}/bench"),
                name: "bench".to_string(),
                tier: 2,
                mode: BenchmarkMode::FixedOperations {
                    operations_per_sample: 1,
                },
                intent: MeasurementIntent::General,
                budgets: BenchmarkBudgets::default(),
                parameters: BTreeMap::new(),
                metadata: BTreeMap::new(),
            }],
            samples: vec![Sample {
                benchmark_id: format!("{suite}/bench"),
                intent: MeasurementIntent::General,
                sample_number: 0,
                phase: SamplePhase::Measured,
                elapsed_ns: 1,
                wall_clock_ns: 1,
                operations_attempted: 1,
                operations_completed: 1,
                throughput: 1.0,
                calibrated_iterations: None,
                gross_elapsed_ns: None,
                overhead_ns: None,
                net_elapsed_ns: None,
                gross_ns_per_op: None,
                overhead_ns_per_op: None,
                net_ns_per_op: None,
                allocs: None,
                bytes: None,
                allocs_per_op: None,
                bytes_per_op: None,
                latency_ns: Vec::new(),
                parameters: BTreeMap::new(),
                counters: CorrectnessCounters {
                    attempted: 1,
                    completed: 1,
                    ..CorrectnessCounters::default()
                },
                environment: EnvironmentInfo::unknown(profile_config),
            }],
            summaries,
            comparisons: Vec::new(),
            diagnostics_summary: Vec::new(),
            started_at: "123".to_string(),
            total_elapsed_ns: 1_000,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn cargo_stress_human_output_is_consolidated() {
        let results = vec![
            result_for(run(
                "suite-a",
                vec![summary("suite_a::fast", QualityClass::Authoritative)],
            )),
            result_for(run(
                "suite-b",
                vec![summary("suite_b::fast", QualityClass::Acceptable)],
            )),
        ];

        let output = consolidated_output(&results).expect("output");

        assert_eq!(output.matches("@cntryl/stress").count(), 1);
        assert!(output.contains("suite-a"));
        assert!(output.contains("suite-b"));
        assert_eq!(output.matches("benchmark").count(), 2);
        assert_eq!(output.matches("result:").count(), 1);
        assert!(output.trim_end().ends_with("result: passed"));
        assert!(!output.contains("summary: gate"));
    }

    #[test]
    fn cargo_stress_human_output_shows_all_suites() {
        let results = vec![
            result_for(run(
                "clean-suite",
                vec![summary("clean::fast", QualityClass::Authoritative)],
            )),
            result_for(run(
                "noisy-suite",
                vec![summary("noisy::row", QualityClass::Noisy)],
            )),
        ];

        let output = consolidated_output(&results).expect("output");

        assert!(output.contains("clean-suite"));
        assert!(output.contains("noisy-suite"));
        assert!(output.contains("issues"));
    }

    #[test]
    fn cargo_stress_json_output_is_parseable_suite_array() {
        let results = vec![
            result_for(run(
                "suite-a",
                vec![summary("suite_a::fast", QualityClass::Authoritative)],
            )),
            result_for(run(
                "suite-b",
                vec![summary("suite_b::fast", QualityClass::Acceptable)],
            )),
        ];

        let output = consolidated_json_output(&results).expect("output");
        let parsed = serde_json::from_str::<Vec<StressRun>>(&output).expect("json array");

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].suite, "suite-a");
        assert_eq!(parsed[1].suite, "suite-b");
    }

    #[test]
    fn cargo_stress_passes_new_flags_to_child_binaries() {
        let stress_cli_args = StressArgs {
            names: Some("full".to_string()),
            no_progress: true,
            baseline: Some(PathBuf::from("latest")),
            baseline_dir: Some(PathBuf::from("target/custom-baselines")),
            save_baseline: true,
            fail_on_issues: true,
            deny_diagnostics: Some("error".to_string()),
            ..stress_args()
        };
        let mut cmd = Command::new("stress-child");

        build_passthrough_args(&mut cmd, &stress_cli_args, true);

        let child_args = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(child_args
            .windows(2)
            .any(|window| window[0] == "--names" && window[1] == "full"));
        assert!(child_args.contains(&"--no-progress".to_string()));
        assert!(child_args
            .windows(2)
            .any(|window| window[0] == "--baseline" && window[1] == "latest"));
        assert!(child_args
            .windows(2)
            .any(|window| window[0] == "--baseline-dir" && window[1] == "target/custom-baselines"));
        assert!(child_args.contains(&"--save-baseline".to_string()));
        assert!(child_args.contains(&"--fail-on-issues".to_string()));
        assert!(child_args
            .windows(2)
            .any(|window| window[0] == "--deny-diagnostics" && window[1] == "error"));
        assert!(child_args.contains(&"--json".to_string()));
    }
}
