//! Read-only run history over immutable timestamped artifacts, plus a
//! guarded prune of old artifact sets.
//!
//! A suite directory (`target/stress/<suite>/`, or
//! `target/stress/<package>/<suite>/` under `cargo stress`) holds one
//! artifact set per run: `{stem}.json` plus sibling `{stem}.txt`, `.md`, and
//! `.csv` files, where `stem` is the run's `started_at`. History reads only
//! those sets. It never reads or touches `latest.*`, the baselines
//! directory, or publication state.

use crate::artifact::{gated_confidence_interval, incompatible_environment_reason, StressRun};
use crate::csv::{csv_number_cell, csv_record, csv_text_cell};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Name of the default baselines directory under `target/stress`.
const BASELINES_DIR: &str = "baselines";

/// Filters for [`load_history`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct HistoryOptions {
    /// Only suites with this name (suite field or directory name).
    pub suite: Option<String>,
    /// Only benchmark rows whose id contains this substring.
    pub bench: Option<String>,
    /// Only the most recent N compatible runs per suite.
    pub last: Option<usize>,
}

impl HistoryOptions {
    /// No filters.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by suite name.
    #[must_use]
    pub fn suite(mut self, suite: impl Into<String>) -> Self {
        self.suite = Some(suite.into());
        self
    }

    /// Filter benchmark ids by substring.
    #[must_use]
    pub fn bench(mut self, bench: impl Into<String>) -> Self {
        self.bench = Some(bench.into());
        self
    }

    /// Keep only the most recent `last` compatible runs.
    #[must_use]
    pub const fn last(mut self, last: usize) -> Self {
        self.last = Some(last);
        self
    }
}

/// One run's value for one benchmark.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[non_exhaustive]
pub struct HistoryPoint {
    /// Artifact stem (the run's `started_at`).
    pub started_at: String,
    /// UTC timestamp derived from the stem, when it has the standard form.
    pub timestamp: Option<String>,
    /// Git commit recorded by the run.
    pub git_commit: Option<String>,
    /// Primary (gated) value.
    pub value: Option<f64>,
    /// Unit of `value`, e.g. `op/s` or `ns/op`.
    pub unit: String,
    /// Lower bound of the gated 95% confidence interval.
    pub ci_lower: Option<f64>,
    /// Upper bound of the gated 95% confidence interval.
    pub ci_upper: Option<f64>,
    /// Quality class.
    pub quality: String,
}

/// History of one benchmark row, grouped by git commit.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[non_exhaustive]
pub struct BenchmarkHistory {
    /// Benchmark row id.
    pub benchmark_id: String,
    /// Groups of points sharing a git commit, oldest group first; points in
    /// a group are oldest first.
    pub commits: Vec<CommitGroup>,
}

/// Points recorded at one git commit.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[non_exhaustive]
pub struct CommitGroup {
    /// Git commit, or `None` when runs did not record one.
    pub git_commit: Option<String>,
    /// Points at this commit, oldest first.
    pub points: Vec<HistoryPoint>,
}

/// A run left out of the history, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct SkippedRun {
    /// Artifact file.
    pub path: PathBuf,
    /// Why it was skipped.
    pub reason: String,
}

/// History of one suite directory.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[non_exhaustive]
pub struct SuiteHistory {
    /// Suite name.
    pub suite: String,
    /// Suite directory.
    pub directory: PathBuf,
    /// `started_at` of the most recent run, which defines compatibility.
    pub reference_run: Option<String>,
    /// Number of compatible runs included.
    pub runs: usize,
    /// Per-benchmark history.
    pub benchmarks: Vec<BenchmarkHistory>,
    /// Runs left out (incompatible environment or unreadable).
    pub skipped: Vec<SkippedRun>,
}

/// Whether `name` is a timestamped artifact stem file for `extension`
/// (never `latest.*` or a hidden coordination file).
fn stem_of(name: &str) -> Option<&str> {
    let (stem, _extension) = name.rsplit_once('.')?;
    (!stem.is_empty() && stem != "latest" && !stem.starts_with('.') && !stem.contains('.'))
        .then_some(stem)
}

