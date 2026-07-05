//! Harness for auto-discovered stress benchmarks.

use crate::config::{parse_bool_env, ConsoleMode, StressRunnerConfig};
use crate::result::{BenchmarkBudgets, BenchmarkModeKind, BenchmarkSpec, RunProfile};
use crate::runner::{evaluate_run_gate, RunGate, StressRunner};
use crate::StressContext;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A registered benchmark entry.
#[doc(hidden)]
pub struct BenchmarkEntry {
    /// Benchmark name.
    pub name: &'static str,
    /// Rust function name used for stable ids.
    pub function_name: &'static str,
    /// Benchmark function.
    pub func: fn(&mut StressContext),
    /// Whether this benchmark is ignored by default.
    pub ignored: bool,
    /// Module path where the benchmark is defined.
    pub module_path: &'static str,
    /// Numeric tier.
    pub tier: u32,
    /// Static benchmark mode kind.
    pub mode: BenchmarkModeKind,
    /// Static budget gates.
    pub budgets: BenchmarkBudgets,
    /// Static descriptive metadata.
    pub metadata: &'static [(&'static str, &'static str)],
}

#[doc(hidden)]
pub use linkme;

/// Distributed slice collecting all registered benchmarks.
#[doc(hidden)]
#[linkme::distributed_slice]
pub static STRESS_BENCHMARKS: [BenchmarkEntry];

#[derive(Debug, Clone, Default)]
struct StressBinaryArgs {
    workload: Option<String>,
    profile: Option<RunProfile>,
    tier: Option<u32>,
    samples: Option<usize>,
    warmup_samples: Option<usize>,
    cooldown_samples: Option<usize>,
    console: Option<ConsoleMode>,
    include_ignored: Option<bool>,
    list: bool,
    print_config: bool,
    output_dir: Option<PathBuf>,
    baseline: Option<PathBuf>,
    threshold: Option<f64>,
}

#[derive(Debug, Clone)]
struct ResolvedStressConfig {
    config: StressRunnerConfig,
    metadata: BTreeMap<String, String>,
    warnings: Vec<String>,
    workload: Option<String>,
    include_ignored: bool,
    baseline: Option<PathBuf>,
    print_config: bool,
}

impl StressBinaryArgs {
    fn parse() -> Self {
        let args = std::env::args().collect::<Vec<_>>();
        Self::parse_from_args(&args)
    }

