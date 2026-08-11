//! cargo-stress: Cargo subcommand for running system-level stress tests.
//!
//! ## Design Philosophy
//!
//! This tool follows the same model as `cargo test`:
//!
//! 1. **No runtime magic**: Stress tests are compiled as normal Rust binaries
//! 2. **Cargo does the building**: We build declared Cargo bench targets in place
//! 3. **Orchestration only**: cargo-stress reads Cargo metadata, builds, and runs targets
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
//!   Cargo.toml          # Declares [[bench]] targets with harness = false
//!   src/lib.rs
//!   benches/
//!     fsync.rs
//!     compaction.rs
//!     recovery.rs
//! ```
//!
//! Each stress file must:
//! - Contain functions annotated with `#[stress]`
//! - End with `cntryl_stress::stress_main!()`

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use cntryl_stress::{
    artifact::{
        ConsoleNameMode, DiagnosticSeverity, RunProfile, StressRun, MAX_TIER, SCHEMA_VERSION,
    },
    reporting::format_console_runs,
    runner::{evaluate_run_gate, RunGate},
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::num::NonZeroU64;
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

Cargo bench targets containing stress_main! are built in place with their
package's real features, dev-dependencies, support modules, and build config.
Tests are defined with #[stress] and discovered at runtime by each target.

Example:
    cargo stress                        # Run all stress tests with the trustworthy default gate
    cargo stress --bench fsync          # Run one Cargo bench target
    cargo stress --workload 'fsync*'    # Filter by pattern
    cargo stress --baseline latest      # Compare each suite with its accepted baseline
    cargo stress --list                 # List available tests
"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ThresholdPercent(f64);

impl ThresholdPercent {
    fn as_fraction(self) -> f64 {
        self.0 / 100.0
    }
}

impl std::str::FromStr for ThresholdPercent {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let value = value
            .parse::<f64>()
            .map_err(|_| "threshold percent must be a number between 0 and 100".to_string())?;
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            return Err("threshold percent must be a number between 0 and 100".to_string());
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LegacyThresholdFraction(f64);

impl LegacyThresholdFraction {
    fn as_fraction(self) -> f64 {
        self.0
    }
}

impl std::str::FromStr for LegacyThresholdFraction {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let value = value.parse::<f64>().map_err(|_| {
            "legacy --threshold must be a fraction from 0 to 1; prefer --threshold-percent"
                .to_string()
        })?;
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(
                "legacy --threshold must be a fraction from 0 to 1 (0.05 means 5%); use --threshold-percent 5 for percentage points"
                    .to_string(),
            );
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PositiveU64(NonZeroU64);

impl PositiveU64 {
    const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BenchmarkTier(u32);

impl BenchmarkTier {
    const fn get(self) -> u32 {
        self.0
    }
}

impl std::str::FromStr for BenchmarkTier {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let tier = value
            .parse::<u32>()
            .map_err(|_| format!("tier must be an integer from 1 to {MAX_TIER}"))?;
        if (1..=MAX_TIER).contains(&tier) {
            Ok(Self(tier))
        } else {
            Err(format!("tier must be an integer from 1 to {MAX_TIER}"))
        }
    }
}

impl std::str::FromStr for PositiveU64 {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let value = value
            .parse::<u64>()
            .map_err(|_| "value must be a positive integer".to_string())?;
        NonZeroU64::new(value)
            .map(Self)
            .ok_or_else(|| "value must be a positive integer".to_string())
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run stress benchmarks
    #[command(
        name = "stress",
        after_help = "Child output is captured for consolidated reporting; live child progress is not currently streamed."
    )]
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

    /// Run one Cargo stress bench target (target name or source filename stem)
    #[arg(long = "bench", visible_alias = "bin", value_name = "BENCH")]
    bin: Option<String>,

    // ========================================================================
    // Execution Options
    // ========================================================================
    /// Optional profile override: default, smoke, lab, or release (falls back to `STRESS_PROFILE`)
    #[arg(long)]
    profile: Option<RunProfile>,

    /// Run only one numeric benchmark tier
    #[arg(long)]
    tier: Option<BenchmarkTier>,

    /// Number of measured samples per benchmark (falls back to `STRESS_SAMPLES`)
    #[arg(long)]
    samples: Option<usize>,

    /// Number of warmup samples (falls back to `STRESS_WARMUP_SAMPLES`)
    #[arg(long)]
    warmup_samples: Option<usize>,

    /// Number of cooldown samples (falls back to `STRESS_COOLDOWN_SAMPLES`)
    #[arg(long)]
    cooldown_samples: Option<usize>,

    /// Per-benchmark deadline in positive whole seconds
    #[arg(long, value_name = "SECONDS")]
    timeout_secs: Option<PositiveU64>,

    /// Operations in each fixed-operations sample (falls back to `STRESS_OPERATIONS_PER_SAMPLE`)
    #[arg(long, value_name = "COUNT")]
    operations_per_sample: Option<PositiveU64>,

    /// Milliseconds in each fixed-duration sample (falls back to `STRESS_SAMPLE_DURATION_MS`)
    #[arg(long, value_name = "MILLISECONDS")]
    sample_duration_ms: Option<PositiveU64>,

    /// Target milliseconds for calibrated micro samples (falls back to `STRESS_MICRO_SAMPLE_DURATION_MS`)
    #[arg(long, value_name = "MILLISECONDS")]
    micro_sample_duration_ms: Option<PositiveU64>,

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
    names: Option<ConsoleNameMode>,

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

    /// Regression threshold in percentage points (5 means 5%)
    #[arg(long, value_name = "PERCENT", conflicts_with = "threshold")]
    threshold_percent: Option<ThresholdPercent>,

    /// Compatibility option: threshold fraction (0.05 means 5%); prefer --threshold-percent
    #[arg(long, value_name = "FRACTION", conflicts_with = "threshold_percent")]
    threshold: Option<LegacyThresholdFraction>,

    /// Fail on warning-or-error diagnostics
    #[arg(long)]
    fail_on_issues: bool,

    /// Fail on diagnostics at info, warning, or error
    #[arg(long)]
    deny_diagnostics: Option<DiagnosticSeverity>,

    // ========================================================================
    // Build Options
    // ========================================================================
    /// Run in debug mode instead of release mode (not recommended for benchmarks)
    #[arg(long)]
    dev: bool,

    /// One additional argument to pass to Cargo; repeat for multiple arguments
    #[arg(long = "cargo-arg", value_name = "ARG", allow_hyphen_values = true)]
    cargo_args: Vec<String>,

    /// Comma-separated Cargo features to enable; may be repeated
    #[arg(
        long,
        value_name = "FEATURES",
        value_delimiter = ',',
        conflicts_with = "all_features"
    )]
    features: Vec<String>,

    /// Enable every Cargo feature
    #[arg(long, conflicts_with_all = ["features", "no_default_features"])]
    all_features: bool,

    /// Disable Cargo default features
    #[arg(long, conflicts_with = "all_features")]
    no_default_features: bool,

    /// Build and run for a Cargo target triple using its configured runner
    #[arg(long, value_name = "TRIPLE")]
    target: Option<String>,

    /// Override Cargo's target directory
    #[arg(long, value_name = "PATH")]
    target_dir: Option<PathBuf>,

    /// Package to run stress tests for (in a workspace)
    #[arg(long, short = 'p')]
    package: Option<String>,

    /// Path to Cargo.toml
    #[arg(long)]
    manifest_path: Option<PathBuf>,

    // ========================================================================
    // Advanced
    // ========================================================================
    /// Legacy option; rejected because artifact discovery requires a Cargo build
    #[arg(long)]
    no_build: bool,

    /// Keep going even if a stress binary fails
    #[arg(long)]
    no_fail_fast: bool,
}

// ============================================================================
// Cargo Metadata and Stress Targets
// ============================================================================

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
    #[serde(default)]
    workspace_default_members: Vec<String>,
    workspace_root: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    version: String,
    id: String,
    manifest_path: PathBuf,
    targets: Vec<CargoTarget>,
    #[serde(default)]
    dependencies: Vec<CargoDependency>,
}

#[derive(Debug, Deserialize)]
struct CargoDependency {
    name: String,
    #[serde(default)]
    rename: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
    #[serde(default)]
    required_features: Vec<String>,
}

#[derive(Debug, Clone)]
struct StressTarget {
    package_id: String,
    package_name: String,
    package_version: String,
    name: String,
    path: PathBuf,
    required_features: Vec<String>,
}

impl StressTarget {
    fn label(&self) -> String {
        format!("{}::{}", self.package_name, self.name)
    }

    fn package_spec(&self) -> String {
        format!("{}@{}", self.package_name, self.package_version)
    }

    fn suite_name(&self) -> String {
        cntryl_stress::__private::canonical_suite_name(&self.name)
    }

    fn artifact_namespace(&self) -> String {
        portable_suite_component(&self.package_name)
    }

    fn matches_filter(&self, filter: &str) -> bool {
        let stem = self.path.file_stem().and_then(|stem| stem.to_str());
        self.name == filter
            || stem.is_some_and(|stem| stem == filter || format!("stress_{stem}") == filter)
    }
}

fn portable_suite_component(value: &str) -> String {
    let component = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if component.is_empty() {
        "stress".to_string()
    } else {
        component
    }
}

#[derive(Debug, Clone)]
struct BuiltStressTarget {
    target: StressTarget,
    executable: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: String,
    #[serde(default)]
    package_id: Option<String>,
    #[serde(default)]
    target: Option<CargoMessageTarget>,
    #[serde(default)]
    executable: Option<PathBuf>,
    #[serde(default)]
    message: Option<CargoCompilerMessage>,
}

#[derive(Debug, Deserialize)]
struct CargoMessageTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CargoCompilerMessage {
    #[serde(default)]
    rendered: Option<String>,
}

// ============================================================================
// Execution Result
// ============================================================================

/// Result of running a single stress binary.
#[derive(Debug)]
struct StressRunResult {
    target: StressTarget,
    status: ExitStatus,
    duration: Duration,
    stdout: String,
    stderr: String,
    run: Option<StressRun>,
    result_error: Option<String>,
    gate_error: Option<String>,
}

impl StressRunResult {
    fn success(&self) -> bool {
        self.status.success() && self.result_error.is_none() && self.gate_error.is_none()
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
    validate_stress_args(args)?;
    let verbosity = Verbosity::from_args(args);
    let passthrough_json = !args.list && !args.print_config;

    // Step 1: Resolve Cargo's package and target model.
    let manifest_path = find_manifest(args)?
        .canonicalize()
        .context("Failed to resolve manifest path")?;
    let metadata = load_cargo_metadata(&manifest_path, args)?;
    let build_identity = wrapper_build_input_identity(args, &metadata.workspace_root)?;
    let stress_targets = discover_stress_targets(&metadata, &manifest_path, args)?;

    // Step 2: Build the real Cargo bench targets and capture their executable paths.
    let built_targets = build_stress_binaries(&stress_targets, args, &manifest_path)?;

    // Step 3: Prove each artifact is the custom stress harness, and select only
    // targets with a locally matching registered workload before measurement.
    let run_id = shared_run_id();
    let built_targets = if passthrough_json {
        select_stress_harnesses(
            built_targets,
            args,
            &manifest_path,
            &run_id,
            &build_identity,
        )?
    } else {
        verify_stress_harnesses(
            &built_targets,
            args,
            &manifest_path,
            &run_id,
            &build_identity,
        )?;
        built_targets
    };
    let selected_targets = built_targets
        .iter()
        .map(|built| built.target.clone())
        .collect::<Vec<_>>();
    validate_baseline_target_selection(args, &selected_targets)?;

    // Step 4: Run stress binaries
    let results = run_stress_binaries(
        &built_targets,
        args,
        passthrough_json,
        &manifest_path,
        &run_id,
        &build_identity,
    )?;

    // Step 5: Report results and verify the execution ledger.
    report_results(&results, args.json, verbosity, passthrough_json)?;
    ensure_selected_binaries_executed(&selected_targets, &results)?;

    // Step 6: Exit with appropriate code
    let failed_count = results.iter().filter(|r| !r.success()).count();
    if failed_count > 0 {
        bail!("{failed_count} stress binary or result failed");
    }

    Ok(())
}

fn validate_stress_args(args: &StressArgs) -> Result<()> {
    if args.no_build {
        bail!(
            "--no-build is not supported because cargo-stress relies on Cargo's artifact stream to locate the exact bench executable. Remove --no-build; Cargo will reuse unchanged build artifacts automatically."
        );
    }
    if args.threshold_percent.is_some() && args.threshold.is_some() {
        bail!("choose either --threshold-percent or legacy --threshold, not both");
    }
    validate_cargo_args(&args.cargo_args)
}

fn validate_cargo_args(arguments: &[String]) -> Result<()> {
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        match argument.as_str() {
            "--locked"
            | "--offline"
            | "--frozen"
            | "--ignore-rust-version"
            | "--verbose"
            | "--quiet"
            | "-q"
            | "--timings" => {}
            value
                if value.len() > 1
                    && value.starts_with('-')
                    && value[1..].chars().all(|ch| ch == 'v') => {}
            "--jobs" | "-j" => {
                index += 1;
                let value = arguments.get(index).with_context(|| {
                    format!("--cargo-arg {argument} requires a following --cargo-arg <JOBS> value")
                })?;
                validate_jobs_value(value)?;
            }
            value if value.starts_with("--jobs=") => {
                validate_jobs_value(value.trim_start_matches("--jobs="))?;
            }
            value if value.starts_with("-j") && value.len() > 2 => {
                validate_jobs_value(&value[2..])?;
            }
            "--color" => {
                index += 1;
                let value = arguments.get(index).with_context(|| {
                    "--cargo-arg --color requires a following --cargo-arg <auto|always|never> value"
                })?;
                validate_color_value(value)?;
            }
            value if value.starts_with("--color=") => {
                validate_color_value(value.trim_start_matches("--color="))?;
            }
            value if value.starts_with("--timings=") && value.len() > "--timings=".len() => {}
            value if value.split('=').next() == Some("--message-format") => {
                bail!(
                    "--cargo-arg cannot override --message-format because cargo-stress requires Cargo's JSON artifact stream"
                );
            }
            value if value.starts_with("-F") || value.split('=').next() == Some("--features") => {
                bail!(
                    "Cargo flag {value} has a first-class cargo-stress option; pass it directly with --features instead of through --cargo-arg"
                );
            }
            value
                if matches!(
                    value.split('=').next().unwrap_or(value),
                    "--all-features" | "--no-default-features" | "--target-dir"
                ) =>
            {
                bail!(
                    "Cargo flag {value} has a first-class cargo-stress option; pass it directly instead of through --cargo-arg"
                );
            }
            value if value.split('=').next() == Some("--target") => {
                bail!(
                    "Cargo flag {value} has a first-class cargo-stress option; pass it directly with --target instead of through --cargo-arg"
                );
            }
            value if value.starts_with('-') => {
                bail!(
                    "--cargo-arg {value} is not in the resolution-safe allowlist. Supported Cargo arguments are --locked, --offline, --frozen, --jobs/-j, verbosity/quiet, --color, --timings, and --ignore-rust-version"
                );
            }
            value => {
                bail!(
                    "--cargo-arg positional value {value:?} is not allowed unless it is the value of --jobs or --color"
                );
            }
        }
        index += 1;
    }
    Ok(())
}