/// Timestamped JSON stems in a suite directory, oldest first.
fn history_stems(directory: &Path) -> std::io::Result<Vec<String>> {
    let mut stems = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| {
            name.strip_suffix(".json")
                .and_then(|base| stem_of(&name).filter(|stem| *stem == base))
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    stems.sort();
    Ok(stems)
}

fn is_suite_directory(directory: &Path) -> bool {
    directory.join("latest.json").is_file()
        || history_stems(directory).is_ok_and(|stems| !stems.is_empty())
}

/// Suite directories under an artifact root: the root itself when it is a
/// suite directory, else its children and grandchildren (the `cargo stress`
/// package layout). The baselines directory is never included.
///
/// # Errors
///
/// Returns an error when the root cannot be read.
pub fn discover_suite_directories(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    if is_suite_directory(root) {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut found = Vec::new();
    let children = |directory: &Path| -> std::io::Result<Vec<PathBuf>> {
        let mut paths = std::fs::read_dir(directory)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                !name.starts_with('.') && name != BASELINES_DIR
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    };
    for child in children(root)? {
        if is_suite_directory(&child) {
            found.push(child);
        } else if let Ok(grandchildren) = children(&child) {
            found.extend(
                grandchildren
                    .into_iter()
                    .filter(|path| is_suite_directory(path)),
            );
        }
    }
    Ok(found)
}

/// Format a standard `started_at` stem (epoch nanoseconds first) as UTC.
fn stem_timestamp(stem: &str) -> Option<String> {
    let nanos = stem.split('-').next()?.parse::<u128>().ok()?;
    let seconds = i64::try_from(nanos / 1_000_000_000).ok()?;
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant), proleptic Gregorian.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3_600,
        time % 3_600 / 60,
        time % 60
    ))
}

fn serde_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(ToString::to_string))
        .unwrap_or_default()
}

/// Per-benchmark points from compatible runs (oldest first), grouped by
/// git commit.
fn group_benchmarks(compatible: &[StressRun], options: &HistoryOptions) -> Vec<BenchmarkHistory> {
    let mut benchmarks: Vec<BenchmarkHistory> = Vec::new();
    for run in compatible {
        for summary in &run.summaries {
            if options
                .bench
                .as_ref()
                .is_some_and(|bench| !summary.benchmark_id.contains(bench.as_str()))
            {
                continue;
            }
            let interval = gated_confidence_interval(summary);
            let unit = crate::csv::primary_unit(summary);
            let point = HistoryPoint {
                started_at: run.started_at.clone(),
                timestamp: stem_timestamp(&run.started_at),
                git_commit: run.environment.git_commit.clone(),
                value: summary.primary_value(),
                unit,
                ci_lower: interval.map(|interval| interval.lower),
                ci_upper: interval.map(|interval| interval.upper),
                quality: serde_name(&summary.quality),
            };
            let index = if let Some(index) = benchmarks
                .iter()
                .position(|history| history.benchmark_id == summary.benchmark_id)
            {
                index
            } else {
                benchmarks.push(BenchmarkHistory {
                    benchmark_id: summary.benchmark_id.clone(),
                    commits: Vec::new(),
                });
                benchmarks.len() - 1
            };
            let commits = &mut benchmarks[index].commits;
            match commits
                .iter_mut()
                .find(|group| group.git_commit == point.git_commit)
            {
                Some(group) => group.points.push(point),
                None => commits.push(CommitGroup {
                    git_commit: point.git_commit.clone(),
                    points: vec![point],
                }),
            }
        }
    }
    // Groups are ordered by their most recent point, so the current commit
    // is last.
    for history in &mut benchmarks {
        history.commits.sort_by(|left, right| {
            let latest =
                |group: &CommitGroup| group.points.last().map(|point| point.started_at.clone());
            latest(left).cmp(&latest(right))
        });
    }
    benchmarks.sort_by(|left, right| left.benchmark_id.cmp(&right.benchmark_id));
    benchmarks
}