    fn parse_from_args(args: &[String]) -> Self {
        let mut result = Self::default();
        let mut index = 1;

        while index < args.len() {
            match args[index].as_str() {
                "--workload" | "--filter" => {
                    index += 1;
                    if let Some(value) = args.get(index) {
                        result.workload = Some(value.clone());
                    }
                }
                "--profile" => {
                    index += 1;
                    if let Some(value) = args.get(index).and_then(|value| value.parse().ok()) {
                        result.profile = Some(value);
                    }
                }
                "--tier" => {
                    index += 1;
                    if let Some(value) = args.get(index).and_then(|value| value.parse().ok()) {
                        result.tier = Some(value);
                    }
                }
                "--samples" => {
                    index += 1;
                    if let Some(value) = args.get(index).and_then(|value| value.parse().ok()) {
                        result.samples = Some(value);
                    }
                }
                "--warmup-samples" => {
                    index += 1;
                    if let Some(value) = args.get(index).and_then(|value| value.parse().ok()) {
                        result.warmup_samples = Some(value);
                    }
                }
                "--cooldown-samples" => {
                    index += 1;
                    if let Some(value) = args.get(index).and_then(|value| value.parse().ok()) {
                        result.cooldown_samples = Some(value);
                    }
                }
                "--verbose" | "-v" => {
                    result.console = Some(ConsoleMode::Verbose);
                }
                "--console" => {
                    index += 1;
                    if let Some(value) = args.get(index).and_then(|value| value.parse().ok()) {
                        result.console = Some(value);
                    }
                }
                "--include-ignored" => {
                    result.include_ignored = Some(true);
                }
                "--list" => {
                    result.list = true;
                }
                "--print-config" | "--dry-run-config" => {
                    result.print_config = true;
                }
                "--output-dir" => {
                    index += 1;
                    if let Some(value) = args.get(index) {
                        result.output_dir = Some(PathBuf::from(value));
                    }
                }
                "--baseline" => {
                    index += 1;
                    if let Some(value) = args.get(index) {
                        result.baseline = Some(PathBuf::from(value));
                    }
                }
                "--threshold" => {
                    index += 1;
                    if let Some(value) = args.get(index).and_then(|value| value.parse().ok()) {
                        result.threshold = Some(value);
                    }
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => {}
            }
            index += 1;
        }

        result
    }
}

fn print_help() {
    eprintln!("Stress benchmark binary");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    <binary> [OPTIONS]");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!(
        "    --profile <default|smoke|lab|release>  Optional profile override; default is default"
    );
    eprintln!("    --workload <PATTERN>           Filter benchmarks by name/module glob");
    eprintln!("    --tier <N>                     Run one numeric tier");
    eprintln!("    --samples <N>                  Measured samples per benchmark");
    eprintln!("    --warmup-samples <N>           Warmup samples");
    eprintln!("    --cooldown-samples <N>         Cooldown samples");
    eprintln!("    --console <MODE>               compact, full, verbose, ci, or json");
    eprintln!("    -v, --verbose                  Shortcut for --console verbose");
    eprintln!("    --include-ignored              Include ignored benchmarks");
    eprintln!("    --list                         List benchmarks");
    eprintln!("    --print-config                 Print resolved config");
    eprintln!("    --output-dir <PATH>            Artifact output directory");
    eprintln!("    --baseline <PATH>              Current baseline artifact");
    eprintln!("    --threshold <FLOAT>            Regression threshold");
}

/// Entry point used by `stress_main!`.
pub fn stress_binary_main() {
    run_from_env_and_args();
}

/// Parse environment/CLI and run registered benchmarks.
pub fn run_from_env_and_args() {
    let args = StressBinaryArgs::parse();

    if args.list {
        print_benchmark_list();
        return;
    }

    let resolved = resolve_from_binary_args(&args);
    for warning in &resolved.warnings {
        eprintln!("Warning: {warning}");
    }

    exit_on_invalid_config(&resolved.config);

    if resolved.print_config {
        print_resolved_config(&get_suite_name(), &resolved);
        return;
    }

    run_with_resolved_config(resolved);
}

/// Options for programmatic execution of registered benchmarks.
#[derive(Debug, Clone, Default)]
pub struct StressRunnerOptions {
    /// Benchmark name/module filter.
    pub workload: Option<String>,
    /// Include ignored benchmarks.
    pub include_ignored: bool,
    /// Run profile.
    pub profile: Option<RunProfile>,
    /// Exact tier filter.
    pub tier: Option<u32>,
    /// Measured samples.
    pub samples: Option<usize>,
    /// Warmup samples.
    pub warmup_samples: Option<usize>,
    /// Console output mode.
    pub console: Option<ConsoleMode>,
    /// Baseline artifact.
    pub baseline: Option<PathBuf>,
    /// Regression threshold.
    pub threshold: Option<f64>,
}

impl StressRunnerOptions {
    /// Create default options.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set workload filter.
    #[must_use]
    pub fn workload(mut self, pattern: impl Into<String>) -> Self {
        self.workload = Some(pattern.into());
        self
    }

    /// Include ignored benchmarks.
    #[must_use]
    pub fn include_ignored(mut self, value: bool) -> Self {
        self.include_ignored = value;
        self
    }