fn validate_jobs_value(value: &str) -> Result<()> {
    let jobs = value
        .parse::<i32>()
        .with_context(|| format!("Cargo jobs value {value:?} must be a non-zero integer"))?;
    if jobs == 0 {
        bail!("Cargo jobs value must be a non-zero integer");
    }
    Ok(())
}

fn validate_color_value(value: &str) -> Result<()> {
    if matches!(value, "auto" | "always" | "never") {
        Ok(())
    } else {
        bail!("Cargo color value {value:?} must be auto, always, or never")
    }
}

fn validate_baseline_target_selection(args: &StressArgs, targets: &[StressTarget]) -> Result<()> {
    let Some(baseline) = &args.baseline else {
        return Ok(());
    };
    if baseline.as_os_str() == "latest" || targets.len() == 1 {
        return Ok(());
    }

    let selected = targets
        .iter()
        .map(StressTarget::label)
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "--baseline {} is an explicit artifact and requires exactly one selected Cargo stress bench target, but {} were selected: {}. Select one with --package <PACKAGE> and --bench <BENCH>, or use --baseline latest for per-target baselines.",
        baseline.display(),
        targets.len(),
        selected,
    )
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

    const fn is_quiet(self) -> bool {
        matches!(self, Verbosity::Quiet)
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
// Cargo Target Discovery
// ============================================================================

fn load_cargo_metadata(manifest_path: &Path, args: &StressArgs) -> Result<CargoMetadata> {
    let mut command = Command::new("cargo");
    command
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .arg("--no-deps")
        .arg("--manifest-path")
        .arg(manifest_path);
    apply_cargo_resolution_args(&mut command, args);
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("Failed to run cargo metadata")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "cargo metadata failed for {}: {}",
            manifest_path.display(),
            stderr.trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("Failed to parse cargo metadata JSON")
}

fn apply_cargo_resolution_args(cmd: &mut Command, args: &StressArgs) {
    if let Some(target) = &args.target {
        cmd.arg("--filter-platform").arg(target);
    }
    for argument in &args.cargo_args {
        if matches!(argument.as_str(), "--locked" | "--offline" | "--frozen") {
            cmd.arg(argument);
        }
    }
}

fn selected_packages<'a>(
    metadata: &'a CargoMetadata,
    manifest_path: &Path,
    args: &StressArgs,
) -> Result<Vec<&'a CargoPackage>> {
    if let Some(spec) = &args.package {
        let matches = metadata
            .packages
            .iter()
            .filter(|package| {
                package.name == *spec
                    || format!("{}@{}", package.name, package.version) == *spec
                    || package.id == *spec
            })
            .collect::<Vec<_>>();
        if matches.is_empty() {
            let available = metadata
                .packages
                .iter()
                .filter(|package| metadata.workspace_members.contains(&package.id))
                .map(|package| package.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!("package selector '{spec}' did not match a workspace package; available: {available}");
        }
        if matches.len() > 1 {
            bail!("package selector '{spec}' is ambiguous; use name@version");
        }
        return Ok(matches);
    }

    if let Some(package) = metadata
        .packages
        .iter()
        .find(|package| package.manifest_path == manifest_path)
    {
        return Ok(vec![package]);
    }

    let selected_members = if metadata.workspace_default_members.is_empty() {
        &metadata.workspace_members
    } else {
        &metadata.workspace_default_members
    };
    let mut packages = metadata
        .packages
        .iter()
        .filter(|package| selected_members.contains(&package.id))
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    if packages.is_empty() {
        bail!(
            "cargo metadata selected no packages for {}",
            manifest_path.display()
        );
    }
    Ok(packages)
}

#[derive(Debug)]
struct StressUseBinding {
    source: Vec<String>,
    local: Option<String>,
    glob: bool,
}

fn has_cfg_attribute(attributes: &[syn::Attribute]) -> bool {
    attributes
        .iter()
        .any(|attribute| attribute.path().is_ident("cfg") || attribute.path().is_ident("cfg_attr"))
}

fn collect_stress_use_bindings(
    tree: &syn::UseTree,
    prefix: &[String],
    bindings: &mut Vec<StressUseBinding>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            let mut next = prefix.to_vec();
            next.push(path.ident.to_string());
            collect_stress_use_bindings(&path.tree, &next, bindings);
        }
        syn::UseTree::Name(name) => {
            let mut source = prefix.to_vec();
            if name.ident != "self" {
                source.push(name.ident.to_string());
            }
            let local = if name.ident == "self" {
                source.last().cloned()
            } else {
                Some(name.ident.to_string())
            };
            bindings.push(StressUseBinding {
                source,
                local,
                glob: false,
            });
        }
        syn::UseTree::Rename(rename) => {
            let mut source = prefix.to_vec();
            if rename.ident != "self" {
                source.push(rename.ident.to_string());
            }
            bindings.push(StressUseBinding {
                source,
                local: Some(rename.rename.to_string()),
                glob: false,
            });
        }
        syn::UseTree::Glob(_) => bindings.push(StressUseBinding {
            source: prefix.to_vec(),
            local: None,
            glob: true,
        }),
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                collect_stress_use_bindings(tree, prefix, bindings);
            }
        }
    }
}

fn normalized_crate_name(name: &str) -> String {
    name.replace('-', "_")
}

fn stress_crate_roots(package: &CargoPackage) -> BTreeSet<String> {
    let mut roots = BTreeSet::new();
    if package.name == "cntryl-stress" {
        roots.insert("cntryl_stress".to_string());
    }
    roots.extend(
        package
            .dependencies
            .iter()
            .filter(|dependency| {
                dependency.name == "cntryl-stress" && dependency.kind.as_deref() != Some("build")
            })
            .map(|dependency| {
                normalized_crate_name(
                    dependency
                        .rename
                        .as_deref()
                        .unwrap_or(dependency.name.as_str()),
                )
            }),
    );
    roots
}

fn supported_stress_macro_path(
    path: &syn::Path,
    crate_aliases: &BTreeSet<String>,
    macro_aliases: &BTreeSet<String>,
    local_macros: &BTreeSet<String>,
) -> bool {
    let segments = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>();
    match segments.as_slice() {
        [root, macro_name] => macro_name == "stress_main" && crate_aliases.contains(root.as_str()),
        [macro_name] => {
            macro_aliases.contains(macro_name.as_str())
                && !local_macros.contains(macro_name.as_str())
        }
        _ => false,
    }
}

fn stress_crate_aliases(
    file: &syn::File,
    bindings: &[StressUseBinding],
    declared_roots: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut aliases = declared_roots.clone();
    loop {
        let mut changed = false;
        for item in &file.items {
            if let syn::Item::ExternCrate(extern_crate) = item {
                if has_cfg_attribute(&extern_crate.attrs)
                    || !aliases.contains(extern_crate.ident.to_string().as_str())
                {
                    continue;
                }
                let local = extern_crate
                    .rename
                    .as_ref()
                    .map_or(&extern_crate.ident, |(_, rename)| rename)
                    .to_string();
                changed |= aliases.insert(local);
            }
        }
        for binding in bindings {
            if binding.source.len() == 1
                && aliases.contains(binding.source[0].as_str())
                && binding
                    .local
                    .as_ref()
                    .is_some_and(|local| aliases.insert(local.clone()))
            {
                changed = true;
            }
        }
        if !changed {
            return aliases;
        }
    }
}

fn stress_macro_aliases(
    bindings: &[StressUseBinding],
    crate_aliases: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut aliases = BTreeSet::new();
    loop {
        let mut changed = false;
        for binding in bindings {
            let imports_stress_main = match binding.source.as_slice() {
                [root, macro_name] => {
                    crate_aliases.contains(root.as_str()) && macro_name == "stress_main"
                }
                [macro_alias] => aliases.contains(macro_alias.as_str()),
                _ => false,
            };
            if imports_stress_main
                && binding
                    .local
                    .as_ref()
                    .is_some_and(|local| aliases.insert(local.clone()))
            {
                changed = true;
            }
            if binding.glob
                && binding.source.len() == 1
                && crate_aliases.contains(binding.source[0].as_str())
            {
                changed |= aliases.insert("stress_main".to_string());
            }
        }
        if !changed {
            return aliases;
        }
    }
}

/// Recognize entrypoints that can be attributed to a declared `cntryl-stress`
/// crate without compiling or executing the candidate bench. Supported forms
/// are unconditional, top-level qualified invocations; explicit direct,
/// renamed, grouped, or glob imports; and `use`/`extern crate` aliases.
/// Nested, conditional, locally generated, and unattributed bare invocations
/// intentionally fail closed.
fn has_supported_stress_entrypoint(
    source: &str,
    declared_roots: &BTreeSet<String>,
) -> std::result::Result<bool, String> {
    let file = syn::parse_file(source)
        .map_err(|error| format!("source could not be parsed as Rust: {error}"))?;
    let mut bindings = Vec::new();
    for item in &file.items {
        if let syn::Item::Use(item_use) = item {
            if !has_cfg_attribute(&item_use.attrs) {
                collect_stress_use_bindings(&item_use.tree, &[], &mut bindings);
            }
        }
    }

    let crate_aliases = stress_crate_aliases(&file, &bindings, declared_roots);
    let macro_aliases = stress_macro_aliases(&bindings, &crate_aliases);

    let local_macros = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Macro(item_macro) => item_macro.ident.as_ref().map(ToString::to_string),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut conditional_entrypoint = false;
    for item in &file.items {
        let syn::Item::Macro(item_macro) = item else {
            continue;
        };
        if item_macro.ident.is_some()
            || !supported_stress_macro_path(
                &item_macro.mac.path,
                &crate_aliases,
                &macro_aliases,
                &local_macros,
            )
        {
            continue;
        }
        if has_cfg_attribute(&item_macro.attrs) {
            conditional_entrypoint = true;
        } else {
            return Ok(true);
        }
    }
    if conditional_entrypoint {
        Err(
            "recognized stress_main! only behind #[cfg]/#[cfg_attr]; use one unconditional top-level entrypoint"
                .to_string(),
        )
    } else {
        Ok(false)
    }
}