/// Load the history of one suite directory.
///
/// Runs whose environment is incompatible with the most recent readable run
/// are skipped and listed, as are unreadable artifacts.
///
/// # Errors
///
/// Returns an error when the directory cannot be read.
pub fn load_suite_history(
    directory: &Path,
    options: &HistoryOptions,
) -> std::io::Result<Option<SuiteHistory>> {
    let mut skipped = Vec::new();
    let mut runs = Vec::new();
    for stem in history_stems(directory)? {
        let path = directory.join(format!("{stem}.json"));
        match StressRun::load(&path) {
            Ok(run) if run.started_at == stem => runs.push(run),
            Ok(run) => skipped.push(SkippedRun {
                path,
                reason: format!("started_at {} does not match the file name", run.started_at),
            }),
            Err(error) => skipped.push(SkippedRun {
                path,
                reason: format!("unreadable artifact: {error}"),
            }),
        }
    }
    let directory_name = directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(suite) = &options.suite {
        let matches = directory_name == *suite || runs.iter().any(|run| run.suite == *suite);
        if !matches {
            return Ok(None);
        }
    }
    runs.sort_by(|left, right| left.started_at.cmp(&right.started_at));
    let reference = runs.last().cloned();
    let mut compatible = Vec::new();
    for run in runs {
        let reason = reference.as_ref().and_then(|reference| {
            incompatible_environment_reason(&run.environment, &reference.environment)
        });
        match reason {
            Some(reason) => skipped.push(SkippedRun {
                path: directory.join(format!("{}.json", run.started_at)),
                reason,
            }),
            None => compatible.push(run),
        }
    }
    if let Some(last) = options.last {
        let excess = compatible.len().saturating_sub(last);
        compatible.drain(..excess);
    }
    skipped.sort_by(|left, right| left.path.cmp(&right.path));

    let benchmarks = group_benchmarks(&compatible, options);
    let suite = reference
        .as_ref()
        .map_or(directory_name, |run| run.suite.clone());
    Ok(Some(SuiteHistory {
        suite,
        directory: directory.to_path_buf(),
        reference_run: reference.map(|run| run.started_at),
        runs: compatible.len(),
        benchmarks,
        skipped,
    }))
}

/// Load history for every suite directory under `root`.
///
/// # Errors
///
/// Returns an error when `root` cannot be read.
pub fn load_history(root: &Path, options: &HistoryOptions) -> std::io::Result<Vec<SuiteHistory>> {
    let mut histories = Vec::new();
    for directory in discover_suite_directories(root)? {
        if let Some(history) = load_suite_history(&directory, options)? {
            histories.push(history);
        }
    }
    Ok(histories)
}

fn short_commit(commit: Option<&str>) -> String {
    commit.map_or_else(
        || "unknown".to_string(),
        |commit| commit.chars().take(12).collect(),
    )
}

fn number(value: Option<f64>) -> String {
    value
        .filter(|value| value.is_finite())
        .map_or_else(|| "n/a".to_string(), |value| format!("{value:.4}"))
}

fn interval(point: &HistoryPoint) -> String {
    match (point.ci_lower, point.ci_upper) {
        (Some(lower), Some(upper)) if lower.is_finite() && upper.is_finite() => {
            format!("[{lower:.4}, {upper:.4}]")
        }
        _ => "n/a".to_string(),
    }
}

fn point_rows(history: &BenchmarkHistory) -> impl Iterator<Item = &HistoryPoint> {
    history.commits.iter().flat_map(|group| group.points.iter())
}