    /// Set profile.
    #[must_use]
    pub const fn profile(mut self, profile: RunProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Set exact tier filter.
    #[must_use]
    pub const fn tier(mut self, tier: u32) -> Self {
        self.tier = Some(tier);
        self
    }

    /// Set measured samples.
    #[must_use]
    pub const fn samples(mut self, samples: usize) -> Self {
        self.samples = Some(samples);
        self
    }

    /// Set warmup samples.
    #[must_use]
    pub const fn warmup_samples(mut self, warmup_samples: usize) -> Self {
        self.warmup_samples = Some(warmup_samples);
        self
    }

    /// Set console output mode.
    #[must_use]
    pub const fn console(mut self, value: ConsoleMode) -> Self {
        self.console = Some(value);
        self
    }

    /// Set verbose console output.
    #[must_use]
    pub const fn verbose(mut self, value: bool) -> Self {
        self.console = Some(if value {
            ConsoleMode::Verbose
        } else {
            ConsoleMode::Compact
        });
        self
    }

    /// Set baseline artifact.
    #[must_use]
    pub fn baseline(mut self, path: impl Into<PathBuf>) -> Self {
        self.baseline = Some(path.into());
        self
    }

    /// Set regression threshold.
    #[must_use]
    pub const fn threshold(mut self, threshold: f64) -> Self {
        self.threshold = Some(threshold);
        self
    }
}

/// Run all registered benchmarks with default options.
pub fn run_registered_benchmarks() {
    run_with_options(StressRunnerOptions::new());
}

/// Run all registered benchmarks with programmatic options.
pub fn run_with_options(options: StressRunnerOptions) {
    let args = StressBinaryArgs {
        workload: options.workload,
        profile: options.profile,
        tier: options.tier,
        samples: options.samples,
        warmup_samples: options.warmup_samples,
        console: options.console,
        include_ignored: Some(options.include_ignored),
        baseline: options.baseline,
        threshold: options.threshold,
        ..StressBinaryArgs::default()
    };
    run_with_resolved_config(resolve_from_binary_args(&args));
}

fn resolve_from_binary_args(args: &StressBinaryArgs) -> ResolvedStressConfig {
    resolve_from_binary_args_with(args, |key| std::env::var(key).ok())
}

fn resolve_from_binary_args_with<F>(args: &StressBinaryArgs, get_var: F) -> ResolvedStressConfig
where
    F: Fn(&str) -> Option<String>,
{
    let env_resolution = StressRunnerConfig::resolve_from_env_with_profile_override(
        &get_var,
        args.profile.map(|profile| (profile, "cli --profile")),
    );
    let mut config = env_resolution.config;
    let mut metadata = env_resolution
        .metadata
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let mut warnings = env_resolution.warnings;

    let mut include_ignored = get_var("STRESS_INCLUDE_IGNORED")
        .and_then(|value| {
            if let Some(value) = parse_bool_env(&value) {
                Some(value)
            } else {
                warnings.push("invalid STRESS_INCLUDE_IGNORED, using false".to_string());
                None
            }
        })
        .unwrap_or(false);
    metadata.insert(
        "include_ignored_src".to_string(),
        source_for(&get_var, "STRESS_INCLUDE_IGNORED"),
    );

    let mut baseline = get_var("STRESS_BASELINE").map(PathBuf::from);
    metadata.insert(
        "baseline_src".to_string(),
        source_for(&get_var, "STRESS_BASELINE"),
    );

    if let Some(tier) = args.tier {
        config.tier = Some(tier);
        metadata.insert("tier_src".to_string(), "cli --tier".to_string());
    }
    if let Some(samples) = args.samples {
        config.samples = samples;
        metadata.insert("samples_src".to_string(), "cli --samples".to_string());
    }
    if let Some(warmup_samples) = args.warmup_samples {
        config.warmup_samples = warmup_samples;
        metadata.insert(
            "warmup_samples_src".to_string(),
            "cli --warmup-samples".to_string(),
        );
    }
    if let Some(cooldown_samples) = args.cooldown_samples {
        config.cooldown_samples = cooldown_samples;
        metadata.insert(
            "cooldown_samples_src".to_string(),
            "cli --cooldown-samples".to_string(),
        );
    }
    if let Some(console) = args.console {
        config.console = console;
        metadata.insert(
            "console_src".to_string(),
            format!("cli --console {console}"),
        );
    }
    if let Some(output_dir) = &args.output_dir {
        config.output_dir.clone_from(output_dir);
        metadata.insert("output_dir_src".to_string(), "cli --output-dir".to_string());
    }
    if let Some(workload) = &args.workload {
        config.filter = Some(workload.clone());
        metadata.insert("filter_src".to_string(), "cli --workload".to_string());
    }
    if let Some(value) = args.include_ignored {
        include_ignored = value;
        metadata.insert(
            "include_ignored_src".to_string(),
            "cli --include-ignored".to_string(),
        );
    }
    if let Some(path) = &args.baseline {
        baseline = Some(path.clone());
        metadata.insert("baseline_src".to_string(), "cli --baseline".to_string());
    }
    if let Some(threshold) = args.threshold {
        config.threshold = threshold;
        metadata.insert("threshold_src".to_string(), "cli --threshold".to_string());
    }

    ResolvedStressConfig {
        workload: config.filter.clone(),
        config,
        metadata,
        warnings,
        include_ignored,
        baseline,
        print_config: args.print_config,
    }
}

fn exit_on_invalid_config(config: &StressRunnerConfig) {
    let errors = config.validation_errors();
    if errors.is_empty() {
        return;
    }

    eprintln!("Invalid stress config: {}", errors.join("; "));
    std::process::exit(1);
}

fn run_with_resolved_config(resolved: ResolvedStressConfig) {
    exit_on_invalid_config(&resolved.config);

    let benchmarks = selected_benchmarks(&resolved);
    if benchmarks.is_empty() {
        eprintln!("{}", empty_selection_error(&resolved));
        std::process::exit(1);
    }

    let suite_name = get_suite_name();
    let config_for_specs = resolved.config.clone();
    let mut runner =
        StressRunner::with_config_and_metadata(&suite_name, resolved.config, resolved.metadata);

    for entry in benchmarks {
        let stable_name = format!("{}::{}", entry.module_path, entry.function_name);
        let display_name = format!("{}::{}", entry.module_path, entry.name);
        let id = format!("{suite_name}/{stable_name}");
        let metadata = entry
            .metadata
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<BTreeMap<_, _>>();
        let spec = BenchmarkSpec {
            id,
            name: display_name,
            tier: entry.tier,
            mode: config_for_specs.mode_for_kind(entry.mode),
            intent: crate::result::MeasurementIntent::General,
            budgets: entry.budgets,
            parameters: BTreeMap::new(),
            metadata,
        };
        runner.run_spec(&spec, entry.func);
    }

    let run_result = if let Some(baseline_path) = resolved.baseline {
        runner.finish_with_baseline(baseline_path)
    } else {
        Ok(runner.finish())
    };

    match run_result {
        Ok(run) => {
            let gate = evaluate_run_gate(&run);
            if gate != RunGate::Passed {
                eprintln!("Stress run failed: {gate:?}");
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("Stress run failed to load baseline: {error}");
            std::process::exit(1);
        }
    }
}

fn empty_selection_error(resolved: &ResolvedStressConfig) -> &'static str {
    if resolved.workload.is_some() {
        "No benchmarks matched the workload pattern"
    } else {
        "No benchmarks registered. Add #[stress] to benchmark functions."
    }
}

fn selected_benchmarks(resolved: &ResolvedStressConfig) -> Vec<&'static BenchmarkEntry> {
    STRESS_BENCHMARKS
        .iter()
        .filter(|entry| {
            if entry.ignored && !resolved.include_ignored {
                return false;
            }
            if let Some(tier) = resolved.config.tier {
                if entry.tier != tier {
                    return false;
                }
            }
            if let Some(pattern) = &resolved.workload {
                matches_glob(entry.name, pattern) || matches_glob(entry.module_path, pattern)
            } else {
                true
            }
        })
        .collect()
}