fn discover_stress_targets(
    metadata: &CargoMetadata,
    manifest_path: &Path,
    args: &StressArgs,
) -> Result<Vec<StressTarget>> {
    let packages = selected_packages(metadata, manifest_path, args)?;
    let selected_package_names = packages
        .iter()
        .map(|package| package.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut targets = Vec::new();
    let mut rejected_candidates = Vec::new();
    for package in packages {
        let declared_roots = stress_crate_roots(package);
        for target in &package.targets {
            if !target.kind.iter().any(|kind| kind == "bench") {
                continue;
            }
            let stress_target = StressTarget {
                package_id: package.id.clone(),
                package_name: package.name.clone(),
                package_version: package.version.clone(),
                name: target.name.clone(),
                path: target.src_path.clone(),
                required_features: target.required_features.clone(),
            };
            if args
                .bin
                .as_ref()
                .is_some_and(|filter| !stress_target.matches_filter(filter))
            {
                continue;
            }
            let source = fs::read_to_string(&target.src_path).with_context(|| {
                format!(
                    "Failed to read Cargo bench target {}",
                    target.src_path.display()
                )
            })?;
            match has_supported_stress_entrypoint(&source, &declared_roots) {
                Ok(true) => targets.push(stress_target),
                Ok(false) => rejected_candidates.push(format!(
                    "{} ({})",
                    target.src_path.display(),
                    "no supported unconditional top-level cntryl-stress stress_main! entrypoint"
                )),
                Err(reason) => {
                    rejected_candidates.push(format!("{} ({reason})", target.src_path.display()));
                }
            }
        }
    }
    targets.sort_by(|left, right| {
        left.package_name
            .cmp(&right.package_name)
            .then(left.name.cmp(&right.name))
    });
    let mut canonical_suites = BTreeMap::new();
    for target in &targets {
        let key = (target.package_name.clone(), target.suite_name());
        if let Some(previous) = canonical_suites.insert(key.clone(), target.name.clone()) {
            bail!(
                "Cargo stress bench targets {previous:?} and {:?} in package {:?} both canonicalize to suite {:?}. Rename one target so artifact and baseline paths cannot collide.",
                target.name,
                target.package_name,
                key.1,
            );
        }
    }
    if targets.is_empty() {
        let inspected = if rejected_candidates.is_empty() {
            " No matching Cargo bench source was inspected.".to_string()
        } else {
            format!(
                " Inspected and rejected: {}.",
                rejected_candidates.join("; ")
            )
        };
        let supported = " Supported entrypoints are unconditional top-level invocations through the package's declared cntryl-stress dependency: `crate_name::stress_main!()`, an explicit direct/renamed/grouped/glob `use` import, or a `use`/`extern crate` crate alias. Conditional, nested, and macro-generated entrypoints are intentionally not executed; move stress_main! to the crate root.";
        if let Some(filter) = &args.bin {
            bail!(
                "No Cargo stress bench target matched --bench/--bin {filter} in package(s): {selected_package_names}.{inspected}{supported}"
            );
        }
        bail!(
            "No stress test files found as Cargo bench targets in package(s): {selected_package_names}. Declare each stress entrypoint as [[bench]] with harness = false.{inspected}{supported}"
        );
    }
    Ok(targets)
}

// ============================================================================
// Build Phase
// ============================================================================

/// Build every real Cargo bench target and capture its exact executable path.
fn build_stress_binaries(
    targets: &[StressTarget],
    args: &StressArgs,
    manifest_path: &Path,
) -> Result<Vec<BuiltStressTarget>> {
    let mut built = Vec::with_capacity(targets.len());
    for target in targets {
        let mut cmd = Command::new("cargo");
        if args.dev {
            cmd.arg("build");
        } else {
            cmd.arg("bench").arg("--no-run");
        }
        cmd.arg("--manifest-path")
            .arg(manifest_path)
            .arg("--package")
            .arg(target.package_spec())
            .arg("--bench")
            .arg(&target.name);
        apply_cargo_args(&mut cmd, args);
        cmd.arg("--message-format=json-render-diagnostics");

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to build Cargo bench target {}", target.label()))?;
        let messages = parse_cargo_messages(&output.stdout);
        if !output.status.success() {
            eprintln!("Build failed for {}.", target.label());
            for rendered in messages.iter().filter_map(|message| {
                message
                    .message
                    .as_ref()
                    .and_then(|diagnostic| diagnostic.rendered.as_deref())
            }) {
                eprint!("{rendered}");
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                eprintln!("{stderr}");
            }
            bail!(
                "Cargo failed to build stress bench target {} with exit code {:?}",
                target.label(),
                output.status.code()
            );
        }
        let executable = messages
            .iter()
            .find(|message| cargo_message_matches_target(message, target))
            .and_then(|message| message.executable.clone())
            .with_context(|| missing_artifact_message(target))?;
        built.push(BuiltStressTarget {
            target: target.clone(),
            executable,
        });
    }
    Ok(built)
}

fn apply_cargo_args(cmd: &mut Command, args: &StressArgs) {
    if !args.features.is_empty() {
        cmd.arg("--features").arg(args.features.join(","));
    }
    if args.all_features {
        cmd.arg("--all-features");
    }
    if args.no_default_features {
        cmd.arg("--no-default-features");
    }
    if let Some(target_dir) = &args.target_dir {
        cmd.arg("--target-dir").arg(target_dir);
    }
    if let Some(target) = &args.target {
        cmd.arg("--target").arg(target);
    }
    for argument in &args.cargo_args {
        cmd.arg(argument);
    }
}

fn parse_cargo_messages(stdout: &[u8]) -> Vec<CargoMessage> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn cargo_message_matches_target(message: &CargoMessage, target: &StressTarget) -> bool {
    message.reason == "compiler-artifact"
        && message.package_id.as_deref() == Some(target.package_id.as_str())
        && message.target.as_ref().is_some_and(|message_target| {
            message_target.name == target.name
                && message_target.kind.iter().any(|kind| kind == "bench")
        })
        && message.executable.is_some()
}

fn missing_artifact_message(target: &StressTarget) -> String {
    if target.required_features.is_empty() {
        format!(
            "Cargo did not emit an executable for {}. Ensure the target is declared with [[bench]] and harness = false.",
            target.label()
        )
    } else {
        format!(
            "Cargo did not emit an executable for {}. Required features: {}. Enable them with --features {}.",
            target.label(),
            target.required_features.join(", "),
            target.required_features.join(",")
        )
    }
}

fn cargo_bench_command(
    target: &StressTarget,
    args: &StressArgs,
    manifest_path: &Path,
    run_id: &str,
    build_identity: &str,
) -> Command {
    let mut command = Command::new("cargo");
    command
        .arg("bench")
        .arg("--manifest-path")
        .arg(manifest_path)
        .arg("--package")
        .arg(target.package_spec())
        .arg("--bench")
        .arg(&target.name);
    if args.dev {
        command.arg("--profile").arg("dev");
    }
    apply_cargo_args(&mut command, args);
    apply_run_id_env(&mut command, run_id);
    apply_stress_env(&mut command, args, target, build_identity);
    command.arg("--");
    command
}

fn verify_stress_harnesses(
    targets: &[BuiltStressTarget],
    args: &StressArgs,
    manifest_path: &Path,
    run_id: &str,
    build_identity: &str,
) -> Result<()> {
    for built in targets {
        let mut cmd =
            cargo_bench_command(&built.target, args, manifest_path, run_id, build_identity);
        let output = cmd
            .arg("--print-config")
            .arg("--no-progress")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to verify stress harness {}", built.target.label()))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success()
            || !stdout.contains("Benchmark Suite:")
            || !stdout.contains("Profile:")
        {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "{} is not an executable cntryl-stress harness. Declare it with [[bench]] and harness = false, and end the source with stress_main!(). Child stderr: {}",
                built.target.label(),
                stderr.trim()
            );
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct HarnessSelectionProbe {
    workload: Option<String>,
    tier: Option<u32>,
    include_ignored: bool,
    selected: Vec<HarnessSelectionBenchmark>,
    registered: Vec<HarnessSelectionBenchmark>,
}

#[derive(Debug, Deserialize)]
struct HarnessSelectionBenchmark {
    name: String,
    function_name: String,
    module_path: String,
    tier: u32,
    ignored: bool,
}

fn select_stress_harnesses(
    targets: Vec<BuiltStressTarget>,
    args: &StressArgs,
    manifest_path: &Path,
    run_id: &str,
    build_identity: &str,
) -> Result<Vec<BuiltStressTarget>> {
    let mut selected = Vec::new();
    let mut inventories = Vec::with_capacity(targets.len());
    for built in targets {
        if !built.executable.exists() {
            bail!(
                "Binary not found for {} at {}. Cargo reported an artifact that is no longer present.",
                built.target.label(),
                built.executable.display()
            );
        }

        let mut command =
            cargo_bench_command(&built.target, args, manifest_path, run_id, build_identity);
        build_passthrough_args(&mut command, args, false);
        let output = command
            .arg("--__cntryl-stress-selection-probe")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| {
                format!(
                    "Failed to probe registered workloads in {}",
                    built.target.label()
                )
            })?;
        if !output.status.success() {
            bail!(
                "Could not probe registered workloads in {}. Ensure it is a cntryl-stress harness and its configuration is valid. Child stderr: {}",
                built.target.label(),
                String::from_utf8_lossy(&output.stderr).trim(),
            );
        }
        let probe =
            serde_json::from_slice::<HarnessSelectionProbe>(&output.stdout).with_context(|| {
                format!(
                    "{} did not emit a valid cntryl-stress selection probe; stdout: {}",
                    built.target.label(),
                    String::from_utf8_lossy(&output.stdout).trim(),
                )
            })?;
        let is_selected = !probe.selected.is_empty();
        inventories.push((built.target.label(), probe));
        if is_selected {
            selected.push(built);
        }
    }

    if selected.is_empty() {
        bail!(global_selection_error(&inventories));
    }
    Ok(selected)
}

fn global_selection_error(inventories: &[(String, HarnessSelectionProbe)]) -> String {
    let selector = inventories.first().map_or_else(
        || "the resolved selection".to_string(),
        |(_, probe)| {
            let mut parts = Vec::new();
            if let Some(workload) = &probe.workload {
                parts.push(format!("workload {workload:?}"));
            }
            if let Some(tier) = probe.tier {
                parts.push(format!("tier {tier}"));
            }
            if !probe.include_ignored {
                parts.push("ignored benchmarks excluded".to_string());
            }
            if parts.is_empty() {
                "the resolved selection".to_string()
            } else {
                parts.join(", ")
            }
        },
    );
    let mut candidates = inventories
        .iter()
        .flat_map(|(target, probe)| {
            probe.registered.iter().map(move |benchmark| {
                let ignored = if benchmark.ignored { ", ignored" } else { "" };
                format!(
                    "{target} -> {}::{} (display {:?}, tier {}{ignored})",
                    benchmark.module_path, benchmark.function_name, benchmark.name, benchmark.tier,
                )
            })
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    let omitted = candidates.len().saturating_sub(24);
    candidates.truncate(24);
    let mut message = format!(
        "No registered benchmark matched {selector} across {} Cargo stress bench target(s).",
        inventories.len()
    );
    if !candidates.is_empty() {
        message.push_str(" Available candidates: ");
        message.push_str(&candidates.join("; "));
        if omitted != 0 {
            message.push_str("; and ");
            message.push_str(&omitted.to_string());
            message.push_str(" more");
        }
    }
    message
}

// ============================================================================
// Execution Phase
// ============================================================================

/// Run all stress binaries and collect results.
fn run_stress_binaries(
    targets: &[BuiltStressTarget],
    args: &StressArgs,
    passthrough_json: bool,
    manifest_path: &Path,
    run_id: &str,
    build_identity: &str,
) -> Result<Vec<StressRunResult>> {
    let mut results = Vec::new();

    for built in targets {
        if !built.executable.exists() {
            bail!(
                "Binary not found for {} at {}. Cargo reported an artifact that is no longer present.",
                built.target.label(),
                built.executable.display()
            );
        }

        let result = run_single_binary(
            &built.target,
            args,
            passthrough_json,
            manifest_path,
            run_id,
            build_identity,
        )?;

        let failed = !result.success();
        results.push(result);

        // Fail fast unless --no-fail-fast
        if failed && !args.no_fail_fast {
            break;
        }
    }

    Ok(results)
}

fn ensure_selected_binaries_executed(
    targets: &[StressTarget],
    results: &[StressRunResult],
) -> Result<()> {
    let not_executed = targets
        .iter()
        .filter(|target| {
            !results.iter().any(|result| {
                result.target.package_id == target.package_id && result.target.name == target.name
            })
        })
        .map(StressTarget::label)
        .collect::<Vec<_>>();
    if !not_executed.is_empty() {
        bail!(
            "selected stress binaries were not executed: {}",
            not_executed.join(", ")
        );
    }
    Ok(())
}

/// Run a single stress binary and capture output.
fn run_single_binary(
    target: &StressTarget,
    args: &StressArgs,
    passthrough_json: bool,
    manifest_path: &Path,
    run_id: &str,
    build_identity: &str,
) -> Result<StressRunResult> {
    let mut cmd = cargo_bench_command(target, args, manifest_path, run_id, build_identity);

    // Pass through all relevant arguments to the stress binary
    // These are handled by the stress_main!() macro via clap parsing
    build_passthrough_args(&mut cmd, args, passthrough_json);

    let start = Instant::now();

    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("Failed to execute {} through Cargo", target.label()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let (run, result_error, gate_error) = if passthrough_json {
        match parse_and_validate_child_run(&stdout, target, run_id, build_identity) {
            Ok(run) => {
                let gate = evaluate_run_gate(&run);
                let gate_error = (gate != RunGate::Passed)
                    .then(|| format!("serialized run did not pass its recorded gate: {gate:?}"));
                (Some(run), None, gate_error)
            }
            Err(error) => (None, Some(error), None),
        }
    } else {
        (None, None, None)
    };

    let output = StressRunResult {
        target: target.clone(),
        status: output.status,
        duration: start.elapsed(),
        stdout,
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        run,
        result_error,
        gate_error,
    };

    Ok(output)
}

fn parse_and_validate_child_run(
    stdout: &str,
    target: &StressTarget,
    run_id: &str,
    expected_build_identity: &str,
) -> std::result::Result<StressRun, String> {
    let run = StressRun::from_json_str(stdout).map_err(|error| format!("invalid JSON: {error}"))?;
    if run.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported schema_version {:?}; expected {SCHEMA_VERSION:?}",
            run.schema_version
        ));
    }
    run.validate_canonical_evidence()
        .map_err(|error| format!("non-canonical stress evidence: {error}"))?;
    if run.suite != target.suite_name() {
        return Err(format!(
            "suite {:?} does not match selected Cargo bench target suite {:?}",
            run.suite,
            target.suite_name()
        ));
    }
    if run.metadata.get("run_id").map(String::as_str) != Some(run_id) {
        return Err(format!(
            "metadata.run_id does not match wrapper run id {run_id:?}"
        ));
    }
    if run.environment.build_profile != expected_build_identity {
        return Err(format!(
            "build identity {:?} does not match wrapper build inputs {:?}",
            run.environment.build_profile, expected_build_identity
        ));
    }
    let namespace = run
        .metadata
        .get("artifact_namespace")
        .ok_or_else(|| "wrapper receipt is missing metadata.artifact_namespace".to_string())?;
    if namespace != &target.artifact_namespace() {
        return Err(format!(
            "artifact namespace {namespace:?} does not match selected package namespace {:?}",
            target.artifact_namespace()
        ));
    }
    if run.benchmark_specs.is_empty() || run.samples.is_empty() || run.summaries.is_empty() {
        return Err(
            "stress result must contain non-empty specs, raw samples, and summaries".to_string(),
        );
    }
    Ok(run)
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

fn apply_stress_env(
    cmd: &mut Command,
    args: &StressArgs,
    target: &StressTarget,
    build_identity: &str,
) {
    cmd.env("STRESS_SUITE", target.suite_name());
    cmd.env("STRESS_ARTIFACT_NAMESPACE", target.artifact_namespace());
    cmd.env("STRESS_BUILD_INPUT_IDENTITY", build_identity);
    if let Some(timeout) = args.timeout_secs {
        cmd.env("STRESS_TIMEOUT_SECS", timeout.get().to_string());
    }
}

fn wrapper_build_input_identity(args: &StressArgs, workspace_root: &Path) -> Result<String> {
    let current_dir =
        std::env::current_dir().context("Failed to resolve the Cargo working directory")?;
    let cargo_home = cargo_home_from_environment(&current_dir);
    wrapper_build_input_identity_with_context(
        args,
        workspace_root,
        &current_dir,
        cargo_home.as_deref(),
        std::env::vars_os().filter_map(|(key, value)| {
            Some((
                key.into_string().ok()?,
                value.to_string_lossy().into_owned(),
            ))
        }),
    )
}

#[cfg(test)]
fn wrapper_build_input_identity_with_env<'a>(
    args: &StressArgs,
    environment: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> String {
    wrapper_build_input_identity_from_pairs(
        args,
        environment
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string())),
        std::iter::empty(),
    )
}

fn wrapper_build_input_identity_with_context(
    args: &StressArgs,
    workspace_root: &Path,
    current_dir: &Path,
    cargo_home: Option<&Path>,
    environment: impl IntoIterator<Item = (String, String)>,
) -> Result<String> {
    let config_inputs = build_config_input_receipts(args, workspace_root, current_dir, cargo_home)?;
    Ok(wrapper_build_input_identity_from_pairs(
        args,
        environment,
        config_inputs,
    ))
}

fn wrapper_build_input_identity_from_pairs(
    args: &StressArgs,
    environment: impl IntoIterator<Item = (String, String)>,
    config_inputs: impl IntoIterator<Item = (String, String)>,
) -> String {
    let profile = if args.dev { "debug" } else { "release" };
    let mut features = args.features.clone();
    features.sort();
    features.dedup();
    let ambient = environment
        .into_iter()
        .filter(|(key, _)| material_build_environment_key(key, args))
        .map(|(key, value)| (key, stable_environment_value_fingerprint(&value)))
        .collect::<BTreeMap<_, _>>();
    let config_inputs = config_inputs
        .into_iter()
        .map(|(source, fingerprint)| {
            serde_json::json!({
                "source": source,
                "fingerprint": fingerprint,
            })
        })
        .collect::<Vec<_>>();
    if features.is_empty()
        && !args.all_features
        && !args.no_default_features
        && args.target.is_none()
        && ambient.is_empty()
        && config_inputs.is_empty()
    {
        return profile.to_string();
    }
    let mut inputs = serde_json::Map::new();
    inputs.insert("features".to_string(), serde_json::json!(features));
    inputs.insert(
        "all_features".to_string(),
        serde_json::json!(args.all_features),
    );
    inputs.insert(
        "default_features".to_string(),
        serde_json::json!(!args.no_default_features),
    );
    inputs.insert("target".to_string(), serde_json::json!(args.target));
    if !ambient.is_empty() {
        inputs.insert("ambient".to_string(), serde_json::json!(ambient));
    }
    if !config_inputs.is_empty() {
        inputs.insert(
            "config_inputs".to_string(),
            serde_json::Value::Array(config_inputs),
        );
    }
    let inputs = serde_json::Value::Object(inputs);
    format!("{profile};cargo-stress={inputs}")
}

fn cargo_home_from_environment(current_dir: &Path) -> Option<PathBuf> {
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME").filter(|value| !value.is_empty()) {
        let cargo_home = PathBuf::from(cargo_home);
        return Some(if cargo_home.is_absolute() {
            cargo_home
        } else {
            current_dir.join(cargo_home)
        });
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .map(|home| home.join(".cargo"))
}

fn build_config_input_receipts(
    args: &StressArgs,
    workspace_root: &Path,
    current_dir: &Path,
    cargo_home: Option<&Path>,
) -> Result<Vec<(String, String)>> {
    let mut receipts = Vec::new();
    if let Some(profile) = relevant_manifest_profile(&workspace_root.join("Cargo.toml"))? {
        receipts.push((
            "workspace-profile".to_string(),
            fingerprint_json_value(&profile)?,
        ));
    }

    let mut seen = BTreeSet::new();
    if let Some(cargo_home) = cargo_home {
        if let Some(path) = cargo_config_path(cargo_home)? {
            seen.insert(stable_source_path(&path));
            if let Some(config) = relevant_cargo_config_tree(&path, args, &mut Vec::new())? {
                receipts.push(("cargo-home".to_string(), fingerprint_json_value(&config)?));
            }
        }
    }

    let mut ancestor_configs = Vec::new();
    for ancestor in current_dir.ancestors() {
        if let Some(path) = cargo_config_path(&ancestor.join(".cargo"))? {
            ancestor_configs.push(path);
        }
    }
    ancestor_configs.reverse();
    let mut source_index = 0_usize;
    for path in ancestor_configs {
        if !seen.insert(stable_source_path(&path)) {
            continue;
        }
        if let Some(config) = relevant_cargo_config_tree(&path, args, &mut Vec::new())? {
            receipts.push((
                format!("cwd-config:{source_index}"),
                fingerprint_json_value(&config)?,
            ));
            source_index += 1;
        }
    }
    Ok(receipts)
}

fn cargo_config_path(directory: &Path) -> Result<Option<PathBuf>> {
    let legacy = directory.join("config");
    if legacy
        .try_exists()
        .with_context(|| format!("Failed to inspect Cargo config {}", legacy.display()))?
    {
        return Ok(Some(legacy));
    }
    let toml = directory.join("config.toml");
    Ok(toml
        .try_exists()
        .with_context(|| format!("Failed to inspect Cargo config {}", toml.display()))?
        .then_some(toml))
}

fn stable_source_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn relevant_manifest_profile(path: &Path) -> Result<Option<serde_json::Value>> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("Failed to read workspace manifest {}", path.display()))?;
    let manifest = source.parse::<toml::Table>().map_err(|_| {
        anyhow::anyhow!(
            "Workspace manifest {} contains invalid TOML while deriving the build input receipt",
            path.display()
        )
    })?;
    manifest
        .get("profile")
        .map(canonical_json_value)
        .transpose()
}