/// Render history as aligned text tables.
#[must_use]
pub fn render_text(histories: &[SuiteHistory]) -> String {
    let mut output = String::new();
    for suite in histories {
        let _ = writeln!(
            output,
            "suite {} ({} compatible runs, {} skipped) in {}",
            suite.suite,
            suite.runs,
            suite.skipped.len(),
            suite.directory.display()
        );
        for history in &suite.benchmarks {
            let _ = writeln!(output, "\n  {}", history.benchmark_id);
            let _ = writeln!(
                output,
                "    {:<20}  {:<12}  {:>16}  {:<31}  quality",
                "timestamp", "sha", "value", "ci95"
            );
            for group in &history.commits {
                for point in &group.points {
                    let _ = writeln!(
                        output,
                        "    {:<20}  {:<12}  {:>16}  {:<31}  {}",
                        point.timestamp.as_deref().unwrap_or(&point.started_at),
                        short_commit(group.git_commit.as_deref()),
                        format!("{} {}", number(point.value), point.unit),
                        interval(point),
                        point.quality
                    );
                }
            }
        }
        if !suite.skipped.is_empty() {
            let _ = writeln!(output, "\n  skipped runs:");
            for skipped in &suite.skipped {
                let _ = writeln!(output, "    {}: {}", skipped.path.display(), skipped.reason);
            }
        }
        output.push('\n');
    }
    if histories.is_empty() {
        output.push_str("no timestamped artifacts found\n");
    }
    output
}

fn markdown_cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

/// Render history as Markdown tables.
#[must_use]
pub fn render_markdown(histories: &[SuiteHistory]) -> String {
    let mut output = String::new();
    for suite in histories {
        let _ = writeln!(
            output,
            "## {}\n\n{} compatible runs, {} skipped.\n",
            markdown_cell(&suite.suite),
            suite.runs,
            suite.skipped.len()
        );
        for history in &suite.benchmarks {
            let _ = writeln!(output, "### {}\n", markdown_cell(&history.benchmark_id));
            output.push_str("| timestamp | sha | value | ci95 | quality |\n");
            output.push_str("|---|---|---:|---|---|\n");
            for group in &history.commits {
                for point in &group.points {
                    let _ = writeln!(
                        output,
                        "| {} | `{}` | {} {} | {} | {} |",
                        markdown_cell(point.timestamp.as_deref().unwrap_or(&point.started_at)),
                        markdown_cell(&short_commit(group.git_commit.as_deref())),
                        number(point.value),
                        markdown_cell(&point.unit),
                        interval(point),
                        markdown_cell(&point.quality)
                    );
                }
            }
            output.push('\n');
        }
        if !suite.skipped.is_empty() {
            output.push_str("Skipped runs:\n\n");
            for skipped in &suite.skipped {
                let _ = writeln!(
                    output,
                    "- `{}`: {}",
                    markdown_cell(&skipped.path.display().to_string()),
                    markdown_cell(&skipped.reason)
                );
            }
            output.push('\n');
        }
    }
    output
}

/// Render history as CSV (one row per benchmark per run).
#[must_use]
pub fn render_csv(histories: &[SuiteHistory]) -> String {
    let header = [
        "suite",
        "benchmark_id",
        "started_at",
        "timestamp",
        "git_commit",
        "value",
        "unit",
        "ci_lower",
        "ci_upper",
        "quality",
    ]
    .map(ToString::to_string);
    let mut output = csv_record(&header);
    for suite in histories {
        for history in &suite.benchmarks {
            for point in point_rows(history) {
                output.push_str(&csv_record(&[
                    csv_text_cell(&suite.suite),
                    csv_text_cell(&history.benchmark_id),
                    csv_text_cell(&point.started_at),
                    csv_text_cell(point.timestamp.as_deref().unwrap_or_default()),
                    csv_text_cell(point.git_commit.as_deref().unwrap_or_default()),
                    csv_number_cell(point.value),
                    csv_text_cell(&point.unit),
                    csv_number_cell(point.ci_lower),
                    csv_number_cell(point.ci_upper),
                    csv_text_cell(&point.quality),
                ]));
            }
        }
    }
    output
}