fn print_benchmark_list() {
    let benchmarks = list_benchmarks();
    if benchmarks.is_empty() {
        println!("No benchmarks registered.");
    } else {
        println!("Registered benchmarks ({}):", benchmarks.len());
        for benchmark in benchmarks {
            println!("  {benchmark}");
        }
    }
}

fn get_suite_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .map_or_else(|| "stress".to_string(), |name| clean_exe_name(&name))
}

fn clean_exe_name(name: &str) -> String {
    let clean_name = if let Some(dash_pos) = name.rfind('-') {
        let potential_hash = &name[dash_pos + 1..];
        if potential_hash.len() == 16 && potential_hash.chars().all(|char| char.is_ascii_hexdigit())
        {
            &name[..dash_pos]
        } else {
            name
        }
    } else {
        name
    };
    clean_name.replace('_', "-")
}

fn print_resolved_config(suite: &str, resolved: &ResolvedStressConfig) {
    println!("Benchmark Suite: {suite}");
    println!(
        "Profile: {} ({})",
        resolved.config.profile,
        resolved
            .metadata
            .get("profile_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Samples: {} ({})",
        resolved.config.samples,
        resolved
            .metadata
            .get("samples_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Warmup samples: {} ({})",
        resolved.config.warmup_samples,
        resolved
            .metadata
            .get("warmup_samples_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Cooldown samples: {} ({})",
        resolved.config.cooldown_samples,
        resolved
            .metadata
            .get("cooldown_samples_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Output: {} ({})",
        resolved.config.output_dir.display(),
        resolved
            .metadata
            .get("output_dir_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Filter: {} ({})",
        resolved.config.filter.as_deref().unwrap_or("<none>"),
        resolved
            .metadata
            .get("filter_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Tier: {} ({})",
        resolved
            .config
            .tier
            .map_or_else(|| "<any>".to_string(), |tier| tier.to_string()),
        resolved
            .metadata
            .get("tier_src")
            .map_or("unknown", String::as_str)
    );
    println!("Console: {}", resolved.config.console);
    println!("Include ignored: {}", resolved.include_ignored);
    println!(
        "Baseline: {} ({})",
        resolved
            .baseline
            .as_ref()
            .map_or_else(|| "<none>".to_string(), |path| path.display().to_string()),
        resolved
            .metadata
            .get("baseline_src")
            .map_or("unknown", String::as_str)
    );
    println!(
        "Threshold: {} ({})",
        resolved.config.threshold,
        resolved
            .metadata
            .get("threshold_src")
            .map_or("unknown", String::as_str)
    );
}

fn source_for<F>(get_var: &F, env_key: &'static str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    if get_var(env_key).is_some() {
        format!("env {env_key}")
    } else {
        "default".to_string()
    }
}

fn matches_glob(text: &str, pattern: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let text = text.to_lowercase();

    if pattern.contains('*') {
        let parts = pattern.split('*').collect::<Vec<_>>();
        let mut remaining = text.as_str();

        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if index == 0 {
                if !remaining.starts_with(part) {
                    return false;
                }
                remaining = &remaining[part.len()..];
            } else if index == parts.len() - 1 && !pattern.ends_with('*') {
                return remaining.ends_with(part);
            } else if let Some(pos) = remaining.find(part) {
                remaining = &remaining[pos + part.len()..];
            } else {
                return false;
            }
        }
        true
    } else {
        text.contains(&pattern)
    }
}

/// Get a list of registered benchmark names.
#[must_use]
pub fn list_benchmarks() -> Vec<&'static str> {
    STRESS_BENCHMARKS.iter().map(|entry| entry.name).collect()
}

/// Get the number of registered benchmarks.
#[must_use]
pub fn benchmark_count() -> usize {
    STRESS_BENCHMARKS.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_substring_and_wildcards() {
        assert!(matches_glob("foo_bar_baz", "bar"));
        assert!(matches_glob("foo_bar_baz", "foo*baz"));
        assert!(matches_glob("foo_bar_baz", "*bar*"));
        assert!(!matches_glob("foo_bar_baz", "qux*"));
    }

    #[test]
    fn parse_args_uses_new_sample_names() {
        let args = vec![
            "stress-demo".to_string(),
            "--profile".to_string(),
            "release".to_string(),
            "--samples".to_string(),
            "4".to_string(),
            "--warmup-samples".to_string(),
            "2".to_string(),
        ];
        let parsed = StressBinaryArgs::parse_from_args(&args);

        assert_eq!(parsed.profile, Some(RunProfile::Release));
        assert_eq!(parsed.samples, Some(4));
        assert_eq!(parsed.warmup_samples, Some(2));
    }

    #[test]
    fn resolve_from_binary_args_uses_stress_env() {
        let args = StressBinaryArgs::default();
        let env = BTreeMap::from([
            ("STRESS_PROFILE", "release".to_string()),
            ("STRESS_SAMPLES", "3".to_string()),
            ("STRESS_WARMUP_SAMPLES", "1".to_string()),
            ("STRESS_INCLUDE_IGNORED", "true".to_string()),
        ]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(resolved.config.profile, RunProfile::Release);
        assert_eq!(resolved.config.samples, 3);
        assert_eq!(resolved.config.warmup_samples, 1);
        assert!(resolved.include_ignored);
    }

    #[test]
    fn cli_overrides_env() {
        let args = StressBinaryArgs {
            profile: Some(RunProfile::Lab),
            samples: Some(5),
            ..StressBinaryArgs::default()
        };
        let env = BTreeMap::from([
            ("STRESS_PROFILE", "release".to_string()),
            ("STRESS_SAMPLES", "3".to_string()),
        ]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(resolved.config.profile, RunProfile::Lab);
        assert_eq!(resolved.config.samples, 5);
        assert_eq!(
            resolved.metadata.get("samples_src"),
            Some(&"cli --samples".to_string())
        );
    }

    #[test]
    fn cli_profile_is_base_before_env_overrides() {
        let args = StressBinaryArgs {
            profile: Some(RunProfile::Lab),
            ..StressBinaryArgs::default()
        };
        let env = BTreeMap::from([
            ("STRESS_PROFILE", "release".to_string()),
            ("STRESS_SAMPLES", "3".to_string()),
            ("STRESS_THRESHOLD", "0.20".to_string()),
        ]);

        let resolved = resolve_from_binary_args_with(&args, |key| env.get(key).cloned());

        assert_eq!(resolved.config.profile, RunProfile::Lab);
        assert_eq!(resolved.config.samples, 3);
        assert!((resolved.config.threshold - 0.20).abs() < f64::EPSILON);
        assert_eq!(
            resolved.metadata.get("profile_src"),
            Some(&"cli --profile".to_string())
        );
        assert_eq!(
            resolved.metadata.get("samples_src"),
            Some(&"env STRESS_SAMPLES".to_string())
        );
        assert_eq!(
            resolved.metadata.get("threshold_src"),
            Some(&"env STRESS_THRESHOLD".to_string())
        );
    }

    #[test]
    fn empty_workload_selection_has_fatal_error_message() {
        let args = StressBinaryArgs {
            workload: Some("definitely_no_such_benchmark".to_string()),
            ..StressBinaryArgs::default()
        };
        let resolved = resolve_from_binary_args_with(&args, |_| None);

        assert!(selected_benchmarks(&resolved).is_empty());
        assert_eq!(
            empty_selection_error(&resolved),
            "No benchmarks matched the workload pattern"
        );
    }
}