fn relevant_cargo_config_tree(
    path: &Path,
    args: &StressArgs,
    stack: &mut Vec<PathBuf>,
) -> Result<Option<serde_json::Value>> {
    let stable_path = stable_source_path(path);
    if stack.contains(&stable_path) {
        bail!(
            "Cargo config include cycle encountered while deriving the build input receipt at {}",
            path.display()
        );
    }
    stack.push(stable_path);
    let result = (|| {
        let source = fs::read_to_string(path)
            .with_context(|| format!("Failed to read Cargo config {}", path.display()))?;
        let config = source.parse::<toml::Table>().map_err(|_| {
            anyhow::anyhow!(
                "Cargo config {} contains invalid TOML while deriving the build input receipt",
                path.display()
            )
        })?;

        let mut includes = Vec::new();
        for (include_path, optional) in cargo_config_includes(&config, path)? {
            if !include_path.try_exists().with_context(|| {
                format!(
                    "Failed to inspect included Cargo config {}",
                    include_path.display()
                )
            })? {
                if optional {
                    continue;
                }
                bail!(
                    "Cargo config {} includes missing required config {}",
                    path.display(),
                    include_path.display()
                );
            }
            if let Some(include) = relevant_cargo_config_tree(&include_path, args, stack)? {
                includes.push(include);
            }
        }

        let own = relevant_cargo_config_subset(&config, args)?;
        if includes.is_empty() && own.is_none() {
            return Ok(None);
        }
        let mut tree = serde_json::Map::new();
        if !includes.is_empty() {
            tree.insert("includes".to_string(), serde_json::Value::Array(includes));
        }
        if let Some(own) = own {
            tree.insert("own".to_string(), own);
        }
        Ok(Some(serde_json::Value::Object(tree)))
    })();
    stack.pop();
    result
}

fn cargo_config_includes(config: &toml::Table, path: &Path) -> Result<Vec<(PathBuf, bool)>> {
    let Some(include) = config.get("include") else {
        return Ok(Vec::new());
    };
    let entries = include.as_array().with_context(|| {
        format!(
            "Cargo config {} has a non-array include directive",
            path.display()
        )
    })?;
    entries
        .iter()
        .map(|entry| {
            let (include_path, optional) = match entry {
                toml::Value::String(include_path) => (include_path.as_str(), false),
                toml::Value::Table(table) => {
                    let include_path = table
                        .get("path")
                        .and_then(toml::Value::as_str)
                        .with_context(|| {
                            format!(
                                "Cargo config {} has an include table without a string path",
                                path.display()
                            )
                        })?;
                    let optional = table
                        .get("optional")
                        .map(|optional| {
                            optional.as_bool().with_context(|| {
                                format!(
                                    "Cargo config {} has a non-boolean include optional flag",
                                    path.display()
                                )
                            })
                        })
                        .transpose()?
                        .unwrap_or(false);
                    (include_path, optional)
                }
                _ => bail!(
                    "Cargo config {} has an include entry that is not a path or table",
                    path.display()
                ),
            };
            let include_path = PathBuf::from(include_path);
            Ok((
                if include_path.is_absolute() {
                    include_path
                } else {
                    path.parent()
                        .unwrap_or_else(|| Path::new(""))
                        .join(include_path)
                },
                optional,
            ))
        })
        .collect()
}

fn relevant_cargo_config_subset(
    config: &toml::Table,
    args: &StressArgs,
) -> Result<Option<serde_json::Value>> {
    let mut selected = toml::Table::new();
    for key in ["build", "target", "profile", "unstable"] {
        if let Some(value) = config.get(key) {
            selected.insert(key.to_string(), value.clone());
        }
    }
    if let Some(environment) = config.get("env") {
        let environment = environment
            .as_table()
            .ok_or_else(|| anyhow::anyhow!("Cargo config env section is not a TOML table"))?;
        let material = environment
            .iter()
            .filter(|(key, _)| material_build_environment_key(key, args))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<toml::Table>();
        if !material.is_empty() {
            selected.insert("env".to_string(), toml::Value::Table(material));
        }
    }
    if selected.is_empty() {
        Ok(None)
    } else {
        canonical_json_value(&toml::Value::Table(selected)).map(Some)
    }
}

fn canonical_json_value(value: &toml::Value) -> Result<serde_json::Value> {
    let value = serde_json::to_value(value).context("Failed to canonicalize Cargo TOML input")?;
    Ok(sort_json_value(value))
}

fn sort_json_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => {
            let sorted = object
                .into_iter()
                .map(|(key, value)| (key, sort_json_value(value)))
                .collect::<BTreeMap<_, _>>();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(array) => {
            serde_json::Value::Array(array.into_iter().map(sort_json_value).collect())
        }
        value => value,
    }
}

fn fingerprint_json_value(value: &serde_json::Value) -> Result<String> {
    let canonical =
        serde_json::to_string(value).context("Failed to serialize Cargo input receipt")?;
    Ok(stable_environment_value_fingerprint(&canonical))
}

fn material_build_environment_key(key: &str, args: &StressArgs) -> bool {
    let exact = matches!(
        key,
        "RUSTFLAGS"
            | "CARGO_ENCODED_RUSTFLAGS"
            | "RUSTC"
            | "RUSTC_WRAPPER"
            | "RUSTC_WORKSPACE_WRAPPER"
            | "RUSTC_BOOTSTRAP"
            | "RUSTUP_TOOLCHAIN"
            | "CARGO_BUILD_RUSTFLAGS"
            | "CARGO_BUILD_RUSTC"
            | "CARGO_BUILD_RUSTC_WRAPPER"
            | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
            | "CARGO_INCREMENTAL"
    ) || (key == "CARGO_BUILD_TARGET" && args.target.is_none());
    let profile_override = if args.dev {
        key.starts_with("CARGO_PROFILE_DEV_")
    } else {
        key.starts_with("CARGO_PROFILE_BENCH_") || key.starts_with("CARGO_PROFILE_RELEASE_")
    };
    let target_specific = key.starts_with("CARGO_TARGET_")
        && ["_RUSTFLAGS", "_LINKER", "_RUNNER"]
            .iter()
            .any(|suffix| key.ends_with(suffix));
    exact || profile_override || target_specific
}