/// Render history as pretty JSON.
#[must_use]
pub fn render_json(histories: &[SuiteHistory]) -> String {
    let mut output = serde_json::to_string_pretty(histories).unwrap_or_else(|_| "[]".to_string());
    output.push('\n');
    output
}

/// Artifact sets a prune would delete from one suite directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct PrunePlan {
    /// Suite directory.
    pub directory: PathBuf,
    /// Stems kept (newest).
    pub kept: Vec<String>,
    /// Files to delete, grouped by stem, oldest first.
    pub delete: Vec<PathBuf>,
}

/// Files belonging to the artifact set `stem`: `{stem}.<ext>` regular files.
fn stem_files(directory: &Path, stem: &str) -> std::io::Result<Vec<PathBuf>> {
    let prefix = format!("{stem}.");
    let mut files = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let extension = name.strip_prefix(&prefix)?;
            (!extension.is_empty() && !extension.contains('.') && stem_of(&name) == Some(stem))
                .then(|| entry.path())
        })
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

/// Plan deleting every artifact set older than the newest `keep` in one
/// suite directory. Only sets whose `{stem}.json` is a run artifact with a
/// matching `started_at` are candidates; `latest.*`, hidden files, and other
/// files are never included.
///
/// # Errors
///
/// Returns an error when the directory cannot be read.
pub fn plan_prune(directory: &Path, keep: usize) -> std::io::Result<PrunePlan> {
    let stems = history_stems(directory)?
        .into_iter()
        .filter(|stem| {
            StressRun::load(directory.join(format!("{stem}.json")))
                .is_ok_and(|run| run.started_at == *stem)
        })
        .collect::<Vec<_>>();
    let split = stems.len().saturating_sub(keep);
    let mut delete = Vec::new();
    for stem in &stems[..split] {
        delete.extend(stem_files(directory, stem)?);
    }
    Ok(PrunePlan {
        directory: directory.to_path_buf(),
        kept: stems[split..].to_vec(),
        delete,
    })
}