fn stable_environment_value_fingerprint(value: &str) -> String {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = value
        .as_bytes()
        .iter()
        .fold(FNV_OFFSET_BASIS, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
        });
    format!("fnv1a64:{hash:016x}")
}

/// Build arguments to pass through to the stress binary.
fn build_passthrough_args(cmd: &mut Command, args: &StressArgs, passthrough_json: bool) {
    // Workload filter
    if let Some(ref workload) = args.workload {
        cmd.arg("--workload").arg(workload);
    }

    if let Some(profile) = &args.profile {
        cmd.arg("--profile").arg(profile.to_string());
    }

    if let Some(tier) = args.tier {
        cmd.arg("--tier").arg(tier.get().to_string());
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

    if let Some(operations) = args.operations_per_sample {
        cmd.arg("--operations-per-sample")
            .arg(operations.get().to_string());
    }

    if let Some(duration) = args.sample_duration_ms {
        cmd.arg("--sample-duration-ms")
            .arg(duration.get().to_string());
    }

    if let Some(duration) = args.micro_sample_duration_ms {
        cmd.arg("--micro-sample-duration-ms")
            .arg(duration.get().to_string());
    }

    if let Some(timeout) = args.timeout_secs {
        cmd.arg("--timeout-secs").arg(timeout.get().to_string());
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
        cmd.arg("--names").arg(names.to_string());
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

    // The child harness accepts a fraction, while the canonical cargo-stress
    // surface accepts explicit percentage points.
    if let Some(threshold) = args
        .threshold_percent
        .map(ThresholdPercent::as_fraction)
        .or_else(|| args.threshold.map(LegacyThresholdFraction::as_fraction))
    {
        cmd.arg("--threshold").arg(threshold.to_string());
    }

    if args.fail_on_issues {
        cmd.arg("--fail-on-issues");
    }

    if let Some(ref deny_diagnostics) = args.deny_diagnostics {
        cmd.arg("--deny-diagnostics")
            .arg(deny_diagnostics.to_string());
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
        bail!("no stress binaries produced results");
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
    if should_emit_consolidated_output(json_stdout, verbosity) {
        println!("{output}");
    }
    report_child_failures(results);
    Ok(())
}

const fn should_emit_consolidated_output(json_stdout: bool, verbosity: Verbosity) -> bool {
    json_stdout || !verbosity.is_quiet()
}

fn consolidated_output(results: &[StressRunResult]) -> Result<String> {
    report_result_errors(results)?;
    report_empty_runs(results)?;
    let runs = results
        .iter()
        .filter_map(|result| result.run.clone())
        .collect::<Vec<_>>();
    Ok(format_console_runs(&runs))
}

fn consolidated_json_output(results: &[StressRunResult]) -> Result<String> {
    report_result_errors(results)?;
    report_empty_runs(results)?;
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

fn report_result_errors(results: &[StressRunResult]) -> Result<()> {
    let failed = results
        .iter()
        .filter_map(|result| {
            result
                .result_error
                .as_ref()
                .map(|error| (result, error.as_str()))
        })
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return Ok(());
    }

    for (result, error) in failed {
        eprintln!(
            "Rejected stress JSON result from {} after {:.2}s: {}",
            result.target.label(),
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
    bail!("one or more stress binaries did not emit a valid passing current-schema result")
}

fn report_empty_runs(results: &[StressRunResult]) -> Result<()> {
    let empty_suites = results
        .iter()
        .filter_map(|result| {
            result
                .run
                .as_ref()
                .filter(|run| run.summaries.is_empty())
                .map(|run| run.suite.as_str())
        })
        .collect::<Vec<_>>();
    if empty_suites.is_empty() {
        return Ok(());
    }
    bail!(
        "stress binaries emitted zero benchmark results: {}",
        empty_suites.join(", ")
    )
}

fn report_child_failures(results: &[StressRunResult]) {
    for result in results.iter().filter(|result| !result.status.success()) {
        let exit_info = result
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |code| format!("exit {code}"));
        eprintln!(
            "Stress binary {} failed after {:.2}s ({exit_info})",
            result.target.label(),
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
    use clap::CommandFactory;
    use cntryl_stress::artifact::{
        BenchmarkBudgets, BenchmarkMode, BenchmarkSpec, BenchmarkSummary, ComparisonClass,
        ConsoleNameMode, CorrectnessCounters, CorrectnessSummary, EnvironmentInfo,
        MeasurementIntent, PrimaryMetric, ProfileConfig, QualityClass, RunProfile, Sample,
        SamplePhase, SummaryStats, TrustClass, SCHEMA_VERSION,
    };
    use cntryl_stress::{runner::StressRunner, StressRunnerConfig};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "cargo-stress-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, relative: impl AsRef<Path>, contents: &str) {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create fixture parent directory");
            }
            fs::write(path, contents).expect("write fixture file");
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn cargo_native_fixture() -> TestDir {
        let fixture = TestDir::new("cargo-native");
        let stress_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .display()
            .to_string()
            .replace('\\', "/");
        fixture.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"alpha\", \"beta\", \"helper\"]\nresolver = \"2\"\n",
        );
        fixture.write(
            ".cargo/config.toml",
            "[build]\ntarget-dir = \"target\"\n\n[net]\noffline = true\n",
        );
        fixture.write(
            "helper/Cargo.toml",
            "[package]\nname = \"fixture-helper\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        );
        fixture.write("helper/src/lib.rs", "pub const fn answer() -> u32 { 42 }\n");
        fixture.write(
            "alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[[bench]]\nname = \"must-not-run\"\nharness = false\n",
        );
        fixture.write("alpha/src/lib.rs", "");
        fixture.write(
            "alpha/benches/must-not-run.rs",
            "// cntryl_stress::stress_main!()\nfn main() { std::process::exit(91); }\n",
        );
        fixture.write(
            "beta/Cargo.toml",
            &format!(
                "[package]\nname = \"beta\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[features]\nbench-fixture = []\n\n[dev-dependencies]\nstress-kit = {{ package = \"cntryl-stress\", path = \"{stress_path}\" }}\nrenamed-helper = {{ package = \"fixture-helper\", path = \"../helper\" }}\n\n[[bench]]\nname = \"shared-case\"\npath = \"benches/shared_case.rs\"\nharness = false\nrequired-features = [\"bench-fixture\"]\n"
            ),
        );
        fixture.write("beta/src/lib.rs", "");
        fixture.write(
            "beta/benches/shared_case.rs",
            "use stress_kit::{stress, StressContext};\nmod support;\n\n#[stress(tier = 2)]\nfn shared_case(ctx: &mut StressContext) {\n    let completed = u64::from(std::hint::black_box(support::answer()));\n    ctx.record_external(\"shared support\", std::time::Duration::from_millis(10), completed);\n}\n\nstress_kit::stress_main!();\n",
        );
        fixture.write(
            "beta/benches/support.rs",
            "pub fn answer() -> u32 { renamed_helper::answer() }\n",
        );
        fixture
    }

    fn run_direct_cargo_native_fixture(
        fixture: &TestDir,
        output_dir: &Path,
        build_identity: &str,
    ) -> std::process::Output {
        Command::new("cargo")
            .current_dir(fixture.path())
            .env_remove("STRESS_SUITE")
            .env_remove("STRESS_ARTIFACT_NAMESPACE")
            .env_remove("STRESS_BASELINE")
            .env_remove("STRESS_SAVE_BASELINE")
            .env("STRESS_BUILD_INPUT_IDENTITY", build_identity)
            .args([
                "bench",
                "--manifest-path",
                "beta/Cargo.toml",
                "--package",
                "beta",
                "--bench",
                "shared-case",
                "--features",
                "bench-fixture",
                "--",
                "--profile",
                "default",
                "--output-dir",
            ])
            .arg(output_dir)
            .output()
            .expect("run direct Cargo bench fixture")
    }

    fn multi_bench_selection_fixture() -> TestDir {
        let fixture = TestDir::new("multi-bench-selection");
        let stress_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .display()
            .to_string()
            .replace('\\', "/");
        fixture.write(
            "Cargo.toml",
            &format!(
                "[package]\nname = \"selection-fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dev-dependencies]\ncntryl-stress = {{ path = \"{stress_path}\" }}\n\n[[bench]]\nname = \"a-first\"\nharness = false\n\n[[bench]]\nname = \"b-later\"\nharness = false\n\n[[bench]]\nname = \"c-later\"\nharness = false\n"
            ),
        );
        fixture.write(
            ".cargo/config.toml",
            "[build]\ntarget-dir = \"target\"\n\n[net]\noffline = true\n",
        );
        fixture.write("src/lib.rs", "");
        fixture.write(
            "benches/a-first.rs",
            "use cntryl_stress::{stress, StressContext};\n#[stress(tier = 2)]\nfn first_only(ctx: &mut StressContext) { ctx.measure(\"first only\", || std::hint::black_box(1_u64)); }\ncntryl_stress::stress_main!();\n",
        );
        for target in ["b-later", "c-later"] {
            fixture.write(
                format!("benches/{target}.rs"),
                "use cntryl_stress::{stress, StressContext};\n#[stress(tier = 2)]\nfn selected_case(ctx: &mut StressContext) { ctx.measure(\"selected case\", || std::hint::black_box(2_u64)); }\ncntryl_stress::stress_main!();\n",
            );
        }
        fixture
    }

    fn stress_target_in(package: &str, name: &str) -> StressTarget {
        StressTarget {
            package_id: "fixture-package-id".to_string(),
            package_name: package.to_string(),
            package_version: "0.0.0".to_string(),
            name: name.to_string(),
            path: PathBuf::from(format!("benches/{name}.rs")),
            required_features: Vec::new(),
        }
    }

    fn stress_target(name: &str) -> StressTarget {
        stress_target_in("fixture-package", name)
    }

    fn built_target(name: &str, executable: PathBuf) -> BuiltStressTarget {
        BuiltStressTarget {
            target: stress_target(name),
            executable,
        }
    }

    fn result_for(run: StressRun) -> StressRunResult {
        StressRunResult {
            target: stress_target(&run.suite),
            status: success_status(),
            duration: Duration::from_millis(1),
            stdout: String::new(),
            stderr: String::new(),
            run: Some(run),
            result_error: None,
            gate_error: None,
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
            timeout_secs: None,
            operations_per_sample: None,
            sample_duration_ms: None,
            micro_sample_duration_ms: None,
            quiet: false,
            json: false,
            output_dir: None,
            names: None,
            baseline: None,
            baseline_dir: None,
            save_baseline: false,
            threshold_percent: None,
            threshold: None,
            fail_on_issues: false,
            deny_diagnostics: None,
            dev: false,
            cargo_args: Vec::new(),
            features: Vec::new(),
            all_features: false,
            no_default_features: false,
            target: None,
            target_dir: None,
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
            trust_class: TrustClass::Gate,
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

    fn canonical_child_receipt(run_id: &str, fail_correctness: bool) -> StressRun {
        let config = StressRunnerConfig::for_profile(RunProfile::Smoke)
            .samples(2)
            .warmup_samples(0)
            .cooldown_samples(0)
            .operations_per_sample(1)
            .progress(false);
        let metadata = BTreeMap::from([
            ("run_id".to_string(), run_id.to_string()),
            (
                "artifact_namespace".to_string(),
                "fixture-package".to_string(),
            ),
        ]);
        let mut runner = StressRunner::with_config_and_metadata("receipt-case", config, metadata);
        runner.reporters(Vec::new());
        runner.run("bench", |ctx| {
            // Receipt identity is independent of host scheduling. Keep this fixture's primary
            // evidence deterministic; floating-point round-trip tolerance has focused tests.
            ctx.record_external("work", Duration::from_millis(10), 1);
            if fail_correctness {
                let _ = ctx.correctness().attempted(1).completed(0).failures(1);
            }
        });
        runner.finish()
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
    fn explicit_json_output_is_not_suppressed_by_quiet_mode() {
        assert!(should_emit_consolidated_output(true, Verbosity::Quiet));
        assert!(!should_emit_consolidated_output(false, Verbosity::Quiet));
    }

    #[test]
    fn child_receipts_require_canonical_wrapper_identity_but_retain_valid_gate_failures() {
        let target = stress_target("receipt_case");
        let run_id = "shared-receipt-run";
        let run = canonical_child_receipt(run_id, false);
        let expected_build_identity = run.environment.build_profile.clone();
        let json = serde_json::to_string(&run).expect("serialize canonical receipt");

        let parsed = parse_and_validate_child_run(&json, &target, run_id, &expected_build_identity)
            .expect("canonical wrapper receipt");
        assert_eq!(parsed.suite, "receipt-case");
        assert_eq!(evaluate_run_gate(&parsed), RunGate::Passed);

        let failing = canonical_child_receipt(run_id, true);
        let failing_identity = failing.environment.build_profile.clone();
        let failing_json = serde_json::to_string(&failing).expect("serialize failing receipt");
        let parsed_failing =
            parse_and_validate_child_run(&failing_json, &target, run_id, &failing_identity)
                .expect("a canonical failed gate remains reportable evidence");
        assert_eq!(
            evaluate_run_gate(&parsed_failing),
            RunGate::CorrectnessFailed
        );

        let mut wrong_suite = run.clone();
        wrong_suite.suite = "other-suite".to_string();
        let error = parse_and_validate_child_run(
            &serde_json::to_string(&wrong_suite).expect("serialize wrong suite"),
            &target,
            run_id,
            &expected_build_identity,
        )
        .expect_err("suite identity is part of the wrapper receipt");
        assert!(error.contains("selected Cargo bench target suite"));

        let mut wrong_run_id = run.clone();
        wrong_run_id
            .metadata
            .insert("run_id".to_string(), "other-run".to_string());
        let error = parse_and_validate_child_run(
            &serde_json::to_string(&wrong_run_id).expect("serialize wrong run id"),
            &target,
            run_id,
            &expected_build_identity,
        )
        .expect_err("run generation identity is part of the wrapper receipt");
        assert!(error.contains("metadata.run_id"));

        let mut wrong_namespace = run.clone();
        wrong_namespace.metadata.insert(
            "artifact_namespace".to_string(),
            "other-package".to_string(),
        );
        let error = parse_and_validate_child_run(
            &serde_json::to_string(&wrong_namespace).expect("serialize wrong namespace"),
            &target,
            run_id,
            &expected_build_identity,
        )
        .expect_err("package namespace is part of the wrapper receipt");
        assert!(error.contains("artifact namespace"));

        let mut missing_namespace = run.clone();
        missing_namespace.metadata.remove("artifact_namespace");
        let error = parse_and_validate_child_run(
            &serde_json::to_string(&missing_namespace).expect("serialize missing namespace"),
            &target,
            run_id,
            &expected_build_identity,
        )
        .expect_err("package namespace must be present in a wrapper receipt");
        assert!(error.contains("metadata.artifact_namespace"));

        let mut wrong_build = run.clone();
        wrong_build.environment.build_profile = "spoofed-build".to_string();
        for sample in &mut wrong_build.samples {
            sample.environment.build_profile = "spoofed-build".to_string();
        }
        let error = parse_and_validate_child_run(
            &serde_json::to_string(&wrong_build).expect("serialize wrong build identity"),
            &target,
            run_id,
            &expected_build_identity,
        )
        .expect_err("build input identity is part of the wrapper receipt");
        assert!(error.contains("build identity"));

        let mut tampered = run;
        tampered.summaries[0]
            .stats
            .as_mut()
            .expect("measured summary statistics")
            .mean += 1.0;
        let error = parse_and_validate_child_run(
            &serde_json::to_string(&tampered).expect("serialize tampered summary"),
            &target,
            run_id,
            &expected_build_identity,
        )
        .expect_err("serialized summaries cannot disagree with raw evidence");
        assert!(error.contains("non-canonical stress evidence"));
    }

    #[test]
    fn wrapper_build_identity_is_canonical_and_only_tracks_material_build_inputs() {
        assert_eq!(
            wrapper_build_input_identity_with_env(&stress_args(), std::iter::empty()),
            "release"
        );
        assert_eq!(
            wrapper_build_input_identity_with_env(
                &StressArgs {
                    dev: true,
                    ..stress_args()
                },
                std::iter::empty(),
            ),
            "debug"
        );

        let first = wrapper_build_input_identity_with_env(
            &StressArgs {
                features: vec!["net".to_string(), "io".to_string(), "net".to_string()],
                cargo_args: vec!["--locked".to_string()],
                ..stress_args()
            },
            std::iter::empty(),
        );
        let second = wrapper_build_input_identity_with_env(
            &StressArgs {
                features: vec!["io".to_string(), "net".to_string()],
                cargo_args: vec!["--offline".to_string()],
                ..stress_args()
            },
            std::iter::empty(),
        );
        assert_eq!(first, second);
        assert!(first.contains("io"));
        assert!(first.contains("net"));

        let targeted = wrapper_build_input_identity_with_env(
            &StressArgs {
                target: Some("x86_64-unknown-linux-gnu".to_string()),
                ..stress_args()
            },
            std::iter::empty(),
        );
        assert_ne!(targeted, "release");
        assert!(targeted.contains("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn wrapper_build_identity_fingerprints_relevant_ambient_inputs_canonically() {
        let args = stress_args();
        let first = wrapper_build_input_identity_with_env(
            &args,
            [
                ("RUSTFLAGS", "-C target-cpu=native"),
                ("CARGO_PROFILE_BENCH_LTO", "fat"),
                ("RUSTC_WRAPPER", "/opt/private/bin/sccache"),
                (
                    "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
                    "/usr/bin/clang",
                ),
            ],
        );
        let reordered = wrapper_build_input_identity_with_env(
            &args,
            [
                (
                    "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
                    "/usr/bin/clang",
                ),
                ("RUSTC_WRAPPER", "/opt/private/bin/sccache"),
                ("CARGO_PROFILE_BENCH_LTO", "fat"),
                ("RUSTFLAGS", "-C target-cpu=native"),
            ],
        );
        let changed = wrapper_build_input_identity_with_env(
            &args,
            [("RUSTFLAGS", "-C target-cpu=x86-64-v3")],
        );

        assert_eq!(first, reordered);
        assert_ne!(first, changed);
        assert!(first.contains("RUSTFLAGS"));
        assert!(first.contains("CARGO_PROFILE_BENCH_LTO"));
        assert!(first.contains("RUSTC_WRAPPER"));
        assert!(first.contains("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER"));
        assert!(!first.contains("target-cpu=native"));
        assert!(!first.contains("/opt/private/bin/sccache"));
        assert!(!first.contains("/usr/bin/clang"));
    }

    #[test]
    fn wrapper_build_identity_handles_encoded_flags_and_ignores_secrets_and_irrelevant_env() {
        let args = stress_args();
        let encoded = wrapper_build_input_identity_with_env(
            &args,
            [(
                "CARGO_ENCODED_RUSTFLAGS",
                "-Copt-level=3\u{1f}-Ctarget-cpu=native",
            )],
        );
        let first = wrapper_build_input_identity_with_env(
            &args,
            [
                ("CARGO_REGISTRIES_CRATES_IO_TOKEN", "secret-one"),
                ("PATH", "/first/path"),
                ("CARGO_PROFILE_DEV_OPT_LEVEL", "0"),
            ],
        );
        let second = wrapper_build_input_identity_with_env(
            &args,
            [
                ("CARGO_REGISTRIES_CRATES_IO_TOKEN", "secret-two"),
                ("PATH", "/second/path"),
                ("CARGO_PROFILE_DEV_OPT_LEVEL", "3"),
            ],
        );

        assert_ne!(encoded, "release");
        assert!(encoded.contains("CARGO_ENCODED_RUSTFLAGS"));
        assert!(!encoded.contains("target-cpu=native"));
        assert!(!encoded.contains('\u{1f}'));
        assert_eq!(first, "release");
        assert_eq!(first, second);
        assert_ne!(
            wrapper_build_input_identity_with_env(
                &args,
                [("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1")]
            ),
            "release",
            "Cargo's bench profile inherits release profile overrides"
        );

        let debug = StressArgs {
            dev: true,
            ..stress_args()
        };
        assert_ne!(
            wrapper_build_input_identity_with_env(&debug, [("CARGO_PROFILE_DEV_OPT_LEVEL", "3")]),
            "debug"
        );
    }

    fn write_build_identity_fixture(
        fixture: &TestDir,
        profile_lto: &str,
        rustflags: &str,
        linker: &str,
        runner: &str,
        secret: &str,
        reverse_order: bool,
    ) {
        fixture.write(
            "Cargo.toml",
            &format!(
                "[workspace]\nmembers = []\n\n[profile.bench]\nlto = {profile_lto:?}\ncodegen-units = 1\n"
            ),
        );
        let config = if reverse_order {
            format!(
                "[env]\nPRIVATE_TOKEN = {secret:?}\n\n[registries.private]\ntoken = {secret:?}\n\n[target.aarch64-unknown-linux-gnu]\nrunner = {runner:?}\nlinker = {linker:?}\n\n[build]\nrustflags = [{rustflags:?}]\n"
            )
        } else {
            format!(
                "[build]\nrustflags = [{rustflags:?}]\n\n[target.aarch64-unknown-linux-gnu]\nlinker = {linker:?}\nrunner = {runner:?}\n\n[registries.private]\ntoken = {secret:?}\n\n[env]\nPRIVATE_TOKEN = {secret:?}\n"
            )
        };
        fixture.write(".cargo/config.toml", &config);
        fixture.write("member/work/.keep", "");
    }

    fn fixture_build_identity(fixture: &TestDir) -> String {
        wrapper_build_input_identity_with_context(
            &stress_args(),
            fixture.path(),
            &fixture.path().join("member/work"),
            None,
            std::iter::empty::<(String, String)>(),
        )
        .expect("build config receipt")
    }

    #[test]
    fn wrapper_build_identity_tracks_material_manifest_and_cargo_config_inputs() {
        let fixture = TestDir::new("build-config-material");
        write_build_identity_fixture(
            &fixture,
            "thin",
            "-Ctarget-cpu=x86-64",
            "clang",
            "qemu-aarch64",
            "secret-one",
            false,
        );
        let base = fixture_build_identity(&fixture);

        for (profile_lto, rustflags, linker, runner) in [
            ("fat", "-Ctarget-cpu=x86-64", "clang", "qemu-aarch64"),
            ("thin", "-Ctarget-cpu=native", "clang", "qemu-aarch64"),
            ("thin", "-Ctarget-cpu=x86-64", "mold", "qemu-aarch64"),
            ("thin", "-Ctarget-cpu=x86-64", "clang", "wasmtime"),
        ] {
            write_build_identity_fixture(
                &fixture,
                profile_lto,
                rustflags,
                linker,
                runner,
                "secret-one",
                false,
            );
            assert_ne!(fixture_build_identity(&fixture), base);
        }
    }

    #[test]
    fn wrapper_build_identity_ignores_secret_config_and_is_path_and_order_stable() {
        let first = TestDir::new("build-config-stable-first");
        write_build_identity_fixture(
            &first,
            "thin",
            "-Ctarget-cpu=x86-64",
            "clang",
            "qemu-aarch64",
            "secret-one",
            false,
        );
        let second = TestDir::new("build-config-stable-second");
        write_build_identity_fixture(
            &second,
            "thin",
            "-Ctarget-cpu=x86-64",
            "clang",
            "qemu-aarch64",
            "different-secret",
            true,
        );

        let first_identity = fixture_build_identity(&first);
        let second_identity = fixture_build_identity(&second);
        assert_eq!(first_identity, second_identity);
        assert!(!first_identity.contains(first.path().to_string_lossy().as_ref()));
        assert!(!first_identity.contains("secret-one"));
        assert!(!second_identity.contains("different-secret"));
    }

    #[test]
    fn wrapper_build_identity_tracks_cargo_home_and_included_material_config() {
        let fixture = TestDir::new("build-config-includes");
        fixture.write("Cargo.toml", "[workspace]\nmembers = []\n");
        fixture.write("member/work/.keep", "");
        fixture.write(
            "cargo-home/config.toml",
            "include = [\"material.toml\"]\n[net]\noffline = true\n",
        );
        fixture.write(
            "cargo-home/material.toml",
            "[target.x86_64-unknown-linux-gnu]\nlinker = \"clang\"\n",
        );
        let identity = || {
            wrapper_build_input_identity_with_context(
                &stress_args(),
                fixture.path(),
                &fixture.path().join("member/work"),
                Some(&fixture.path().join("cargo-home")),
                std::iter::empty::<(String, String)>(),
            )
            .expect("Cargo home receipt")
        };
        let first = identity();
        fixture.write(
            "cargo-home/material.toml",
            "[target.x86_64-unknown-linux-gnu]\nlinker = \"mold\"\n",
        );
        assert_ne!(identity(), first);
    }

    #[test]
    fn cargo_stress_passes_new_flags_to_child_binaries() {
        let stress_cli_args = StressArgs {
            names: Some(ConsoleNameMode::Full),
            baseline: Some(PathBuf::from("latest")),
            baseline_dir: Some(PathBuf::from("target/custom-baselines")),
            save_baseline: true,
            fail_on_issues: true,
            deny_diagnostics: Some(DiagnosticSeverity::Error),
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

    #[test]
    fn zero_discovered_stress_files_is_an_error() {
        let project = TestDir::new("zero-discovered");
        let manifest = project.path().join("Cargo.toml");
        fs::write(
            &manifest,
            "[package]\nname = \"empty-project\"\nversion = \"0.0.0\"\n",
        )
        .expect("write manifest");
        project.write("src/lib.rs", "");
        let args = StressArgs {
            manifest_path: Some(manifest),
            quiet: true,
            ..stress_args()
        };

        let error = run_stress(&args).expect_err("an empty suite must not pass");

        assert!(error.to_string().contains("No stress test files"));
    }

    #[test]
    fn stress_entrypoint_detection_supports_qualified_and_imported_macro_forms() {
        let roots = ["cntryl_stress", "stress_kit"]
            .into_iter()
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        for source in [
            "cntryl_stress :: stress_main ! ();",
            "stress_kit::stress_main! {}",
            "use cntryl_stress::stress_main; stress_main!();",
            "use cntryl_stress::{stress_main as suite_main, StressContext}; suite_main!();",
            "use cntryl_stress::*; stress_main!();",
            "use cntryl_stress as kit; kit::stress_main!();",
            "extern crate cntryl_stress as kit; kit::stress_main!();",
            "use stress_kit::stress_main as renamed_main; renamed_main!();",
        ] {
            assert!(
                has_supported_stress_entrypoint(source, &roots)
                    .expect("supported source should parse"),
                "expected a supported entrypoint in {source:?}"
            );
        }
    }

    #[test]
    fn stress_entrypoint_detection_rejects_lexical_and_unattributed_false_positives() {
        let roots = ["cntryl_stress"]
            .into_iter()
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        for source in [
            "// cntryl_stress::stress_main!()\nfn main() {}",
            "const NOTE: &str = \"cntryl_stress::stress_main!()\"; fn main() {}",
            "other_crate::stress_main!();",
            "use other_crate::stress_main; stress_main!();",
            "stress_main!();",
            "mod nested { cntryl_stress::stress_main!(); } fn main() {}",
        ] {
            assert!(
                !has_supported_stress_entrypoint(source, &roots)
                    .expect("unsupported source should still parse"),
                "unexpectedly accepted {source:?}"
            );
        }

        let conditional = has_supported_stress_entrypoint(
            "#[cfg(feature = \"bench\")] cntryl_stress::stress_main!(); fn main() {}",
            &roots,
        )
        .expect_err("conditional entrypoints must fail closed");
        assert!(conditional.contains("unconditional top-level"));
    }

    #[test]
    fn discovery_ignores_comment_markers_and_explains_explicit_unsupported_benches() {
        let fixture = cargo_native_fixture();
        let manifest_path = fixture
            .path()
            .join("Cargo.toml")
            .canonicalize()
            .expect("canonical fixture manifest");
        let args = StressArgs {
            manifest_path: Some(manifest_path.clone()),
            ..stress_args()
        };
        let metadata = load_cargo_metadata(&manifest_path, &args).expect("fixture metadata");
        let discovered = discover_stress_targets(&metadata, &manifest_path, &args)
            .expect("renamed dependency entrypoint should be discovered");
        assert_eq!(
            discovered
                .iter()
                .map(StressTarget::label)
                .collect::<Vec<_>>(),
            ["beta::shared-case"]
        );

        let unsupported = StressArgs {
            package: Some("alpha".to_string()),
            bin: Some("must-not-run".to_string()),
            manifest_path: Some(manifest_path.clone()),
            ..stress_args()
        };
        let error = discover_stress_targets(&metadata, &manifest_path, &unsupported)
            .expect_err("a comment must never opt an unrelated bench into execution");
        let message = error.to_string();
        assert!(message.contains("must-not-run.rs"));
        assert!(message.contains("supported"));
        assert!(message.contains("top-level"));
    }

    #[test]
    fn canonical_suite_collisions_are_rejected_during_discovery() {
        let fixture = TestDir::new("suite-collision");
        let manifest_path = fixture.path().join("Cargo.toml");
        let hyphen = fixture.path().join("same-name.rs");
        let underscore = fixture.path().join("same_name.rs");
        fixture.write("same-name.rs", "cntryl_stress::stress_main!();\n");
        fixture.write("same_name.rs", "cntryl_stress::stress_main!();\n");
        let package_id = "collision 0.0.0".to_string();
        let metadata = CargoMetadata {
            packages: vec![CargoPackage {
                name: "collision".to_string(),
                version: "0.0.0".to_string(),
                id: package_id.clone(),
                manifest_path: manifest_path.clone(),
                dependencies: vec![CargoDependency {
                    name: "cntryl-stress".to_string(),
                    rename: None,
                    kind: Some("dev".to_string()),
                }],
                targets: vec![
                    CargoTarget {
                        name: "same-name".to_string(),
                        kind: vec!["bench".to_string()],
                        src_path: hyphen,
                        required_features: Vec::new(),
                    },
                    CargoTarget {
                        name: "same_name".to_string(),
                        kind: vec!["bench".to_string()],
                        src_path: underscore,
                        required_features: Vec::new(),
                    },
                ],
            }],
            workspace_members: vec![package_id],
            workspace_default_members: Vec::new(),
            workspace_root: fixture.path().to_path_buf(),
        };

        let error = discover_stress_targets(&metadata, &manifest_path, &stress_args())
            .expect_err("canonical suite names must be unique within a package namespace");

        assert!(error.to_string().contains("same-name"));
        assert!(error.to_string().contains("same_name"));
        assert!(error.to_string().contains("artifact and baseline paths"));
    }

    #[test]
    fn missing_selected_binary_is_an_error() {
        let target = TestDir::new("missing-binary");

        let error = run_stress_binaries(
            &[built_target("missing", target.path().join("missing"))],
            &stress_args(),
            false,
            target.path(),
            "run-id",
            "release",
        )
        .expect_err("a missing selected binary must not be skipped");

        assert!(error.to_string().contains("Binary not found"));
        assert!(error.to_string().contains("missing"));
    }

    #[test]
    fn empty_child_result_set_is_an_error() {
        let error = report_results(&[], false, Verbosity::Quiet, true)
            .expect_err("an empty child result set must not pass");

        assert!(error
            .to_string()
            .contains("no stress binaries produced results"));
    }

    #[test]
    fn parsed_child_run_without_benchmark_results_is_an_error() {
        let results = vec![result_for(run("empty-suite", Vec::new()))];

        let error = consolidated_output(&results)
            .expect_err("a selected suite that executed no benchmarks must not pass");

        assert!(error.to_string().contains("zero benchmark results"));
        assert!(error.to_string().contains("empty-suite"));
    }

    #[test]
    fn fail_fast_reports_selected_binaries_that_were_not_executed() {
        let mut first = result_for(run(
            "first",
            vec![summary("first", QualityClass::Acceptable)],
        ));
        first.target = stress_target("first");
        first.result_error = Some("child failed".to_string());
        let results = vec![first];
        let error = ensure_selected_binaries_executed(
            &[stress_target("first"), stress_target("second")],
            &results,
        )
        .expect_err("fail-fast must identify selected binaries it did not execute");

        assert!(error.to_string().contains("not executed"));
        assert!(error.to_string().contains("second"));
    }

    #[test]
    fn no_build_is_rejected_with_an_actionable_error() {
        let args = StressArgs {
            no_build: true,
            quiet: true,
            ..stress_args()
        };

        let error = run_stress(&args).expect_err("--no-build cannot skip artifact discovery");

        assert!(error.to_string().contains("--no-build"));
        assert!(error.to_string().contains("not supported"));
    }

    #[test]
    fn explicit_threshold_percent_is_converted_to_child_fraction() {
        let cli = Cli::try_parse_from(["cargo", "stress", "--threshold-percent", "5"])
            .expect("explicit percent threshold should parse");
        let Commands::Stress(args) = cli.cmd;
        let mut cmd = Command::new("stress-child");

        build_passthrough_args(&mut cmd, &args, true);

        let child_args = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(child_args
            .windows(2)
            .any(|window| window[0] == "--threshold" && window[1] == "0.05"));
    }

    #[test]
    fn ambiguous_legacy_threshold_percentage_is_rejected() {
        let error = Cli::try_parse_from(["cargo", "stress", "--threshold", "5"])
            .expect_err("legacy --threshold accepts a fraction, not percentage points");

        assert!(error.to_string().contains("--threshold-percent"));
    }

    #[test]
    fn legacy_fraction_threshold_remains_compatible() {
        let cli = Cli::try_parse_from(["cargo", "stress", "--threshold", "0.05"])
            .expect("legacy fraction should remain compatible");
        let Commands::Stress(args) = cli.cmd;
        let mut cmd = Command::new("stress-child");

        build_passthrough_args(&mut cmd, &args, true);

        let child_args = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(child_args
            .windows(2)
            .any(|window| window[0] == "--threshold" && window[1] == "0.05"));
    }

    #[test]
    fn workspace_package_selection_runs_only_the_requested_native_bench() {
        let fixture = cargo_native_fixture();
        let args = StressArgs {
            package: Some("beta".to_string()),
            manifest_path: Some(fixture.path().join("Cargo.toml")),
            features: vec!["bench-fixture".to_string()],
            list: true,
            quiet: true,
            ..stress_args()
        };

        run_stress(&args).expect("selected workspace package bench should run in place");
    }

    #[test]
    fn native_bench_build_preserves_shared_modules_and_renamed_dev_dependencies() {
        let fixture = cargo_native_fixture();
        let args = StressArgs {
            manifest_path: Some(fixture.path().join("beta/Cargo.toml")),
            features: vec!["bench-fixture".to_string()],
            list: true,
            quiet: true,
            ..stress_args()
        };

        run_stress(&args).expect("bench should compile beside its shared support module");
    }

    #[test]
    fn workload_selection_skips_local_misses_and_runs_every_matching_target() {
        let fixture = multi_bench_selection_fixture();
        let output_dir = fixture.path().join("artifacts");
        let miss = StressArgs {
            manifest_path: Some(fixture.path().join("Cargo.toml")),
            workload: Some("missing_case".to_string()),
            profile: Some(RunProfile::Smoke),
            output_dir: Some(output_dir.clone()),
            quiet: true,
            ..stress_args()
        };

        let error = run_stress(&miss).expect_err("a global selection miss must fail once");
        let message = error.to_string();
        assert!(message.contains("No registered benchmark matched"));
        assert!(message.contains("first_only"));
        assert!(message.contains("selected_case"));

        let matching = StressArgs {
            workload: Some("selected_case".to_string()),
            ..miss
        };
        run_stress(&matching).expect("later matching targets should both execute");

        let package_dir = output_dir.join("selection-fixture");
        assert!(!package_dir.join("a-first/latest.json").exists());
        assert!(package_dir.join("b-later/latest.json").exists());
        assert!(package_dir.join("c-later/latest.json").exists());
    }

    #[test]
    fn timeout_secs_is_positive_and_forwarded_as_a_child_cli_override() {
        let cli = Cli::try_parse_from(["cargo", "stress", "--timeout-secs", "30"])
            .expect("positive timeout should parse");
        let Commands::Stress(args) = cli.cmd;
        let mut cmd = Command::new("stress-child");

        apply_stress_env(&mut cmd, &args, &stress_target("timeout-case"), "release");
        build_passthrough_args(&mut cmd, &args, false);

        assert!(cmd.get_envs().any(|(key, value)| {
            key == "STRESS_TIMEOUT_SECS" && value == Some(std::ffi::OsStr::new("30"))
        }));
        let child_args = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(child_args
            .windows(2)
            .any(|window| window[0] == "--timeout-secs" && window[1] == "30"));
    }

    #[test]
    fn stabilization_controls_are_positive_and_forwarded_to_children() {
        let cli = Cli::try_parse_from([
            "cargo",
            "stress",
            "--operations-per-sample",
            "32",
            "--sample-duration-ms",
            "250",
            "--micro-sample-duration-ms",
            "15",
        ])
        .expect("positive stabilization controls should parse");
        let Commands::Stress(args) = cli.cmd;
        let mut cmd = Command::new("stress-child");

        build_passthrough_args(&mut cmd, &args, false);

        let child_args = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        for (flag, value) in [
            ("--operations-per-sample", "32"),
            ("--sample-duration-ms", "250"),
            ("--micro-sample-duration-ms", "15"),
        ] {
            assert!(child_args
                .windows(2)
                .any(|window| window[0] == flag && window[1] == value));
        }
    }

    #[test]
    fn zero_stabilization_controls_are_rejected() {
        for flag in [
            "--operations-per-sample",
            "--sample-duration-ms",
            "--micro-sample-duration-ms",
        ] {
            let error = Cli::try_parse_from(["cargo", "stress", flag, "0"])
                .expect_err("stabilization controls must be positive");

            assert!(error.to_string().contains("positive"));
        }
    }

    #[test]
    fn help_discovers_stabilization_and_cargo_target_controls() {
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("stress")
            .expect("stress subcommand")
            .render_long_help()
            .to_string();

        for flag in [
            "--operations-per-sample",
            "--sample-duration-ms",
            "--micro-sample-duration-ms",
            "--target",
            "--target-dir",
        ] {
            assert!(help.contains(flag), "missing {flag} from help:\n{help}");
        }
    }

    #[test]
    fn wrapper_no_progress_option_is_rejected_and_not_advertised() {
        Cli::try_parse_from(["cargo", "stress", "--no-progress"])
            .expect_err("captured child output cannot provide live progress");

        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("stress")
            .expect("stress subcommand")
            .render_long_help()
            .to_string();
        assert!(!help.contains("--no-progress"));
        assert!(help.contains("live child progress is not currently streamed"));
    }

    #[test]
    fn zero_timeout_secs_is_rejected() {
        let error = Cli::try_parse_from(["cargo", "stress", "--timeout-secs", "0"])
            .expect_err("timeout must be positive");

        assert!(error.to_string().contains("positive"));
    }

    #[test]
    fn canonical_bench_selector_retains_bin_alias() {
        for flag in ["--bench", "--bin"] {
            let cli = Cli::try_parse_from(["cargo", "stress", flag, "shared-case"])
                .expect("bench selector should parse");
            let Commands::Stress(args) = cli.cmd;
            assert_eq!(args.bin.as_deref(), Some("shared-case"));
        }
    }

    #[test]
    fn cargo_message_format_override_is_rejected() {
        let args = StressArgs {
            cargo_args: vec!["--message-format=short".to_string()],
            ..stress_args()
        };

        let error = validate_stress_args(&args)
            .expect_err("artifact discovery owns Cargo's message format");

        assert!(error.to_string().contains("JSON artifact stream"));
    }

    #[test]
    fn child_suite_identity_is_canonical_and_artifact_namespace_is_package_qualified() {
        let args = stress_args();
        let target = stress_target_in("package:name", "shared_case");
        let mut cmd = Command::new("stress-child");

        apply_stress_env(&mut cmd, &args, &target, "release");

        assert!(cmd.get_envs().any(|(key, value)| {
            key == "STRESS_SUITE" && value == Some(std::ffi::OsStr::new("shared-case"))
        }));
        assert!(cmd.get_envs().any(|(key, value)| {
            key == "STRESS_ARTIFACT_NAMESPACE"
                && value == Some(std::ffi::OsStr::new("package-name"))
        }));
        assert_eq!(
            stress_target_in("alpha", "shared-case").suite_name(),
            stress_target_in("beta", "shared-case").suite_name()
        );
        assert_ne!(
            stress_target_in("alpha", "shared-case").artifact_namespace(),
            stress_target_in("beta", "shared-case").artifact_namespace()
        );
    }

    #[test]
    fn direct_artifact_is_a_compatible_wrapper_baseline() {
        let fixture = cargo_native_fixture();
        let direct_output_dir = fixture.path().join("direct-artifacts");
        let wrapper_output_dir = fixture.path().join("wrapper-artifacts");
        let feature_build_identity = wrapper_build_input_identity(
            &StressArgs {
                features: vec!["bench-fixture".to_string()],
                ..stress_args()
            },
            fixture.path(),
        )
        .expect("fixture build identity");
        let direct =
            run_direct_cargo_native_fixture(&fixture, &direct_output_dir, &feature_build_identity);
        let direct_artifact = direct_output_dir.join("shared-case/latest.json");
        assert!(
            direct_artifact.exists(),
            "direct Cargo bench did not write an artifact:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&direct.stdout),
            String::from_utf8_lossy(&direct.stderr)
        );
        let direct_run = StressRun::load(&direct_artifact).expect("load direct artifact");

        let args = StressArgs {
            manifest_path: Some(fixture.path().join("beta/Cargo.toml")),
            features: vec!["bench-fixture".to_string()],
            profile: Some(RunProfile::Default),
            baseline: Some(direct_artifact),
            output_dir: Some(wrapper_output_dir.clone()),
            quiet: true,
            ..stress_args()
        };
        let manifest_path = args
            .manifest_path
            .as_ref()
            .expect("fixture manifest")
            .canonicalize()
            .expect("canonical fixture manifest");
        let metadata = load_cargo_metadata(&manifest_path, &args).expect("load fixture metadata");
        let build_identity = wrapper_build_input_identity(&args, &metadata.workspace_root)
            .expect("wrapper build identity");
        let targets = discover_stress_targets(&metadata, &manifest_path, &args)
            .expect("discover fixture target");
        let built = build_stress_binaries(&targets, &args, &manifest_path)
            .expect("build fixture target through wrapper");
        let run_id = shared_run_id();
        verify_stress_harnesses(&built, &args, &manifest_path, &run_id, &build_identity)
            .expect("verify fixture harness");
        let results = run_stress_binaries(
            &built,
            &args,
            true,
            &manifest_path,
            &run_id,
            &build_identity,
        )
        .expect("execute fixture target through wrapper");
        assert_eq!(results.len(), 1);
        assert!(results[0].result_error.is_none());
        let comparisons = results[0].run.as_ref().map(|run| &run.comparisons);
        assert!(
            results[0].gate_error.is_none(),
            "unexpected wrapper gate failure: {:?}; stderr: {}; comparisons: {:?}",
            results[0].gate_error,
            results[0].stderr,
            comparisons,
        );
        let wrapper_run = results[0]
            .run
            .as_ref()
            .expect("wrapper child should emit a complete JSON run");

        let wrapper_artifact = wrapper_output_dir.join("beta/shared-case/latest.json");
        assert!(wrapper_artifact.exists());
        assert_eq!(direct_run.suite, "shared-case");
        assert_eq!(wrapper_run.suite, direct_run.suite);
        assert_eq!(
            wrapper_run
                .benchmark_specs
                .iter()
                .map(|spec| &spec.id)
                .collect::<Vec<_>>(),
            direct_run
                .benchmark_specs
                .iter()
                .map(|spec| &spec.id)
                .collect::<Vec<_>>()
        );
        assert!(wrapper_run
            .comparisons
            .iter()
            .all(|comparison| comparison.classification != ComparisonClass::MissingBaseline));
    }

    #[test]
    fn explicit_baseline_file_requires_one_real_metadata_target_but_latest_allows_many() {
        let fixture = multi_bench_selection_fixture();
        let manifest_path = fixture
            .path()
            .join("Cargo.toml")
            .canonicalize()
            .expect("canonical fixture manifest");
        let explicit_args = StressArgs {
            manifest_path: Some(manifest_path.clone()),
            baseline: Some(PathBuf::from("target/baseline.json")),
            ..stress_args()
        };
        let metadata =
            load_cargo_metadata(&manifest_path, &explicit_args).expect("load fixture metadata");
        let targets = discover_stress_targets(&metadata, &manifest_path, &explicit_args)
            .expect("discover all fixture targets");
        assert_eq!(targets.len(), 3);

        let error = validate_baseline_target_selection(&explicit_args, &targets)
            .expect_err("one explicit artifact cannot be forwarded to multiple targets");
        let message = error.to_string();
        assert!(message.contains("requires exactly one selected Cargo stress bench target"));
        assert!(message.contains("--package <PACKAGE> and --bench <BENCH>"));
        assert!(message.contains("--baseline latest"));

        let latest_args = StressArgs {
            baseline: Some(PathBuf::from("latest")),
            ..stress_args()
        };
        validate_baseline_target_selection(&latest_args, &targets)
            .expect("latest resolves separately for every selected target");

        let single_args = StressArgs {
            bin: Some("b-later".to_string()),
            baseline: Some(PathBuf::from("target/baseline.json")),
            ..stress_args()
        };
        let single_target = discover_stress_targets(&metadata, &manifest_path, &single_args)
            .expect("select one package target");
        assert_eq!(single_target.len(), 1);
        validate_baseline_target_selection(&single_args, &single_target)
            .expect("an explicit artifact is valid for one selected target");
    }

    #[test]
    fn invalid_typed_child_options_fail_before_cargo_discovery() {
        for (flag, value) in [
            ("--profile", "banana"),
            ("--tier", "0"),
            ("--tier", "7"),
            ("--names", "abbreviated"),
            ("--deny-diagnostics", "severe"),
        ] {
            Cli::try_parse_from(["cargo", "stress", flag, value])
                .expect_err("invalid child options must fail in the wrapper parser");
        }
    }

    #[test]
    fn resolution_safe_cargo_arg_values_are_forwarded_without_retokenizing() {
        let cli = Cli::try_parse_from([
            "cargo",
            "stress",
            "--cargo-arg",
            "--locked",
            "--cargo-arg",
            "--jobs",
            "--cargo-arg",
            "2",
        ])
        .expect("repeatable Cargo arguments should parse");
        let Commands::Stress(args) = cli.cmd;
        validate_stress_args(&args).expect("safe Cargo arguments should be accepted");
        let mut build = Command::new("cargo");
        let mut metadata = Command::new("cargo");

        apply_cargo_args(&mut build, &args);
        apply_cargo_resolution_args(&mut metadata, &args);

        let build_args = build
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(build_args, ["--locked", "--jobs", "2"]);
        let metadata_args = metadata
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(metadata_args, ["--locked"]);
    }

    #[test]
    fn cargo_arg_allowlist_rejects_scope_profile_short_circuit_and_escape_flags() {
        for cargo_args in [
            vec!["--profile", "dev"],
            vec!["--no-run"],
            vec!["--bench", "other"],
            vec!["--package", "other"],
            vec!["--config", "build.rustflags=[]"],
            vec!["-Zunstable-options"],
            vec!["--keep-going"],
            vec!["positional-escape"],
        ] {
            let args = StressArgs {
                cargo_args: cargo_args.into_iter().map(str::to_string).collect(),
                ..stress_args()
            };

            validate_stress_args(&args)
                .expect_err("non-resolution-safe Cargo arguments must be rejected");
        }
    }

    #[test]
    fn removed_cargo_args_string_is_rejected() {
        let error = Cli::try_parse_from(["cargo", "stress", "--cargo-args", "--locked"])
            .expect_err("the ambiguous whitespace-split option was removed before release");

        assert!(error.to_string().contains("--cargo-arg"));
    }

    #[test]
    fn first_class_cargo_build_flags_are_forwarded() {
        let cli = Cli::try_parse_from([
            "cargo",
            "stress",
            "--features",
            "io,net",
            "--no-default-features",
            "--target-dir",
            "target/custom",
        ])
        .expect("first-class Cargo build flags should parse");
        let Commands::Stress(args) = cli.cmd;
        let mut build = Command::new("cargo");

        apply_cargo_args(&mut build, &args);

        let build_args = build
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            build_args,
            [
                "--features",
                "io,net",
                "--no-default-features",
                "--target-dir",
                "target/custom",
            ]
        );

        let cli = Cli::try_parse_from(["cargo", "stress", "--all-features"])
            .expect("all-features should parse independently");
        let Commands::Stress(args) = cli.cmd;
        let mut all_features = Command::new("cargo");
        apply_cargo_args(&mut all_features, &args);
        assert_eq!(
            all_features
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            ["--all-features"]
        );
    }

    #[test]
    fn all_features_conflicts_with_feature_selection_modes() {
        for conflicting in ["--features", "--no-default-features"] {
            let mut argv = vec!["cargo", "stress", "--all-features", conflicting];
            if conflicting == "--features" {
                argv.push("io");
            }
            Cli::try_parse_from(argv).expect_err("conflicting Cargo feature modes must fail");
        }
    }

    #[test]
    fn first_class_cargo_flags_are_rejected_as_raw_arguments() {
        for flag in [
            "--features=io",
            "-Fio",
            "--all-features",
            "--no-default-features",
            "--target-dir=target/custom",
        ] {
            let args = StressArgs {
                cargo_args: vec![flag.to_string()],
                ..stress_args()
            };

            let error = validate_stress_args(&args)
                .expect_err("first-class Cargo flags must use their typed options");
            assert!(error.to_string().contains("first-class"));
        }
    }

    #[test]
    fn typed_target_is_forwarded_to_metadata_build_and_execution() {
        let cli = Cli::try_parse_from(["cargo", "stress", "--target", "aarch64-unknown-linux-gnu"])
            .expect("typed cross-target execution should use Cargo's configured runner");
        let Commands::Stress(args) = cli.cmd;
        let mut metadata = Command::new("cargo");
        apply_cargo_resolution_args(&mut metadata, &args);
        let metadata_args = metadata
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(metadata_args.windows(2).any(|window| {
            window[0] == "--filter-platform" && window[1] == "aarch64-unknown-linux-gnu"
        }));
        let mut cargo = Command::new("cargo");
        apply_cargo_args(&mut cargo, &args);
        let cargo_args = cargo
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(cargo_args
            .windows(2)
            .any(|window| { window[0] == "--target" && window[1] == "aarch64-unknown-linux-gnu" }));
        assert!(
            wrapper_build_input_identity_with_env(&args, std::iter::empty())
                .contains("aarch64-unknown-linux-gnu")
        );

        for cargo_args in [
            vec![
                "--target".to_string(),
                "aarch64-unknown-linux-gnu".to_string(),
            ],
            vec!["--target=aarch64-unknown-linux-gnu".to_string()],
        ] {
            let args = StressArgs {
                cargo_args,
                ..stress_args()
            };
            let error = validate_stress_args(&args)
                .expect_err("raw target selection must use the typed option");
            let message = error.to_string();
            assert!(message.contains("--target"));
            assert!(message.contains("first-class"));
        }
    }

    #[test]
    fn cargo_mediated_execution_honors_the_configured_host_runner() {
        let fixture = cargo_native_fixture();
        let manifest_path = fixture
            .path()
            .join("beta/Cargo.toml")
            .canonicalize()
            .expect("canonical fixture manifest");
        let args = StressArgs {
            manifest_path: Some(manifest_path.clone()),
            features: vec!["bench-fixture".to_string()],
            ..stress_args()
        };
        let metadata = load_cargo_metadata(&manifest_path, &args).expect("fixture metadata");
        let target = discover_stress_targets(&metadata, &manifest_path, &args)
            .expect("fixture stress target")
            .into_iter()
            .next()
            .expect("one stress target");
        let verbose_version = Command::new("rustc")
            .arg("-vV")
            .output()
            .expect("rustc version");
        let host = String::from_utf8(verbose_version.stdout)
            .expect("utf-8 rustc version")
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .expect("rustc host triple")
            .replace('-', "_")
            .to_ascii_uppercase();
        let runner_env = format!("CARGO_TARGET_{host}_RUNNER");
        let build_identity = wrapper_build_input_identity(&args, &metadata.workspace_root)
            .expect("runner build identity");
        let mut command = cargo_bench_command(
            &target,
            &args,
            &manifest_path,
            "runner-proof",
            &build_identity,
        );
        let output = command
            .env(runner_env, "false")
            .arg("--list")
            .output()
            .expect("Cargo-mediated runner probe");

        assert!(
            !output.status.success(),
            "Cargo must invoke the configured runner instead of executing the artifact directly"
        );
    }
}