/// Delete the planned artifact sets while holding the suite's publication
/// lock, re-planning under the lock so a concurrent publisher is never
/// raced. Refuses to run while an interrupted publication awaits recovery.
///
/// # Errors
///
/// Returns an error when the lock cannot be taken, recovery state exists,
/// or a file cannot be removed.
pub fn apply_prune(directory: &Path, keep: usize) -> std::io::Result<PrunePlan> {
    let _lock = crate::reporting::acquire_artifact_publication_lock(directory)?;
    if crate::reporting::has_artifact_transaction_state(directory)? {
        return Err(std::io::Error::other(format!(
            "{} has an interrupted artifact publication; run the suite once to recover it before pruning",
            directory.display()
        )));
    }
    let plan = plan_prune(directory, keep)?;
    for path in &plan.delete {
        std::fs::remove_file(path)?;
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{QualityClass, RunProfile};
    use crate::{StressRunner, StressRunnerConfig};
    use std::time::Duration;

    struct Dir(PathBuf);
    impl Dir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "stress-history-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run(suite: &str, started_at: &str, sha: Option<&str>, millis: u64) -> StressRun {
        let config = StressRunnerConfig::for_profile(RunProfile::Default)
            .samples(10)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = StressRunner::with_config(suite, config);
        runner.reporters(Vec::new());
        for id in ["queue::push", "map::insert"] {
            runner.run(id, |ctx| {
                ctx.record_external("work", Duration::from_millis(millis), 500);
            });
        }
        let mut run = runner.finish();
        assert!(run.meets_min_quality(QualityClass::Acceptable));
        run.started_at = started_at.to_string();
        run.environment.git_commit = sha.map(ToString::to_string);
        run
    }

    fn write(directory: &Path, run: &StressRun) {
        std::fs::create_dir_all(directory).unwrap();
        let json = serde_json::to_string(run).unwrap();
        std::fs::write(directory.join(format!("{}.json", run.started_at)), &json).unwrap();
        for extension in ["txt", "md", "csv"] {
            std::fs::write(
                directory.join(format!("{}.{extension}", run.started_at)),
                "x",
            )
            .unwrap();
        }
        std::fs::write(directory.join("latest.json"), &json).unwrap();
        std::fs::write(directory.join("latest.txt"), "latest").unwrap();
    }

    const T1: &str = "01790000000000000000-0000000001-00000000000000000000";
    const T2: &str = "01790000100000000000-0000000001-00000000000000000000";
    const T3: &str = "01790000200000000000-0000000001-00000000000000000000";
    const T4: &str = "01790000300000000000-0000000001-00000000000000000000";

    #[test]
    fn timestamps_render_as_utc() {
        assert_eq!(
            stem_timestamp("01790251058875046000-0000070648-00000000000000000000").as_deref(),
            Some("2026-09-24T11:57:38Z")
        );
        assert_eq!(stem_timestamp("0").as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(stem_timestamp("not-a-stamp"), None);
    }

    #[test]
    fn groups_by_commit_and_skips_incompatible_runs() {
        let dir = Dir::new("group");
        let suite = dir.0.join("pkg/suite");
        write(&suite, &run("suite", T1, Some("aaaaaaaaaaaaaaaa"), 10));
        let mut foreign = run("suite", T2, Some("bbbbbbbbbbbbbbbb"), 10);
        foreign.environment.cpu_model = "a different cpu".to_string();
        write(&suite, &foreign);
        write(&suite, &run("suite", T3, Some("aaaaaaaaaaaaaaaa"), 11));
        write(&suite, &run("suite", T4, Some("cccccccccccccccc"), 12));
        std::fs::create_dir_all(dir.0.join("baselines")).unwrap();
        write(&dir.0.join("baselines"), &run("suite", T1, None, 10));

        let histories = load_history(&dir.0, &HistoryOptions::new()).unwrap();
        assert_eq!(histories.len(), 1, "baselines never scanned");
        let history = &histories[0];
        assert_eq!(history.suite, "suite");
        assert_eq!(history.runs, 3);
        assert_eq!(history.reference_run.as_deref(), Some(T4));
        assert_eq!(history.skipped.len(), 1);
        assert!(history.skipped[0].path.ends_with(format!("{T2}.json")));
        assert!(history.skipped[0].reason.contains("CPU model"));
        assert_eq!(history.benchmarks.len(), 2);
        let queue = history
            .benchmarks
            .iter()
            .find(|bench| bench.benchmark_id.contains("queue::push"))
            .unwrap();
        let commits = queue
            .commits
            .iter()
            .map(|group| (group.git_commit.clone().unwrap(), group.points.len()))
            .collect::<Vec<_>>();
        assert_eq!(
            commits,
            vec![
                ("aaaaaaaaaaaaaaaa".to_string(), 2),
                ("cccccccccccccccc".to_string(), 1)
            ]
        );
        let point = &queue.commits[0].points[0];
        assert!(point.value.is_some() && point.ci_lower.is_some() && point.ci_upper.is_some());
        assert_eq!(point.quality, "authoritative");

        let text = render_text(&histories);
        assert!(
            text.contains("aaaaaaaaaaaa") && text.contains("skipped runs:"),
            "{text}"
        );
        let markdown = render_markdown(&histories);
        assert!(markdown.contains("| timestamp | sha | value | ci95 | quality |"));
        let csv = render_csv(&histories);
        assert_eq!(csv.matches("\r\n").count(), 1 + 3 * 2, "{csv}");
        let json: serde_json::Value = serde_json::from_str(&render_json(&histories)).unwrap();
        assert_eq!(json[0]["runs"], 3);
    }

    #[test]
    fn filters_by_suite_bench_and_last() {
        let dir = Dir::new("filter");
        for (index, stamp) in [T1, T2, T3].into_iter().enumerate() {
            write(
                &dir.0.join("alpha"),
                &run("alpha", stamp, Some("a1"), 10 + index as u64),
            );
        }
        write(&dir.0.join("beta"), &run("beta", T1, None, 10));

        let options = HistoryOptions::new().suite("alpha").bench("queue").last(2);
        let histories = load_history(&dir.0, &options).unwrap();
        assert_eq!(histories.len(), 1);
        assert_eq!(histories[0].runs, 2);
        assert_eq!(histories[0].benchmarks.len(), 1);
        let points = point_rows(&histories[0].benchmarks[0])
            .map(|point| point.started_at.as_str())
            .collect::<Vec<_>>();
        assert_eq!(points, vec![T2, T3]);

        // A suite directory passed directly is used as-is.
        let direct = load_history(&dir.0.join("beta"), &HistoryOptions::new()).unwrap();
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].suite, "beta");
    }

    #[test]
    fn unreadable_and_mislabeled_artifacts_are_skipped() {
        let dir = Dir::new("unreadable");
        let suite = dir.0.join("suite");
        write(&suite, &run("suite", T2, None, 10));
        std::fs::write(suite.join(format!("{T1}.json")), "{not json").unwrap();
        let mislabeled = run("suite", T2, None, 10);
        std::fs::write(
            suite.join(format!("{T3}.json")),
            serde_json::to_string(&mislabeled).unwrap(),
        )
        .unwrap();
        let histories = load_history(&dir.0, &HistoryOptions::new()).unwrap();
        assert_eq!(histories[0].runs, 1);
        assert_eq!(histories[0].skipped.len(), 2);
    }

    #[test]
    fn prune_is_dry_run_by_plan_and_keeps_latest_and_foreign_files() {
        let dir = Dir::new("prune");
        let suite = dir.0.join("suite");
        for stamp in [T1, T2, T3, T4] {
            write(&suite, &run("suite", stamp, None, 10));
        }
        // Files that must never be pruned.
        std::fs::write(suite.join("notes.json"), "{}").unwrap();
        std::fs::write(suite.join("README.md"), "keep").unwrap();
        std::fs::write(suite.join(format!("{T1}.json.bak")), "keep").unwrap();
        std::fs::write(suite.join(".artifact-publication.lock"), "").unwrap();

        let plan = plan_prune(&suite, 2).unwrap();
        assert_eq!(plan.kept, vec![T3.to_string(), T4.to_string()]);
        assert_eq!(plan.delete.len(), 8, "{:?}", plan.delete);
        assert!(
            suite.join(format!("{T1}.json")).exists(),
            "planning deletes nothing"
        );

        let applied = apply_prune(&suite, 2).unwrap();
        assert_eq!(applied, plan);
        for stamp in [T1, T2] {
            for extension in ["json", "txt", "md", "csv"] {
                assert!(!suite.join(format!("{stamp}.{extension}")).exists());
            }
        }
        for stamp in [T3, T4] {
            assert!(suite.join(format!("{stamp}.json")).exists());
        }
        for keep in [
            "latest.json",
            "latest.txt",
            "notes.json",
            "README.md",
            ".artifact-publication.lock",
        ] {
            assert!(suite.join(keep).exists(), "{keep}");
        }
        assert!(suite.join(format!("{T1}.json.bak")).exists());

        assert_eq!(plan_prune(&suite, 0).unwrap().delete.len(), 8);
        assert!(apply_prune(&suite, 5).unwrap().delete.is_empty());
    }

    #[test]
    fn prune_refuses_while_a_publication_awaits_recovery() {
        let dir = Dir::new("prune-recovery");
        let suite = dir.0.join("suite");
        for stamp in [T1, T2] {
            write(&suite, &run("suite", stamp, None, 10));
        }
        std::fs::create_dir(suite.join(".artifact-transaction.x")).unwrap();
        assert!(apply_prune(&suite, 1).is_err());
        assert!(suite.join(format!("{T1}.json")).exists());
    }
}
