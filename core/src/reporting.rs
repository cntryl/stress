//! Pluggable reporters for current stress artifacts.

use crate::artifact::{
    BenchmarkDiagnostic, BenchmarkSpec, BenchmarkSummary, ComparisonClass, ComparisonResult,
    ConsoleNameMode, CorrectnessSummary, DiagnosticSeverity, PrimaryMetric, QualityClass,
    RunProfile, SamplePhase, StressRun, SummaryStats, TrustClass,
};
use crate::config::StressRunnerConfig;
use crate::diagnostics::catalog_fix;
use crate::scaling::{sweep_groups, SweepGroupKey};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Trait for benchmark result reporters.
pub trait Reporter: Send + Sync {
    /// Called when a suite starts.
    fn suite_start(&self, _suite: &str, _config: &StressRunnerConfig) {}

    /// Called before a benchmark row starts running.
    fn bench_start(&self, _spec: &BenchmarkSpec) {}

    /// Called as samples are recorded.
    fn sample_progress(&self, _progress: &SampleProgress) {}

    /// Called when a benchmark summary is available.
    fn bench_end(&self, _summary: &BenchmarkSummary) {}

    /// Called when a suite completes.
    ///
    /// # Errors
    ///
    /// Returns an error when the reporter cannot publish its result.
    fn suite_end(&self, _run: &StressRun) -> std::io::Result<()> {
        Ok(())
    }
}

/// Progress update for one benchmark sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleProgress {
    /// Stable benchmark id.
    pub benchmark_id: String,
    /// Display name.
    pub name: String,
    /// Numeric tier.
    pub tier: u32,
    /// Sample phase.
    pub phase: SamplePhase,
    /// Completed samples for this phase.
    pub completed_samples: usize,
    /// Target samples for this phase.
    pub target_samples: usize,
}

const NAME_WIDTH: usize = 36;
const HUMAN_TABLE_NAME_LABEL_MAX_WIDTH: usize = 64;
const HUMAN_TABLE_NAME_DEFAULT_WIDTH: usize = 65;
const HUMAN_TABLE_COLUMN_MIN_WIDTH: usize = 12;
const VALUE_WIDTH: usize = 16;
const ARTIFACT_PUBLICATION_LOCK_FILE: &str = ".artifact-publication.lock";
const ARTIFACT_TRANSACTION_PREFIX: &str = ".artifact-transaction.";
const ARTIFACT_COMMITTED_TRANSACTION_PREFIX: &str = ".artifact-committed.";
const ARTIFACT_TRANSACTION_MANIFEST: &str = "manifest.json";
const ARTIFACT_TRANSACTION_COMMITTED: &str = "committed";
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Console reporter that prints the human benchmark table to stdout.
pub struct ConsoleReporter {
    output_lock: Mutex<()>,
}

impl ConsoleReporter {
    /// Create a console reporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            output_lock: Mutex::new(()),
        }
    }

    fn write_stdout(&self, message: &str) -> std::io::Result<()> {
        let _guard = self
            .output_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{message}")
    }
}

impl Default for ConsoleReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for ConsoleReporter {
    fn suite_end(&self, run: &StressRun) -> std::io::Result<()> {
        self.write_stdout(&format_console_run(run))
    }
}

pub(crate) struct JsonStdoutReporter {
    output_lock: Mutex<()>,
}

impl JsonStdoutReporter {
    pub(crate) fn new() -> Self {
        Self {
            output_lock: Mutex::new(()),
        }
    }

    fn write_stdout(&self, message: &str) -> std::io::Result<()> {
        let _guard = self
            .output_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{message}")
    }
}

impl Reporter for JsonStdoutReporter {
    fn suite_end(&self, run: &StressRun) -> std::io::Result<()> {
        let output = serde_json::to_string_pretty(run).map_err(std::io::Error::other)?;
        self.write_stdout(&output)
    }
}

/// Stderr-only progress reporter for long human runs.
pub struct StderrProgressReporter {
    output_lock: Mutex<()>,
}

impl StderrProgressReporter {
    /// Create a progress reporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            output_lock: Mutex::new(()),
        }
    }

    fn write_stderr(&self, message: &str) {
        let _guard = self
            .output_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{message}");
    }
}

impl Default for StderrProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for StderrProgressReporter {
    fn bench_start(&self, spec: &BenchmarkSpec) {
        self.write_stderr(&format!("stress: start {} (tier {})", spec.name, spec.tier));
    }

    fn sample_progress(&self, progress: &SampleProgress) {
        self.write_stderr(&format!(
            "stress: sample {} {} {}/{}",
            progress.name,
            phase_label(progress.phase),
            progress.completed_samples,
            progress.target_samples
        ));
    }

    fn bench_end(&self, summary: &BenchmarkSummary) {
        let value = summary
            .primary_value()
            .map_or_else(|| "n/a".to_string(), |value| format_metric(value, summary));
        self.write_stderr(&format!(
            "stress: finish {} value={} quality={}",
            summary.name, value, summary.quality
        ));
    }
}

const fn phase_label(phase: SamplePhase) -> &'static str {
    match phase {
        SamplePhase::Warmup => "warmup",
        SamplePhase::Measured => "measured",
        SamplePhase::Cooldown => "cooldown",
    }
}

/// JSON reporter that writes JSON, text, and Markdown reports.
pub struct JsonReporter {
    output_dir: PathBuf,
    announce: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactPublicationPoint {
    BeforeCommit,
    BeforeFinalize,
}

#[derive(Clone)]
struct PendingArtifact<'a> {
    path: PathBuf,
    contents: &'a [u8],
    replace_existing: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ArtifactTransactionManifest {
    targets: Vec<String>,
}

impl JsonReporter {
    /// Create a JSON reporter.
    #[must_use]
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
            announce: true,
        }
    }

    /// Set whether artifact paths are printed to stderr.
    #[must_use]
    pub const fn announce(mut self, value: bool) -> Self {
        self.announce = value;
        self
    }

    fn write_results_inner(&self, run: &StressRun) -> std::io::Result<()> {
        self.write_results_inner_with_hook(run, |_| Ok(()))
    }

    fn write_results_inner_with_hook(
        &self,
        run: &StressRun,
        mut before_publication_point: impl FnMut(ArtifactPublicationPoint) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let sanitized_name = artifact_suite_directory_name(&run.suite)?;
        let suite_dir = self.output_dir.join(sanitized_name);
        std::fs::create_dir_all(&suite_dir)?;

        let timestamp = &run.started_at;
        let json_path = suite_dir.join(format!("{timestamp}.json"));
        let txt_path = suite_dir.join(format!("{timestamp}.txt"));
        let md_path = suite_dir.join(format!("{timestamp}.md"));
        let csv_path = suite_dir.join(format!("{timestamp}.csv"));
        let latest_json_path = suite_dir.join("latest.json");
        let latest_txt_path = suite_dir.join("latest.txt");
        let latest_md_path = suite_dir.join("latest.md");
        let latest_csv_path = suite_dir.join("latest.csv");

        let json = serde_json::to_string_pretty(run).map_err(std::io::Error::other)?;
        let report = format_report(run);
        let markdown = format_markdown_report(run);
        let csv = crate::csv::format_csv_report(run);

        // Keep JSON last so readers never observe a new canonical artifact
        // before its corresponding human reports. If any later operation
        // fails, the transaction restores every replaced file and removes
        // every newly created timestamp artifact.
        let artifacts = [
            PendingArtifact {
                path: txt_path,
                contents: report.as_bytes(),
                replace_existing: false,
            },
            PendingArtifact {
                path: md_path,
                contents: markdown.as_bytes(),
                replace_existing: false,
            },
            PendingArtifact {
                path: latest_txt_path,
                contents: report.as_bytes(),
                replace_existing: true,
            },
            PendingArtifact {
                path: latest_md_path,
                contents: markdown.as_bytes(),
                replace_existing: true,
            },
            PendingArtifact {
                path: csv_path,
                contents: csv.as_bytes(),
                replace_existing: false,
            },
            PendingArtifact {
                path: latest_csv_path,
                contents: csv.as_bytes(),
                replace_existing: true,
            },
            PendingArtifact {
                path: json_path.clone(),
                contents: json.as_bytes(),
                replace_existing: false,
            },
            PendingArtifact {
                path: latest_json_path.clone(),
                contents: json.as_bytes(),
                replace_existing: true,
            },
        ];
        publish_artifact_set(
            &suite_dir,
            &run.started_at,
            &artifacts,
            &mut before_publication_point,
        )?;

        if self.announce {
            eprintln!("  Results written to: {}", json_path.display());
            eprintln!("  Latest results at: {}", latest_json_path.display());
        }
        Ok(())
    }
}

fn publish_artifact_set(
    suite_dir: &Path,
    started_at: &str,
    artifacts: &[PendingArtifact<'_>],
    before_publication_point: &mut impl FnMut(ArtifactPublicationPoint) -> std::io::Result<()>,
) -> std::io::Result<()> {
    validate_artifact_targets(suite_dir, artifacts)?;
    let _publication_lock = acquire_artifact_publication_lock(suite_dir)?;
    recover_interrupted_artifact_transactions(suite_dir)?;
    // A slower concurrent run may finish after a newer one; keep its
    // timestamped artifacts but never move latest.* backwards.
    let filtered;
    let artifacts = if latest_is_newer_than(suite_dir, started_at) {
        filtered = artifacts
            .iter()
            .filter(|artifact| !artifact.replace_existing)
            .cloned()
            .collect::<Vec<_>>();
        filtered.as_slice()
    } else {
        artifacts
    };
    reject_timestamp_artifact_collisions(artifacts)?;
    let transaction_dir = create_artifact_transaction_directory(suite_dir)?;
    if let Err(error) = stage_artifacts(&transaction_dir, artifacts)
        .and_then(|()| write_artifact_transaction_manifest(&transaction_dir, artifacts))
    {
        let _ = std::fs::remove_dir_all(&transaction_dir);
        return Err(error);
    }

    let publication = commit_artifacts(
        suite_dir,
        &transaction_dir,
        artifacts,
        before_publication_point,
    )
    .and_then(|()| mark_artifact_transaction_committed(&transaction_dir));
    if let Err(error) = publication {
        return match rollback_artifacts(suite_dir, &transaction_dir, artifacts) {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(std::io::Error::new(
                error.kind(),
                format!(
                    "artifact publication failed: {error}; rollback also failed: {rollback_error}; recovery data remains at {}",
                    transaction_dir.display()
                ),
            )),
        };
    }

    // Once every target and the committed marker are durable, transaction
    // cleanup is housekeeping rather than part of publication. Rename first
    // so a crash during recursive removal can never make a committed
    // generation look like an interrupted one that should be rolled back.
    let _ = finalize_committed_artifact_transaction(suite_dir, &transaction_dir);
    Ok(())
}

/// Whether an interrupted or uncleaned artifact transaction exists.
pub(crate) fn has_artifact_transaction_state(suite_dir: &Path) -> std::io::Result<bool> {
    Ok(std::fs::read_dir(suite_dir)?
        .filter_map(Result::ok)
        .any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(ARTIFACT_TRANSACTION_PREFIX)
                || name.starts_with(ARTIFACT_COMMITTED_TRANSACTION_PREFIX)
        }))
}

pub(crate) fn acquire_artifact_publication_lock(
    suite_dir: &Path,
) -> std::io::Result<std::fs::File> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(suite_dir.join(ARTIFACT_PUBLICATION_LOCK_FILE))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    Ok(lock)
}

fn write_artifact_transaction_manifest(
    transaction_dir: &Path,
    artifacts: &[PendingArtifact<'_>],
) -> std::io::Result<()> {
    let targets = artifacts
        .iter()
        .map(|artifact| {
            artifact
                .path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "artifact target {} has no UTF-8 file name",
                            artifact.path.display()
                        ),
                    )
                })
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    let manifest = serde_json::to_vec(&ArtifactTransactionManifest { targets })
        .map_err(std::io::Error::other)?;
    write_transaction_marker(
        transaction_dir,
        &format!("{ARTIFACT_TRANSACTION_MANIFEST}.pending"),
        ARTIFACT_TRANSACTION_MANIFEST,
        &manifest,
    )
}

fn mark_artifact_transaction_committed(transaction_dir: &Path) -> std::io::Result<()> {
    write_transaction_marker(
        transaction_dir,
        &format!("{ARTIFACT_TRANSACTION_COMMITTED}.pending"),
        ARTIFACT_TRANSACTION_COMMITTED,
        b"",
    )
}

fn finalize_committed_artifact_transaction(
    suite_dir: &Path,
    transaction_dir: &Path,
) -> std::io::Result<()> {
    let transaction_name = transaction_dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(|name| name.strip_prefix(ARTIFACT_TRANSACTION_PREFIX))
        .ok_or_else(|| {
            invalid_transaction_data(format!(
                "artifact transaction directory {} has an invalid name",
                transaction_dir.display()
            ))
        })?;
    let committed_dir = suite_dir.join(format!(
        "{ARTIFACT_COMMITTED_TRANSACTION_PREFIX}{transaction_name}"
    ));
    std::fs::rename(transaction_dir, &committed_dir)?;
    sync_parent_directory(suite_dir)?;
    std::fs::remove_dir_all(committed_dir)?;
    sync_parent_directory(suite_dir)
}

fn write_transaction_marker(
    transaction_dir: &Path,
    temporary_name: &str,
    final_name: &str,
    contents: &[u8],
) -> std::io::Result<()> {
    let temporary = transaction_dir.join(temporary_name);
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;
    std::fs::rename(temporary, transaction_dir.join(final_name))?;
    sync_parent_directory(transaction_dir)
}

fn recover_interrupted_artifact_transactions(suite_dir: &Path) -> std::io::Result<()> {
    let mut transaction_dirs = std::fs::read_dir(suite_dir)?
        .filter_map(|entry| match entry {
            Ok(entry) => {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(ARTIFACT_TRANSACTION_PREFIX) {
                    Some(Ok((entry.path(), false)))
                } else if name.starts_with(ARTIFACT_COMMITTED_TRANSACTION_PREFIX) {
                    Some(Ok((entry.path(), true)))
                } else {
                    None
                }
            }
            Err(error) => Some(Err(error)),
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    transaction_dirs.sort();

    for (transaction_dir, committed) in transaction_dirs {
        if !std::fs::symlink_metadata(&transaction_dir)?.is_dir() {
            return Err(invalid_transaction_data(format!(
                "artifact recovery entry {} is not a directory",
                transaction_dir.display()
            )));
        }
        if committed {
            std::fs::remove_dir_all(&transaction_dir)?;
            sync_parent_directory(suite_dir)?;
            continue;
        }
        if transaction_regular_file_exists(&transaction_dir.join(ARTIFACT_TRANSACTION_COMMITTED))? {
            std::fs::remove_dir_all(&transaction_dir)?;
            sync_parent_directory(suite_dir)?;
            continue;
        }

        let manifest_path = transaction_dir.join(ARTIFACT_TRANSACTION_MANIFEST);
        let Some(manifest) = read_transaction_manifest(&manifest_path)? else {
            // Publication cannot begin until the manifest rename is durable,
            // so a manifest-free transaction is staging debris.
            std::fs::remove_dir_all(&transaction_dir)?;
            sync_parent_directory(suite_dir)?;
            continue;
        };
        let targets = recovered_artifact_targets(suite_dir, &manifest)?;
        rollback_artifact_targets(suite_dir, &transaction_dir, &targets)?;
    }
    Ok(())
}

fn transaction_regular_file_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(invalid_transaction_data(format!(
            "artifact transaction marker {} is not a regular file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_transaction_manifest(path: &Path) -> std::io::Result<Option<ArtifactTransactionManifest>> {
    if !transaction_regular_file_exists(path)? {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        invalid_transaction_data(format!(
            "artifact transaction manifest {} is invalid: {error}",
            path.display()
        ))
    })
}

fn recovered_artifact_targets(
    suite_dir: &Path,
    manifest: &ArtifactTransactionManifest,
) -> std::io::Result<Vec<PathBuf>> {
    let names = manifest.targets.iter().collect::<BTreeSet<_>>();
    if names.len() != manifest.targets.len() {
        return Err(invalid_transaction_data(
            "artifact transaction manifest targets must be unique",
        ));
    }
    for name in &manifest.targets {
        if name.is_empty()
            || matches!(name.as_str(), "." | "..")
            || name.contains('/')
            || name.contains('\\')
        {
            return Err(invalid_transaction_data(format!(
                "artifact transaction target {name:?} is not a direct file name"
            )));
        }
    }

    let history_json = manifest
        .targets
        .iter()
        .filter(|name| name.as_str() != "latest.json")
        .filter_map(|name| name.strip_suffix(".json"))
        .collect::<Vec<_>>();
    if history_json.len() != 1 || history_json[0].is_empty() || history_json[0] == "latest" {
        return Err(invalid_transaction_data(
            "artifact transaction manifest has no unique timestamp JSON target",
        ));
    }
    let stem = history_json[0];
    // A generation is the timestamped history set, optionally with the
    // matching latest.* set (omitted when a newer run already owns latest).
    // Generations written before CSV output have no `.csv` pair.
    let actual = manifest.targets.iter().cloned().collect::<BTreeSet<_>>();
    let generation = |extensions: &[&str], with_latest: bool| {
        extensions
            .iter()
            .flat_map(|extension| {
                let mut names = vec![format!("{stem}.{extension}")];
                if with_latest {
                    names.push(format!("latest.{extension}"));
                }
                names
            })
            .collect::<BTreeSet<_>>()
    };
    let complete = [
        &["txt", "md", "csv", "json"][..],
        &["txt", "md", "json"][..],
    ]
    .into_iter()
    .any(|extensions| {
        actual == generation(extensions, true) || actual == generation(extensions, false)
    });
    if !complete {
        return Err(invalid_transaction_data(
            "artifact transaction manifest targets do not form one complete generation",
        ));
    }
    Ok(manifest
        .targets
        .iter()
        .map(|name| suite_dir.join(name))
        .collect())
}

fn invalid_transaction_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn reject_timestamp_artifact_collisions(artifacts: &[PendingArtifact<'_>]) -> std::io::Result<()> {
    for artifact in artifacts
        .iter()
        .filter(|artifact| !artifact.replace_existing)
    {
        match std::fs::symlink_metadata(&artifact.path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "timestamp artifact {} already exists; refusing to overwrite immutable run history",
                        artifact.path.display()
                    ),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn validate_artifact_targets(
    suite_dir: &Path,
    artifacts: &[PendingArtifact<'_>],
) -> std::io::Result<()> {
    let mut unique = BTreeSet::new();
    for artifact in artifacts {
        if artifact.path.parent() != Some(suite_dir) || !unique.insert(artifact.path.clone()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "artifact target {} is not a unique direct child of {}",
                    artifact.path.display(),
                    suite_dir.display()
                ),
            ));
        }
    }
    Ok(())
}

fn create_artifact_transaction_directory(suite_dir: &Path) -> std::io::Result<PathBuf> {
    loop {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let transaction_dir = suite_dir.join(format!(
            "{ARTIFACT_TRANSACTION_PREFIX}{}.{}",
            std::process::id(),
            sequence
        ));
        match std::fs::create_dir(&transaction_dir) {
            Ok(()) => {
                if let Err(error) = sync_parent_directory(suite_dir) {
                    let _ = std::fs::remove_dir(&transaction_dir);
                    return Err(error);
                }
                return Ok(transaction_dir);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
}

fn stage_artifacts(
    transaction_dir: &Path,
    artifacts: &[PendingArtifact<'_>],
) -> std::io::Result<()> {
    for (index, artifact) in artifacts.iter().enumerate() {
        let path = staged_artifact_path(transaction_dir, index);
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)?;
        file.write_all(artifact.contents)?;
        file.sync_all()?;
    }
    sync_parent_directory(transaction_dir)
}

fn commit_artifacts(
    suite_dir: &Path,
    transaction_dir: &Path,
    artifacts: &[PendingArtifact<'_>],
    before_publication_point: &mut impl FnMut(ArtifactPublicationPoint) -> std::io::Result<()>,
) -> std::io::Result<()> {
    for (index, artifact) in artifacts.iter().enumerate() {
        before_publication_point(ArtifactPublicationPoint::BeforeCommit)?;
        preserve_previous_artifact(suite_dir, transaction_dir, index, &artifact.path)?;
        std::fs::rename(staged_artifact_path(transaction_dir, index), &artifact.path)?;
        sync_parent_directory(transaction_dir)?;
        sync_parent_directory(suite_dir)?;
    }
    before_publication_point(ArtifactPublicationPoint::BeforeFinalize)?;
    Ok(())
}

fn preserve_previous_artifact(
    suite_dir: &Path,
    transaction_dir: &Path,
    index: usize,
    target: &Path,
) -> std::io::Result<()> {
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            std::fs::rename(target, backup_artifact_path(transaction_dir, index))?;
            sync_parent_directory(suite_dir)?;
            sync_parent_directory(transaction_dir)
        }
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "artifact target {} exists but is not a replaceable file",
                target.display()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let marker = absent_artifact_path(transaction_dir, index);
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(marker)?
                .sync_all()?;
            sync_parent_directory(transaction_dir)
        }
        Err(error) => Err(error),
    }
}

fn rollback_artifacts(
    suite_dir: &Path,
    transaction_dir: &Path,
    artifacts: &[PendingArtifact<'_>],
) -> std::io::Result<()> {
    let targets = artifacts
        .iter()
        .map(|artifact| artifact.path.clone())
        .collect::<Vec<_>>();
    rollback_artifact_targets(suite_dir, transaction_dir, &targets)
}

fn rollback_artifact_targets(
    suite_dir: &Path,
    transaction_dir: &Path,
    targets: &[PathBuf],
) -> std::io::Result<()> {
    let mut errors = Vec::new();
    for (index, target) in targets.iter().enumerate().rev() {
        let backup = backup_artifact_path(transaction_dir, index);
        let absent = absent_artifact_path(transaction_dir, index);
        let has_backup = match transaction_entry_exists(&backup) {
            Ok(exists) => exists,
            Err(error) => {
                errors.push(format!(
                    "could not inspect rollback backup {}: {error}",
                    backup.display()
                ));
                continue;
            }
        };
        let was_absent = match transaction_entry_exists(&absent) {
            Ok(exists) => exists,
            Err(error) => {
                errors.push(format!(
                    "could not inspect rollback marker {}: {error}",
                    absent.display()
                ));
                continue;
            }
        };
        if has_backup {
            if let Err(error) =
                remove_published_artifact(target).and_then(|()| std::fs::rename(&backup, target))
            {
                errors.push(format!("could not restore {}: {error}", target.display()));
            }
        } else if was_absent {
            if let Err(error) = remove_published_artifact(target) {
                errors.push(format!("could not remove {}: {error}", target.display()));
            }
        }
    }

    if errors.is_empty() {
        sync_parent_directory(suite_dir)?;
        std::fs::remove_dir_all(transaction_dir)?;
        sync_parent_directory(suite_dir)
    } else {
        Err(std::io::Error::other(errors.join("; ")))
    }
}

fn transaction_entry_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            Ok(true)
        }
        Ok(_) => Err(std::io::Error::other(format!(
            "transaction entry {} is not a file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_published_artifact(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            std::fs::remove_file(path)
        }
        Ok(_) => Err(std::io::Error::other(format!(
            "rollback target {} is not a replaceable file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether `latest.json` in `suite_dir` records a run that started after
/// `started_at`. Run stems are fixed-width, so shorter stems order first.
fn latest_is_newer_than(suite_dir: &Path, started_at: &str) -> bool {
    let Ok(bytes) = std::fs::read(suite_dir.join("latest.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    value
        .get("started_at")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|existing| (existing.len(), existing) > (started_at.len(), started_at))
}

fn staged_artifact_path(transaction_dir: &Path, index: usize) -> PathBuf {
    transaction_dir.join(format!("staged-{index}"))
}

fn backup_artifact_path(transaction_dir: &Path, index: usize) -> PathBuf {
    transaction_dir.join(format!("backup-{index}"))
}

fn absent_artifact_path(transaction_dir: &Path, index: usize) -> PathBuf {
    transaction_dir.join(format!("absent-{index}"))
}

/// Validate a suite name for use as an artifact directory.
///
/// Applies the same rule as `StressRunner`: non-empty, not a dot segment, and
/// only ASCII letters, digits, `.`, `-`, or `_`.
fn artifact_suite_directory_name(suite: &str) -> std::io::Result<String> {
    let valid = !suite.trim().is_empty()
        && !matches!(suite, "." | "..")
        && suite.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        });
    if valid {
        Ok(suite.to_string())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "invalid stress suite name {suite:?}: must be non-empty, not '.' or '..', and contain only ASCII letters, digits, '.', '-', or '_'"
            ),
        ))
    }
}

/// Escape text for a single Markdown table cell.
fn escape_markdown_cell(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '|' => escaped.push_str("\\|"),
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                escaped.push(' ');
            }
            '\n' => escaped.push(' '),
            other => escaped.push(other),
        }
    }
    escaped
}

pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("artifact path has no parent directory"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("artifact path has no file name"))?
        .to_string_lossy();
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));

    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        sync_parent_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = parent;
    Ok(())
}

impl Reporter for JsonReporter {
    fn suite_end(&self, run: &StressRun) -> std::io::Result<()> {
        self.write_results_inner(run)
    }
}

/// GitHub Actions reporter.
///
/// Emits workflow-command annotations (`::error`, `::warning`, `::notice`) on
/// stderr, never stdout, so `--json` output stays machine readable. When a step
/// summary path is configured (by default from `GITHUB_STEP_SUMMARY`), the
/// markdown report is appended to it.
pub struct GitHubActionsReporter {
    step_summary: Option<PathBuf>,
    sink: AnnotationSink,
}

enum AnnotationSink {
    Stderr,
    #[cfg(test)]
    Buffer(std::sync::Arc<Mutex<Vec<u8>>>),
}

impl GitHubActionsReporter {
    /// Create a reporter that appends to `$GITHUB_STEP_SUMMARY` when set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            step_summary: std::env::var_os("GITHUB_STEP_SUMMARY")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            sink: AnnotationSink::Stderr,
        }
    }

    /// Override the step summary file; `None` disables the step summary.
    #[must_use]
    pub fn with_step_summary_path(mut self, path: Option<PathBuf>) -> Self {
        self.step_summary = path;
        self
    }

    #[cfg(test)]
    fn with_annotation_buffer(mut self, buffer: std::sync::Arc<Mutex<Vec<u8>>>) -> Self {
        self.sink = AnnotationSink::Buffer(buffer);
        self
    }

    fn write_annotations(&self, text: &str) -> std::io::Result<()> {
        match &self.sink {
            AnnotationSink::Stderr => {
                let mut stderr = std::io::stderr().lock();
                stderr.write_all(text.as_bytes())?;
                stderr.flush()
            }
            #[cfg(test)]
            AnnotationSink::Buffer(buffer) => {
                buffer
                    .lock()
                    .map_err(|_| std::io::Error::other("annotation buffer poisoned"))?
                    .extend_from_slice(text.as_bytes());
                Ok(())
            }
        }
    }

    fn append_step_summary(path: &Path, run: &StressRun) -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let mut markdown = format_markdown_report(run);
        if !markdown.ends_with('\n') {
            markdown.push('\n');
        }
        file.write_all(markdown.as_bytes())?;
        file.flush()
    }
}

impl Default for GitHubActionsReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for GitHubActionsReporter {
    fn suite_end(&self, run: &StressRun) -> std::io::Result<()> {
        self.write_annotations(&format_github_annotations(run))?;
        if let Some(path) = &self.step_summary {
            // The step summary is presentation only; failing to append it
            // must not fail the benchmark gate.
            if let Err(error) = Self::append_step_summary(path, run) {
                self.write_annotations(&format!(
                    "::warning title={}::{}\n",
                    escape_workflow_property("stress: step summary"),
                    escape_workflow_data(&format!(
                        "could not append to {}: {error}",
                        path.display()
                    ))
                ))?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnotationLevel {
    Error,
    Warning,
    Notice,
}

impl AnnotationLevel {
    const fn command(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Notice => "notice",
        }
    }
}

fn format_github_annotations(run: &StressRun) -> String {
    let denied = run
        .diagnostic_gate_failures()
        .into_iter()
        .map(|diagnostic| (diagnostic.benchmark_id.as_str(), diagnostic.code.as_str()))
        .collect::<BTreeSet<_>>();
    let quality_failures = quality_gate_failures(run)
        .into_iter()
        .map(|summary| summary.benchmark_id.as_str())
        .collect::<BTreeSet<_>>();
    let comparisons = comparison_by_benchmark(run);
    let gating_regressions = if run.environment.profile_config.fail_on_regression {
        run.regressions()
            .into_iter()
            .map(|comparison| comparison.benchmark_id.as_str())
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let mut output = String::new();
    for summary in &run.summaries {
        if !summary.correctness.passed {
            push_annotation(
                &mut output,
                AnnotationLevel::Error,
                summary,
                "correctness failed",
                &correctness_note(&summary.correctness),
            );
        }
        if summary.budget_results.iter().any(|result| !result.passed) {
            push_annotation(
                &mut output,
                AnnotationLevel::Error,
                summary,
                "budget failed",
                &budget_note(summary),
            );
        }
        if quality_failures.contains(summary.benchmark_id.as_str()) {
            push_annotation(
                &mut output,
                AnnotationLevel::Error,
                summary,
                "quality gate failed",
                &if summary.is_gate() {
                    format!(
                        "quality={} below min={}",
                        summary.quality, run.environment.profile_config.min_quality
                    )
                } else {
                    format!("trust={} is not gate-eligible", summary.trust_class)
                },
            );
        }
        if let Some(comparison) = comparisons
            .get(summary.benchmark_id.as_str())
            .filter(|comparison| comparison.classification == ComparisonClass::Regression)
        {
            let level = if gating_regressions.contains(summary.benchmark_id.as_str()) {
                AnnotationLevel::Error
            } else {
                AnnotationLevel::Warning
            };
            push_annotation(
                &mut output,
                level,
                summary,
                "regression",
                &format!(
                    "regressed against baseline ({})",
                    format_delta_cell(comparison)
                ),
            );
        }
        for diagnostic in &summary.diagnostics {
            let level =
                if denied.contains(&(summary.benchmark_id.as_str(), diagnostic.code.as_str())) {
                    AnnotationLevel::Error
                } else {
                    match diagnostic.severity {
                        DiagnosticSeverity::Error => AnnotationLevel::Error,
                        DiagnosticSeverity::Warning => AnnotationLevel::Warning,
                        _ => AnnotationLevel::Notice,
                    }
                };
            push_annotation(
                &mut output,
                level,
                summary,
                &diagnostic.code,
                &diagnostic.reason,
            );
        }
    }
    push_confirmation_annotation(&mut output, run);
    push_run_gate_annotation(&mut output, run);
    output
}

/// State the confirm-on-regression outcome when confirmation ran.
fn push_confirmation_annotation(output: &mut String, run: &StressRun) {
    if let Some(outcome) = noise_gating_lines(run)
        .into_iter()
        .rfind(|line| line.starts_with("confirmation: "))
    {
        let _ = writeln!(
            output,
            "::notice title={}::{}",
            escape_workflow_property("stress: confirmation"),
            escape_workflow_data(&format!("{}: {outcome}", run.suite))
        );
    }
}

/// Some gate failures (missing baselines, regression budgets, invalid rows,
/// empty runs, artifact errors) have no single row annotation, so always
/// state the run verdict.
fn push_run_gate_annotation(output: &mut String, run: &StressRun) {
    let gate = crate::runner::evaluate_run_gate(run);
    if gate != crate::runner::RunGate::Passed {
        let _ = writeln!(
            output,
            "::error title={}::{}",
            escape_workflow_property("stress: gate failed"),
            escape_workflow_data(&format!("{}: {gate:?}", run.suite))
        );
    }
}

fn push_annotation(
    output: &mut String,
    level: AnnotationLevel,
    summary: &BenchmarkSummary,
    title: &str,
    detail: &str,
) {
    let mut properties = Vec::new();
    if let Some(source) = &summary.source {
        properties.push(format!("file={}", escape_workflow_property(&source.file)));
        properties.push(format!("line={}", source.line));
    }
    properties.push(format!(
        "title={}",
        escape_workflow_property(&format!("stress: {title}"))
    ));
    let message = if detail.is_empty() {
        summary.name.clone()
    } else {
        format!("{}: {detail}", summary.name)
    };
    let _ = writeln!(
        output,
        "::{} {}::{}",
        level.command(),
        properties.join(","),
        escape_workflow_data(&message)
    );
}

/// Escape a workflow-command message (`%`, `\r`, `\n`).
fn escape_workflow_data(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// Escape a workflow-command property value (data rules plus `:` and `,`).
fn escape_workflow_property(value: &str) -> String {
    escape_workflow_data(value)
        .replace(':', "%3A")
        .replace(',', "%2C")
}

/// Combines multiple reporters.
pub struct MultiReporter {
    reporters: Vec<Box<dyn Reporter>>,
}

impl MultiReporter {
    /// Create a multi-reporter.
    #[must_use]
    pub fn new(reporters: Vec<Box<dyn Reporter>>) -> Self {
        Self { reporters }
    }
}

impl Reporter for MultiReporter {
    fn suite_start(&self, suite: &str, config: &StressRunnerConfig) {
        for reporter in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reporter.suite_start(suite, config);
            }));
        }
    }

    fn bench_start(&self, spec: &BenchmarkSpec) {
        for reporter in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reporter.bench_start(spec);
            }));
        }
    }

    fn sample_progress(&self, progress: &SampleProgress) {
        for reporter in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reporter.sample_progress(progress);
            }));
        }
    }

    fn bench_end(&self, summary: &BenchmarkSummary) {
        for reporter in &self.reporters {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                reporter.bench_end(summary);
            }));
        }
    }

    fn suite_end(&self, run: &StressRun) -> std::io::Result<()> {
        for reporter in &self.reporters {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reporter.suite_end(run)))
                    .map_err(|_| {
                        std::io::Error::other("reporter panicked while finishing the suite")
                    })?;
            result?;
        }
        Ok(())
    }
}

pub(crate) fn format_report(run: &StressRun) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "Benchmark Suite: {}", run.suite);
    let _ = writeln!(output, "Schema: {}", run.schema_version);
    let _ = writeln!(output, "Profile: {}", run.run_profile);
    let _ = writeln!(output, "Completed: {}", run.started_at);
    let _ = writeln!(
        output,
        "Samples: measured={} warmup={} cooldown={}",
        run.environment.profile_config.measured_samples,
        run.environment.profile_config.warmup_samples,
        run.environment.profile_config.cooldown_samples
    );
    let _ = writeln!(
        output,
        "Total time: {}",
        format_duration_ns(run.total_elapsed_ns)
    );
    output.push('\n');

    output.push_str("Summary\n");
    output.push_str("-------\n");
    for summary in &run.summaries {
        write_summary_line(&mut output, summary);
    }

    write_comparison_section(&mut output, "Regressions", run, ComparisonClass::Regression);
    write_comparison_section(
        &mut output,
        "Improvements",
        run,
        ComparisonClass::Improvement,
    );
    write_quality_section(&mut output, run);
    write_sweep_tables(&mut output, run);

    output
}

fn write_markdown_attention(output: &mut String, run: &StressRun) {
    let attention = attention_items(run);
    if attention.is_empty() {
        let _ = writeln!(output, "- none");
        return;
    }
    for (benchmark_id, item) in attention {
        let source = run
            .summaries
            .iter()
            .find(|summary| summary.benchmark_id == benchmark_id)
            .and_then(|summary| summary.source.as_ref());
        match source {
            Some(source) => {
                let _ = writeln!(output, "- {item} (at `{source}`)");
            }
            None => {
                let _ = writeln!(output, "- {item}");
            }
        }
    }
}

fn write_markdown_environment(output: &mut String, run: &StressRun) {
    if !run.environment.observations.is_empty() {
        let _ = writeln!(output, "## Environment");
        let _ = writeln!(output);
        for observation in &run.environment.observations {
            let value = escape_markdown_cell(&observation.value);
            if observation.adverse {
                let detail = if observation.detail.is_empty() {
                    String::new()
                } else {
                    format!(": {}", escape_markdown_cell(&observation.detail))
                };
                let _ = writeln!(output, "- `{}`: {value} (adverse{detail})", observation.key);
            } else {
                let _ = writeln!(output, "- `{}`: {value}", observation.key);
            }
        }
        let _ = writeln!(output);
    }
}

fn write_markdown_memory(output: &mut String, run: &StressRun) {
    if let Some((peak_mb, grower)) = peak_rss_overview(run) {
        let _ = writeln!(output, "## Memory");
        let _ = writeln!(output);
        let growth = grower.map_or_else(String::new, |id| {
            format!("; largest growth at `{}`", escape_markdown_cell(id))
        });
        let _ = writeln!(
            output,
            "- Peak RSS: {peak_mb:.1} MiB (process-wide{growth})"
        );
        let _ = writeln!(output);
    }
}

pub(crate) fn format_markdown_report(run: &StressRun) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "# {}", run.suite);
    let _ = writeln!(output);
    let _ = writeln!(output, "- Schema: `{}`", run.schema_version);
    let _ = writeln!(output, "- Profile: `{}`", run.run_profile);
    let _ = writeln!(output, "- Completed: `{}`", run.started_at);
    let _ = writeln!(
        output,
        "- Total time: `{}`",
        format_duration_ns(run.total_elapsed_ns)
    );
    let _ = writeln!(output);
    let _ = writeln!(output, "## Summary");
    let _ = writeln!(output);
    let _ = writeln!(output, "```text");
    output.push_str(&format_summary_blocks(run));
    let _ = writeln!(output, "```");
    let _ = writeln!(output);
    let noise_lines = noise_gating_lines(run);
    if !noise_lines.is_empty() {
        let _ = writeln!(output, "## Noise-aware gating");
        let _ = writeln!(output);
        for line in noise_lines {
            let _ = writeln!(output, "- {}", escape_markdown_cell(&line));
        }
        let _ = writeln!(output);
    }
    write_markdown_environment(&mut output, run);
    write_markdown_memory(&mut output, run);
    let _ = writeln!(output, "## Needs attention");
    let _ = writeln!(output);
    write_markdown_attention(&mut output, run);
    let _ = writeln!(output);
    let _ = writeln!(output, "## Benchmarks");
    let _ = writeln!(output);
    let _ = writeln!(
        output,
        "| Benchmark | Tier | Value | Trust | Mode | Question | Quality | Samples | Wall |"
    );
    let _ = writeln!(output, "|---|---:|---|---|---|---|---|---:|---:|");
    for summary in &run.summaries {
        let value = summary
            .primary_value()
            .map_or_else(|| "n/a".to_string(), |value| format_metric(value, summary));
        let _ = writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            escape_markdown_cell(&summary.name),
            summary.tier,
            value,
            summary.trust_class,
            measurement_mode_label(summary),
            escape_markdown_cell(&measurement_question(summary)),
            summary.quality,
            summary.measured_samples,
            format_duration_ns(summary.total_wall_clock_ns)
        );
    }
    if run
        .summaries
        .iter()
        .any(|summary| !summary.observations.is_empty())
    {
        let _ = writeln!(output);
        let _ = writeln!(output, "## Observations");
        let _ = writeln!(output);
        let _ = writeln!(
            output,
            "| Benchmark | Observation | Median | 95% CI | RSD | Direction |"
        );
        let _ = writeln!(output, "|---|---|---:|---:|---:|---|");
        for summary in &run.summaries {
            for observation in &summary.observations {
                let stats = &observation.stats;
                let rsd = if stats.relative_std_dev.is_finite() {
                    format!("{:.2}%", stats.relative_std_dev * 100.0)
                } else {
                    "n/a".to_string()
                };
                let _ = writeln!(
                    output,
                    "| {} | {} ({:?}) | {:.3} | {:.3}..{:.3} | {} | {:?} |",
                    escape_markdown_cell(&summary.name),
                    escape_markdown_cell(&observation.name),
                    observation.unit,
                    stats.median,
                    stats.confidence_interval_95.lower,
                    stats.confidence_interval_95.upper,
                    rsd,
                    observation.direction,
                );
            }
        }
    }
    write_markdown_tails(&mut output, run);
    output
}

/// One tail distribution row: benchmark name, series label, the
/// distribution, and whether its values are nanoseconds.
type TailRow<'a> = (
    &'a str,
    &'a str,
    &'a crate::artifact::DistributionSummary,
    bool,
);

/// Tail distributions of recorded latencies and observations, in row order.
fn tail_rows(run: &StressRun) -> Vec<TailRow<'_>> {
    let mut rows = Vec::new();
    for summary in &run.summaries {
        if let Some(distribution) = &summary.latency_distribution {
            rows.push((summary.name.as_str(), "latency", distribution, true));
        }
        for observation in &summary.observations {
            if let Some(distribution) = &observation.distribution {
                rows.push((
                    summary.name.as_str(),
                    observation.name.as_str(),
                    distribution,
                    false,
                ));
            }
        }
    }
    rows
}

fn format_tail_value(value: f64, nanoseconds: bool) -> String {
    if nanoseconds {
        format_duration_ns(f64_to_u128(value))
    } else {
        format!("{value:.3}")
    }
}

fn tail_console_lines(run: &StressRun) -> Vec<String> {
    tail_rows(run)
        .into_iter()
        .map(|(name, series, distribution, nanoseconds)| {
            let value = |value| format_tail_value(value, nanoseconds);
            let p999 = distribution
                .p999
                .map_or_else(String::new, |p999| format!(" p99.9={}", value(p999)));
            format!(
                "tail {name} {series} (n={}): p90={} p99={}{p999} max={}",
                distribution.count,
                value(distribution.p90),
                value(distribution.p99),
                value(distribution.max),
            )
        })
        .collect()
}

fn write_markdown_tails(output: &mut String, run: &StressRun) {
    let rows = tail_rows(run);
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(output);
    let _ = writeln!(output, "## Tail Distributions");
    let _ = writeln!(output);
    let _ = writeln!(
        output,
        "| Benchmark | Series | N | p90 | p99 | p99.9 | Max |"
    );
    let _ = writeln!(output, "|---|---|---:|---:|---:|---:|---:|");
    for (name, series, distribution, nanoseconds) in rows {
        let value = |value| format_tail_value(value, nanoseconds);
        let _ = writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} | {} |",
            escape_markdown_cell(name),
            escape_markdown_cell(series),
            distribution.count,
            value(distribution.p90),
            value(distribution.p99),
            distribution.p999.map_or_else(|| "-".to_string(), value),
            value(distribution.max),
        );
    }
}

/// Format one stress run for the human console.
#[must_use]
pub fn format_console_run(run: &StressRun) -> String {
    format_human_console_runs(std::slice::from_ref(run))
}

/// Format multiple stress runs as one consolidated human console report.
#[must_use]
pub fn format_console_runs(runs: &[StressRun]) -> String {
    format_human_console_runs(runs)
}

#[cfg(test)]
fn format_console_output(run: &StressRun) -> String {
    format_console_run(run)
}

fn format_human_console_runs(runs: &[StressRun]) -> String {
    let mut output = String::new();
    write_run_header(&mut output, runs);
    let mut wrote_suite = false;
    for run in runs {
        if wrote_suite || !output.is_empty() {
            let _ = writeln!(output);
        }
        write_suite_block(&mut output, run);
        wrote_suite = true;
    }
    write_final_result_line(&mut output, runs);
    output
}

fn write_run_header(output: &mut String, runs: &[StressRun]) {
    let Some(first) = runs.first() else {
        return;
    };
    let _ = writeln!(output, "@cntryl/stress v{}", first.tool_version);
}

/// Human lines describing captured environment observations. Empty when
/// nothing was observed.
pub(crate) fn environment_lines(run: &StressRun) -> Vec<String> {
    let observations = &run.environment.observations;
    if observations.is_empty() {
        return Vec::new();
    }
    let facts = observations
        .iter()
        .map(|observation| {
            let marker = if observation.adverse {
                " (adverse)"
            } else {
                ""
            };
            format!("{}={}{marker}", observation.key, observation.value)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = vec![format!("environment: {facts}")];
    lines.extend(
        run.environment
            .adverse_observations()
            .filter(|observation| !observation.detail.is_empty())
            .map(|observation| {
                format!(
                    "environment warning: {}: {}",
                    observation.key, observation.detail
                )
            }),
    );
    lines
}

/// Human lines describing baseline pooling and confirm-on-regression
/// attempts. Empty for runs that used neither.
pub(crate) fn noise_gating_lines(run: &StressRun) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(pooled) = run.metadata.get("baseline_runs_pooled") {
        let mut line = format!("baseline: pooled {pooled} saved runs");
        if let Some(skipped) = run.metadata.get("baseline_runs_skipped") {
            let _ = write!(line, " (skipped: {skipped})");
        }
        lines.push(line);
    }
    for attempt in &run.confirmation_runs {
        let mut line = format!(
            "confirmation attempt {}: re-ran {} benchmark(s), +{} samples; regressions {} -> {}",
            attempt.attempt,
            attempt.benchmark_ids.len(),
            attempt.samples_added,
            attempt.regressions_before.len(),
            attempt.regressions_after.len()
        );
        if let Some(error) = &attempt.error {
            let _ = write!(line, " (failed: {error})");
        }
        lines.push(line);
    }
    if let Some(reason) = run.metadata.get("confirmation_skipped") {
        lines.push(format!("confirmation: skipped ({reason})"));
    }
    if let Some(last) = run.confirmation_runs.last() {
        let attempts = run.confirmation_runs.len();
        if last.error.is_none() && last.regressions_after.is_empty() {
            lines.push(format!(
                "confirmation: regressions cleared after {attempts} attempt(s); samples pooled across attempts"
            ));
        } else {
            lines.push(format!(
                "confirmation: regressions persisted after {attempts} attempt(s): {}",
                last.regressions_after.join(", ")
            ));
        }
    }
    lines
}

/// Suite peak RSS in MiB and the row whose invocation grew it the most.
///
/// Peak RSS is process-wide and monotonic, so per-row values are cumulative
/// high-water marks. The first recorded row also carries the process baseline
/// (runtime, harness, earlier setup), so growth is only attributed to later
/// rows; `None` when no later row raised the mark.
fn peak_rss_overview(run: &StressRun) -> Option<(f64, Option<&str>)> {
    let mut values = run
        .summaries
        .iter()
        .filter_map(|summary| summary.peak_rss_bytes.map(|bytes| (bytes, summary)));
    let (mut previous, _) = values.next()?;
    let mut peak = previous;
    let mut largest_growth: Option<(u64, &str)> = None;
    for (bytes, summary) in values {
        let growth = bytes.saturating_sub(previous);
        if growth > 0 && largest_growth.is_none_or(|(largest, _)| growth > largest) {
            largest_growth = Some((growth, summary.benchmark_id.as_str()));
        }
        previous = previous.max(bytes);
        peak = peak.max(bytes);
    }
    #[allow(clippy::cast_precision_loss)]
    let peak_mb = peak as f64 / (1024.0 * 1024.0);
    Some((peak_mb, largest_growth.map(|(_, id)| id)))
}

fn write_suite_block(output: &mut String, run: &StressRun) {
    let _ = writeln!(output, "{}", run.suite);
    for line in environment_lines(run) {
        let _ = writeln!(output, "{line}");
    }
    if let Some((peak_mb, grower)) = peak_rss_overview(run) {
        let growth = grower.map_or_else(String::new, |id| format!("; largest growth at {id}"));
        let _ = writeln!(output, "peak RSS: {peak_mb:.1} MiB (process-wide{growth})");
    }
    for line in noise_gating_lines(run) {
        let _ = writeln!(output, "{line}");
    }

    let rows = rows_for_human_console(run);
    if rows.is_empty() {
        return;
    }

    let comparisons = comparison_by_benchmark(run);
    write_human_table(
        output,
        &rows,
        &comparisons,
        run.environment.profile_config.console_names,
    );
    for line in tail_console_lines(run) {
        let _ = writeln!(output, "{line}");
    }
}

fn rows_for_human_console(run: &StressRun) -> Vec<&BenchmarkSummary> {
    let comparisons = comparison_by_benchmark(run);
    let mut rows = run.summaries.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        summary_attention_rank(run, left, &comparisons)
            .unwrap_or(u8::MAX)
            .cmp(&summary_attention_rank(run, right, &comparisons).unwrap_or(u8::MAX))
            .then_with(|| left.name.cmp(&right.name))
    });
    rows
}

fn write_human_table(
    output: &mut String,
    summaries: &[&BenchmarkSummary],
    comparisons: &BTreeMap<&str, &ComparisonResult>,
    name_mode: ConsoleNameMode,
) {
    let name_width = human_table_name_width(summaries, name_mode);
    let rows = summaries
        .iter()
        .map(|summary| human_table_row(summary, name_mode, name_width))
        .collect::<Vec<_>>();
    let layout = HumanTableLayout::for_rows(&rows, name_width);
    write_human_table_header(output, &layout);
    for row in &rows {
        write_human_table_row(output, row, &layout);
    }
    write_issue_groups(output, &suite_issue_groups(summaries, comparisons));
}

fn human_table_name_width(summaries: &[&BenchmarkSummary], name_mode: ConsoleNameMode) -> usize {
    let max_name_width = summaries
        .iter()
        .map(|summary| match name_mode {
            ConsoleNameMode::Compact => summary.name.chars().count(),
            ConsoleNameMode::Full => name_with_parameter_hint(summary).chars().count(),
        })
        .max()
        .unwrap_or("benchmark".len())
        .max("benchmark".len());

    if matches!(name_mode, ConsoleNameMode::Compact) {
        max_name_width
            .min(HUMAN_TABLE_NAME_LABEL_MAX_WIDTH)
            .max(HUMAN_TABLE_NAME_DEFAULT_WIDTH)
    } else {
        max_name_width
    }
}

#[derive(Debug)]
struct HumanTableRow {
    name: String,
    measurement: String,
    value: String,
    p50: String,
    p95: String,
    p99: String,
    rsd: String,
    trust: String,
    mode: &'static str,
}

#[derive(Debug)]
struct HumanTableLayout {
    name: usize,
    measurement: usize,
    metric: usize,
    rsd: usize,
    trust: usize,
    mode: usize,
}

impl HumanTableLayout {
    fn for_rows(rows: &[HumanTableRow], name_width: usize) -> Self {
        let measurement = max_chars(
            "measurement",
            rows.iter().map(|row| row.measurement.as_str()),
        )
        .max(HUMAN_TABLE_COLUMN_MIN_WIDTH);
        let metric = rows
            .iter()
            .flat_map(|row| {
                [
                    row.value.as_str(),
                    row.p50.as_str(),
                    row.p95.as_str(),
                    row.p99.as_str(),
                ]
            })
            .map(str::chars)
            .map(Iterator::count)
            .chain(["value", "p50", "p95", "p99"].into_iter().map(str::len))
            .max()
            .unwrap_or("value".len())
            .max(HUMAN_TABLE_COLUMN_MIN_WIDTH);
        let rsd = max_chars("rsd", rows.iter().map(|row| row.rsd.as_str()))
            .max(HUMAN_TABLE_COLUMN_MIN_WIDTH);
        let trust = max_chars("trust", rows.iter().map(|row| row.trust.as_str()))
            .max(HUMAN_TABLE_COLUMN_MIN_WIDTH);
        let mode =
            max_chars("mode", rows.iter().map(|row| row.mode)).max(HUMAN_TABLE_COLUMN_MIN_WIDTH);

        Self {
            name: name_width,
            measurement,
            metric,
            rsd,
            trust,
            mode,
        }
    }
}

fn max_chars<'a>(header: &str, values: impl Iterator<Item = &'a str>) -> usize {
    values
        .map(str::chars)
        .map(Iterator::count)
        .chain(std::iter::once(header.chars().count()))
        .max()
        .unwrap_or_else(|| header.chars().count())
}

fn write_human_table_header(output: &mut String, layout: &HumanTableLayout) {
    let header = format!(
        "{benchmark:<name_width$} {measurement:<measurement_width$} {value:>metric_width$} {p50:>metric_width$} {p95:>metric_width$} {p99:>metric_width$} {rsd:>rsd_width$} {trust:<trust_width$} {mode:<mode_width$}",
        benchmark = "benchmark",
        measurement = "measurement",
        value = "value",
        p50 = "p50",
        p95 = "p95",
        p99 = "p99",
        rsd = "rsd",
        trust = "trust",
        mode = "mode",
        name_width = layout.name,
        measurement_width = layout.measurement,
        metric_width = layout.metric,
        rsd_width = layout.rsd,
        trust_width = layout.trust,
        mode_width = layout.mode,
    );
    let _ = writeln!(output, "{header}");
    let _ = writeln!(output, "{}", "-".repeat(header.len()));
}

fn human_table_row(
    summary: &BenchmarkSummary,
    name_mode: ConsoleNameMode,
    name_width: usize,
) -> HumanTableRow {
    let name = format_human_table_name(summary, name_mode, name_width);
    let value = summary.primary_value().map_or_else(
        || "n/a".to_string(),
        |value| format_human_metric_value(value, summary),
    );
    let measurement = human_measurement_label(summary);
    let stats = summary.stats.as_ref();
    let p50 = format_human_metric_stat(summary, stats, |stats| stats.p50);
    let p95 = format_human_metric_stat(summary, stats, |stats| stats.p95);
    let p99 = format_human_metric_stat(summary, stats, |stats| stats.p99);
    let rsd = stats.map_or_else(
        || "n/a".to_string(),
        |stats| format_percent(stats.relative_std_dev),
    );

    HumanTableRow {
        name,
        measurement,
        value,
        p50,
        p95,
        p99,
        rsd,
        trust: summary.trust_class.to_string(),
        mode: measurement_mode_label(summary),
    }
}

fn write_human_table_row(output: &mut String, row: &HumanTableRow, layout: &HumanTableLayout) {
    let _ = writeln!(
        output,
        "{name:<name_width$} {measurement:<measurement_width$} {value:>metric_width$} {p50:>metric_width$} {p95:>metric_width$} {p99:>metric_width$} {rsd:>rsd_width$} {trust:<trust_width$} {mode:<mode_width$}",
        name = row.name,
        measurement = row.measurement,
        value = row.value,
        p50 = row.p50,
        p95 = row.p95,
        p99 = row.p99,
        rsd = row.rsd,
        trust = row.trust,
        mode = row.mode,
        name_width = layout.name,
        measurement_width = layout.measurement,
        metric_width = layout.metric,
        rsd_width = layout.rsd,
        trust_width = layout.trust,
        mode_width = layout.mode,
    );
}

fn format_human_table_name(
    summary: &BenchmarkSummary,
    name_mode: ConsoleNameMode,
    width: usize,
) -> String {
    match name_mode {
        ConsoleNameMode::Compact => {
            truncate_name_to_width(&summary.name, width.min(HUMAN_TABLE_NAME_LABEL_MAX_WIDTH))
        }
        ConsoleNameMode::Full => name_with_parameter_hint(summary),
    }
}

fn name_with_parameter_hint(summary: &BenchmarkSummary) -> String {
    if summary.parameters.is_empty() {
        return summary.name.clone();
    }
    let hints = summary
        .parameters
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{} [{hints}]", summary.name)
}

fn write_final_result_line(output: &mut String, runs: &[StressRun]) {
    let Some(line) = final_result_line(runs) else {
        return;
    };
    let _ = writeln!(output);
    let _ = writeln!(output, "{line}");
}

fn final_result_line(runs: &[StressRun]) -> Option<String> {
    if runs.is_empty() {
        return None;
    }

    let failures = runs
        .iter()
        .filter_map(|run| {
            let status = gate_status(run);
            (status != "passed").then(|| format!("{}: {status}", run.suite))
        })
        .collect::<Vec<_>>();
    match failures.as_slice() {
        [] => Some("result: passed".to_string()),
        [failure] => Some(format!("result: failed ({failure})")),
        [first, ..] => Some(format!(
            "result: failed ({} suites failed; first {first})",
            failures.len()
        )),
    }
}

fn write_issue_groups(output: &mut String, groups: &[IssueGroup]) {
    if groups.is_empty() {
        return;
    }

    let _ = writeln!(output);
    let _ = writeln!(output, "issues");
    for (index, group) in groups.iter().enumerate() {
        if index > 0 {
            let _ = writeln!(output);
        }
        let _ = writeln!(output, "  {}", group.title);
        for item in &group.items {
            let _ = writeln!(output, "    • {}", item.text);
            if let Some(source) = &item.source {
                let _ = writeln!(output, "      at {source}");
            }
        }
        if let Some(fix) = &group.fix {
            let _ = writeln!(output, "    Fix: {fix}");
        }
    }
}

#[derive(Debug)]
struct IssueGroup {
    title: &'static str,
    items: Vec<IssueItem>,
    fix: Option<String>,
}

impl IssueGroup {
    const fn new(title: &'static str) -> Self {
        Self {
            title,
            items: Vec::new(),
            fix: None,
        }
    }

    fn with_fix(title: &'static str, fix: impl Into<String>) -> Self {
        Self {
            title,
            items: Vec::new(),
            fix: Some(fix.into()),
        }
    }

    fn push(&mut self, item: impl Into<String>) {
        self.items.push(IssueItem {
            text: item.into(),
            source: None,
        });
    }

    fn push_at(&mut self, summary: &BenchmarkSummary, item: impl Into<String>) {
        self.items.push(IssueItem {
            text: item.into(),
            source: summary.source.as_ref().map(ToString::to_string),
        });
    }
}

#[derive(Debug)]
struct IssueItem {
    text: String,
    source: Option<String>,
}

fn suite_issue_groups(
    summaries: &[&BenchmarkSummary],
    comparisons: &BTreeMap<&str, &ComparisonResult>,
) -> Vec<IssueGroup> {
    let mut groups = Vec::new();
    push_allocation_issue(&mut groups, summaries);
    push_variance_issues(&mut groups, summaries);
    push_sample_count_issue(&mut groups, summaries);
    push_quality_issue(&mut groups, summaries);
    push_shape_issues(&mut groups, summaries);
    push_validity_issues(&mut groups, summaries);
    push_comparison_issues(&mut groups, summaries, comparisons);
    groups
}

fn push_shape_issues(groups: &mut Vec<IssueGroup>, summaries: &[&BenchmarkSummary]) {
    push_diagnostic_group(
        groups,
        "Micro timing",
        summaries,
        "tiny_micro_timing",
        |summary| {
            format!(
                "{} is too small to trust as a gate-quality microbenchmark.",
                summary.name
            )
        },
    );
    push_diagnostic_group(
        groups,
        "Optimized away",
        summaries,
        "likely_optimized_away",
        |summary| {
            format!(
                "{} is likely optimized away or dominated by compiler artifacts.",
                summary.name
            )
        },
    );
    push_diagnostic_group(groups, "Too fast", summaries, "too_fast", |summary| {
        format!("{} is too small for stable timing.", summary.name)
    });
    push_diagnostic_group(
        groups,
        "Setup",
        summaries,
        "setup_dominates_measurement",
        |summary| format!("{} is dominated by setup or timing overhead.", summary.name),
    );
    push_diagnostic_group(
        groups,
        "Throughput shape",
        summaries,
        "single_op_throughput",
        |summary| {
            format!(
                "{} is a throughput-tier benchmark but records one operation per sample.",
                summary.name
            )
        },
    );
    push_diagnostic_group(
        groups,
        "Measurement semantics",
        summaries,
        "fixed_ops_throughput",
        |summary| {
            format!(
                "{} uses fixed-op timing for a throughput row.",
                summary.name
            )
        },
    );
    push_diagnostic_group(
        groups,
        "Batch unit",
        summaries,
        "batch_unit_ambiguous",
        |summary| {
            format!(
                "{} does not declare an explicit batch normalization basis.",
                summary.name
            )
        },
    );
    push_diagnostic_group(
        groups,
        "Measurement mode",
        summaries,
        "measurement_mode_mismatch",
        |summary| {
            format!(
                "{} mixes throughput measurement modes with sibling rows in the same family.",
                summary.name
            )
        },
    );
    push_diagnostic_group(groups, "Async", summaries, "async_misuse", |summary| {
        format!(
            "{} showed no observable async scheduling or await overhead.",
            summary.name
        )
    });
    push_diagnostic_group(
        groups,
        "Capped throughput",
        summaries,
        "flat_or_capped_throughput",
        |summary| {
            format!(
                "{} is suspiciously flat for a duration-based throughput row.",
                summary.name
            )
        },
    );
}

fn push_validity_issues(groups: &mut Vec<IssueGroup>, summaries: &[&BenchmarkSummary]) {
    let ordinary_summaries = summaries
        .iter()
        .copied()
        .filter(|summary| !summary.metadata.contains_key("benchmark_error"))
        .collect::<Vec<_>>();
    push_diagnostic_group(
        groups,
        "Operations",
        &ordinary_summaries,
        "zero_completed_ops",
        |summary| {
            format!(
                "{} completed zero logical operations in at least one sample.",
                summary.name
            )
        },
    );
    push_diagnostic_group(
        groups,
        "Timing",
        &ordinary_summaries,
        "invalid_timing",
        |summary| format!("{} recorded invalid timing.", summary.name),
    );
    push_diagnostic_group(
        groups,
        "Non-finite samples",
        summaries,
        "non_finite_samples_dropped",
        |summary| {
            format!(
                "{} dropped non-finite metric values from its statistics.",
                summary.name
            )
        },
    );
    let mut benchmark_errors = IssueGroup::with_fix(
        "Benchmark error",
        "Fix the reported benchmark setup or workload error, then rerun the suite.",
    );
    for summary in summaries
        .iter()
        .copied()
        .filter(|summary| summary.metadata.contains_key("benchmark_error"))
    {
        benchmark_errors.push_at(
            summary,
            format!(
                "{}: {}",
                summary.name,
                summary
                    .metadata
                    .get("benchmark_error")
                    .map_or("unknown benchmark error", String::as_str)
            ),
        );
    }
    push_issue_group(groups, benchmark_errors);

    let mut correctness = IssueGroup::with_fix("Correctness", catalog_fix("correctness_failure"));
    for summary in summaries.iter().copied().filter(|summary| {
        !summary.correctness.passed && !summary.metadata.contains_key("benchmark_error")
    }) {
        correctness.push_at(
            summary,
            format!("{} failed correctness checks.", summary.name),
        );
    }
    push_issue_group(groups, correctness);

    let mut budget = IssueGroup::new("Budget");
    for summary in summaries
        .iter()
        .copied()
        .filter(|summary| summary.budget_results.iter().any(|result| !result.passed))
    {
        if budget.fix.is_none() {
            budget.fix = Some(diagnostic_fix(summary, "budget_failure"));
        }
        budget.push_at(
            summary,
            format!(
                "{} failed budget checks: {}.",
                summary.name,
                budget_note(summary)
            ),
        );
    }
    push_issue_group(groups, budget);
}

fn push_comparison_issues(
    groups: &mut Vec<IssueGroup>,
    summaries: &[&BenchmarkSummary],
    comparisons: &BTreeMap<&str, &ComparisonResult>,
) {
    let mut regressions = IssueGroup::with_fix("Regression", catalog_fix("regression"));
    let mut improvements = IssueGroup::with_fix(
        "Improvement",
        "Update baselines only when the improvement is intentional.",
    );
    let mut semantic_changes = IssueGroup::with_fix(
        "Baseline semantics",
        catalog_fix("baseline_semantics_changed"),
    );
    for summary in summaries {
        if let Some(comparison) = comparisons.get(summary.benchmark_id.as_str()).copied() {
            match comparison.classification {
                ComparisonClass::Regression => regressions.push_at(
                    summary,
                    format!(
                        "{} regressed against baseline ({}).",
                        summary.name,
                        format_delta_cell(comparison)
                    ),
                ),
                ComparisonClass::Improvement if comparison_is_trustworthy(comparison) => {
                    improvements.push_at(
                        summary,
                        format!(
                            "{} improved against baseline ({}).",
                            summary.name,
                            format_delta_cell(comparison)
                        ),
                    );
                }
                ComparisonClass::Inconclusive if comparison.reason.is_some() => {
                    semantic_changes.push_at(
                        summary,
                        format!(
                            "{} changed comparison semantics: {}",
                            summary.name,
                            comparison.reason.as_deref().unwrap_or("unknown")
                        ),
                    );
                }
                ComparisonClass::Inconclusive
                | ComparisonClass::Improvement
                | ComparisonClass::MissingBaseline => {}
            }
        }
    }
    push_issue_group(groups, regressions);
    push_issue_group(groups, improvements);
    push_issue_group(groups, semantic_changes);
}

fn push_allocation_issue(groups: &mut Vec<IssueGroup>, summaries: &[&BenchmarkSummary]) {
    let names = summaries
        .iter()
        .copied()
        .filter(|summary| has_diagnostic(summary, "high_allocations"))
        .collect::<Vec<_>>();
    let mut group = IssueGroup::with_fix(
        "Allocation",
        first_diagnostic_fix(summaries, "high_allocations"),
    );
    match names.as_slice() {
        [] => {}
        [summary] => group.push_at(
            summary,
            format!("{} allocates during measurement.", summary.name),
        ),
        _ => group.push(format!(
            "{} benchmarks allocate during measurement.",
            names.len()
        )),
    }
    push_issue_group(groups, group);
}

fn push_variance_issues(groups: &mut Vec<IssueGroup>, summaries: &[&BenchmarkSummary]) {
    push_diagnostic_group(groups, "Variance", summaries, "high_variance", |summary| {
        let rsd = summary.stats.as_ref().map_or_else(
            || "unknown".to_string(),
            |stats| format_percent(stats.relative_std_dev),
        );
        format!("{} ({rsd})", summary.name)
    });
}

fn push_sample_count_issue(groups: &mut Vec<IssueGroup>, summaries: &[&BenchmarkSummary]) {
    let names = summaries
        .iter()
        .copied()
        .filter(|summary| has_diagnostic(summary, "too_few_samples"))
        .collect::<Vec<_>>();
    let mut group = IssueGroup::with_fix(
        "Samples",
        first_diagnostic_fix(summaries, "too_few_samples"),
    );
    match names.as_slice() {
        [] => {}
        [summary] => group.push_at(
            summary,
            format!("{} has too few measured samples.", summary.name),
        ),
        _ => group.push(format!(
            "{} benchmarks have too few measured samples.",
            names.len()
        )),
    }
    push_issue_group(groups, group);
}

fn push_quality_issue(groups: &mut Vec<IssueGroup>, summaries: &[&BenchmarkSummary]) {
    let mut group = IssueGroup::with_fix(
        "Quality",
        "Collect more samples or make the measured workload more deterministic.",
    );
    let noisy = summaries
        .iter()
        .copied()
        .filter(|summary| {
            summary.quality == QualityClass::Noisy && !has_diagnostic(summary, "high_variance")
        })
        .count();
    if noisy == 1 {
        group.push("1 benchmark has noisy results.");
    } else if noisy > 1 {
        group.push(format!("{noisy} benchmarks have noisy results."));
    }

    let untrustworthy = summaries
        .iter()
        .copied()
        .filter(|summary| {
            summary.quality == QualityClass::Untrustworthy
                && !has_diagnostic(summary, "too_few_samples")
                && !has_diagnostic(summary, "invalid_timing")
                && !has_diagnostic(summary, "zero_completed_ops")
                && !has_diagnostic(summary, "setup_dominates_measurement")
                && !has_diagnostic(summary, "budget_failure")
                && summary.correctness.passed
                && summary.budget_results.iter().all(|result| result.passed)
        })
        .count();
    if untrustworthy == 1 {
        group.push("1 benchmark has untrustworthy results.");
        group.fix = Some(
            "Inspect diagnostics and increase measurement reliability before using this row."
                .to_string(),
        );
    } else if untrustworthy > 1 {
        group.push(format!(
            "{untrustworthy} benchmarks have untrustworthy results."
        ));
        group.fix = Some(
            "Inspect diagnostics and increase measurement reliability before using these rows."
                .to_string(),
        );
    }
    push_issue_group(groups, group);
}

fn push_diagnostic_group<F>(
    groups: &mut Vec<IssueGroup>,
    title: &'static str,
    summaries: &[&BenchmarkSummary],
    code: &str,
    format_item: F,
) where
    F: Fn(&BenchmarkSummary) -> String,
{
    let mut group = IssueGroup::with_fix(title, first_diagnostic_fix(summaries, code));
    for summary in summaries
        .iter()
        .copied()
        .filter(|summary| has_diagnostic(summary, code))
    {
        group.push_at(summary, format_item(summary));
    }
    push_issue_group(groups, group);
}

fn push_issue_group(groups: &mut Vec<IssueGroup>, group: IssueGroup) {
    if !group.items.is_empty() {
        groups.push(group);
    }
}

/// Fix text for `code`: the row's own contextual suggestion when present,
/// otherwise the catalog fix.
fn diagnostic_fix(summary: &BenchmarkSummary, code: &str) -> String {
    summary
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == code)
        .and_then(|diagnostic| diagnostic.suggestions.first())
        .map_or_else(|| catalog_fix(code).to_string(), Clone::clone)
}

fn first_diagnostic_fix(summaries: &[&BenchmarkSummary], code: &str) -> String {
    summaries
        .iter()
        .copied()
        .find(|summary| has_diagnostic(summary, code))
        .map_or_else(
            || catalog_fix(code).to_string(),
            |summary| diagnostic_fix(summary, code),
        )
}

fn has_diagnostic(summary: &BenchmarkSummary, code: &str) -> bool {
    summary
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code)
}

fn summary_attention_rank(
    run: &StressRun,
    summary: &BenchmarkSummary,
    comparisons: &BTreeMap<&str, &ComparisonResult>,
) -> Option<u8> {
    if !summary.correctness.passed {
        return Some(0);
    }
    if summary.budget_results.iter().any(|result| !result.passed) {
        return Some(1);
    }
    if run.environment.profile_config.fail_on_quality
        && quality_rank(summary.quality) < quality_rank(run.environment.profile_config.min_quality)
    {
        return Some(2);
    }
    if comparisons
        .get(summary.benchmark_id.as_str())
        .is_some_and(|comparison| comparison.classification == ComparisonClass::Regression)
    {
        return Some(3);
    }
    if summary.quality == QualityClass::Untrustworthy {
        return Some(4);
    }
    if summary.quality == QualityClass::Noisy {
        return Some(5);
    }
    if !summary.diagnostics.is_empty() {
        return Some(6);
    }
    if comparisons
        .get(summary.benchmark_id.as_str())
        .is_some_and(|comparison| {
            matches!(
                comparison.classification,
                ComparisonClass::Improvement | ComparisonClass::Inconclusive
            ) && comparison.change_percent.is_some()
        })
    {
        return Some(7);
    }
    None
}

fn format_summary_blocks(run: &StressRun) -> String {
    let mut output = String::new();
    let correctness_failed = failed_correctness_count(&run.summaries);
    let budget_failures = budget_failure_count(&run.summaries);
    let quality_failures = quality_gate_failures(run).len();
    let regressions = regression_gate_count(run);
    let diagnostic_failures = diagnostic_gate_count(run);
    let improvements = comparison_count(run, ComparisonClass::Improvement);
    let _ = writeln!(output, "Summary");
    let _ = writeln!(output, "  benchmarks:      {}", run.summaries.len());
    let _ = writeln!(output, "  gate:            {}", gate_status(run));
    let _ = writeln!(
        output,
        "  correctness_ok:  {}",
        run.summaries.len().saturating_sub(correctness_failed)
    );
    let _ = writeln!(output, "  correctness_bad: {correctness_failed}");
    let _ = writeln!(output, "  budget_failed:   {budget_failures}");
    let _ = writeln!(output, "  regressions:     {regressions}");
    let _ = writeln!(output, "  diagnostics:     {diagnostic_failures}");
    let _ = writeln!(output, "  quality_failed:  {quality_failures}");
    let _ = writeln!(output, "  improvements:    {improvements}");
    let counts = quality_counts(&run.summaries);
    let trust_counts = trust_class_counts(&run.summaries);
    let _ = writeln!(output, "Quality");
    let _ = writeln!(output, "  authoritative:   {}", counts.authoritative);
    let _ = writeln!(output, "  acceptable:      {}", counts.acceptable);
    let _ = writeln!(output, "  noisy:           {}", counts.noisy);
    let _ = writeln!(output, "  untrustworthy:   {}", counts.untrustworthy);
    let _ = writeln!(output, "Trust");
    let _ = writeln!(output, "  gate:            {}", trust_counts.gate);
    let _ = writeln!(output, "  diagnostic:      {}", trust_counts.diagnostic);
    let _ = writeln!(output, "  experimental:    {}", trust_counts.experimental);
    let _ = writeln!(output, "  invalid:         {}", trust_counts.invalid);
    output
}

pub(crate) fn attention_items(run: &StressRun) -> Vec<(String, String)> {
    let comparisons = comparison_by_benchmark(run);
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    push_correctness_attention(&mut items, &mut seen, &run.summaries);
    push_budget_attention(&mut items, &mut seen, &run.summaries);
    push_comparison_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        &comparisons,
        ComparisonClass::Regression,
    );
    push_diagnostic_gate_attention(&mut items, &mut seen, run);
    push_quality_gate_attention(&mut items, &mut seen, run);
    push_noisy_attention(&mut items, &mut seen, &run.summaries);
    push_diagnostic_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        "single_op_throughput",
    );
    push_diagnostic_attention(&mut items, &mut seen, &run.summaries, "tiny_micro_timing");
    push_diagnostic_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        "likely_optimized_away",
    );
    push_untrustworthy_attention(&mut items, &mut seen, &run.summaries);
    push_comparison_attention(
        &mut items,
        &mut seen,
        &run.summaries,
        &comparisons,
        ComparisonClass::Improvement,
    );
    push_stable_change_attention(&mut items, &mut seen, &run.summaries, &comparisons);
    items
}

fn push_budget_attention(
    items: &mut Vec<(String, String)>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
) {
    for summary in summaries
        .iter()
        .filter(|summary| summary.budget_results.iter().any(|result| !result.passed))
    {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push((
                summary.benchmark_id.clone(),
                format!("! {} budget failed: {}", summary.name, budget_note(summary)),
            ));
        }
    }
}

fn push_diagnostic_attention(
    items: &mut Vec<(String, String)>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
    code: &str,
) {
    for summary in summaries
        .iter()
        .filter(|summary| summary.diagnostics.iter().any(|item| item.code == code))
    {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push((
                summary.benchmark_id.clone(),
                format!("! {} {}", summary.name, diagnostic_note(summary, code)),
            ));
        }
    }
}

fn push_correctness_attention(
    items: &mut Vec<(String, String)>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
) {
    for summary in summaries
        .iter()
        .filter(|summary| !summary.correctness.passed)
    {
        seen.insert(summary.benchmark_id.clone());
        items.push((
            summary.benchmark_id.clone(),
            format!(
                "✗ {} correctness failed: {}",
                summary.name,
                correctness_note(&summary.correctness)
            ),
        ));
    }
}

fn push_comparison_attention(
    items: &mut Vec<(String, String)>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
    comparisons: &BTreeMap<&str, &ComparisonResult>,
    class: ComparisonClass,
) {
    let mut rows = summaries
        .iter()
        .filter_map(|summary| comparisons.get(summary.benchmark_id.as_str()).copied())
        .filter(|comparison| {
            class == ComparisonClass::Regression || comparison_is_trustworthy(comparison)
        })
        .filter(|comparison| comparison.classification == class)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .change_percent
            .unwrap_or_default()
            .abs()
            .total_cmp(&left.change_percent.unwrap_or_default().abs())
    });
    for comparison in rows {
        if seen.insert(comparison.benchmark_id.clone()) {
            let icon = if class == ComparisonClass::Regression {
                "↓"
            } else {
                "↑"
            };
            items.push((
                comparison.benchmark_id.clone(),
                format!(
                    "{icon} {} {}",
                    comparison.benchmark_id,
                    format_delta_cell(comparison)
                ),
            ));
        }
    }
}

fn push_quality_gate_attention(
    items: &mut Vec<(String, String)>,
    seen: &mut BTreeSet<String>,
    run: &StressRun,
) {
    for summary in quality_gate_failures(run) {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push((
                summary.benchmark_id.clone(),
                format!(
                    "! {} quality gate failed: quality={} below min={} {}",
                    summary.name,
                    summary.quality,
                    run.environment.profile_config.min_quality,
                    row_notes(summary)
                ),
            ));
        }
    }
}

fn push_diagnostic_gate_attention(
    items: &mut Vec<(String, String)>,
    seen: &mut BTreeSet<String>,
    run: &StressRun,
) {
    for diagnostic in run.diagnostic_gate_failures() {
        if seen.insert(diagnostic.benchmark_id.clone()) {
            items.push((
                diagnostic.benchmark_id.clone(),
                format!(
                    "! {} diagnostic {}={}: {}",
                    diagnostic.name, diagnostic.severity, diagnostic.code, diagnostic.reason
                ),
            ));
        }
    }
}

fn push_noisy_attention(
    items: &mut Vec<(String, String)>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
) {
    let mut rows = summaries
        .iter()
        .filter(|summary| summary.quality == QualityClass::Noisy)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .stats
            .as_ref()
            .map_or(0.0, |stats| stats.relative_std_dev)
            .total_cmp(
                &left
                    .stats
                    .as_ref()
                    .map_or(0.0, |stats| stats.relative_std_dev),
            )
    });
    for summary in rows {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push((
                summary.benchmark_id.clone(),
                format!("! {} {}", summary.name, quality_note("noisy", summary)),
            ));
        }
    }
}

fn push_untrustworthy_attention(
    items: &mut Vec<(String, String)>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
) {
    let mut rows = summaries
        .iter()
        .filter(|summary| summary.quality == QualityClass::Untrustworthy)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .stats
            .as_ref()
            .map_or(0.0, |stats| stats.relative_std_dev)
            .total_cmp(
                &left
                    .stats
                    .as_ref()
                    .map_or(0.0, |stats| stats.relative_std_dev),
            )
    });
    for summary in rows {
        if seen.insert(summary.benchmark_id.clone()) {
            items.push((
                summary.benchmark_id.clone(),
                format!("! {} {}", summary.name, row_notes(summary)),
            ));
        }
    }
}

fn push_stable_change_attention(
    items: &mut Vec<(String, String)>,
    seen: &mut BTreeSet<String>,
    summaries: &[BenchmarkSummary],
    comparisons: &BTreeMap<&str, &ComparisonResult>,
) {
    let mut rows = summaries
        .iter()
        .filter(|summary| is_trustworthy(summary))
        .filter_map(|summary| comparisons.get(summary.benchmark_id.as_str()).copied())
        .filter(|comparison| comparison.classification == ComparisonClass::Inconclusive)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .change_percent
            .unwrap_or_default()
            .abs()
            .total_cmp(&left.change_percent.unwrap_or_default().abs())
    });
    for comparison in rows.into_iter().take(3) {
        if seen.insert(comparison.benchmark_id.clone()) {
            items.push((
                comparison.benchmark_id.clone(),
                format!(
                    "~ {} {}",
                    comparison.benchmark_id,
                    format_delta_cell(comparison)
                ),
            ));
        }
    }
}

fn comparison_by_benchmark(run: &StressRun) -> BTreeMap<&str, &ComparisonResult> {
    run.comparisons
        .iter()
        .map(|comparison| (comparison.benchmark_id.as_str(), comparison))
        .collect()
}

fn format_delta_cell(comparison: &ComparisonResult) -> String {
    let Some(change) = comparison.change_percent else {
        if comparison.reason.is_some() {
            return "semantic change".to_string();
        }
        return "-".to_string();
    };
    if comparison.reason.is_some() {
        return format!("{change:+.1}% semantic change");
    }
    if !comparison_is_trustworthy(comparison) {
        return format!("{change:+.1}% noisy");
    }
    let label = match comparison.classification {
        ComparisonClass::Regression => "regression",
        ComparisonClass::Improvement => "improved",
        ComparisonClass::Inconclusive | ComparisonClass::MissingBaseline => "unchanged",
    };
    format!("{change:+.1}% {label}")
}

fn comparison_is_trustworthy(comparison: &ComparisonResult) -> bool {
    is_comparison_quality_trustworthy(comparison.current_quality)
        && comparison
            .baseline_quality
            .is_some_and(is_comparison_quality_trustworthy)
}

const fn is_comparison_quality_trustworthy(quality: QualityClass) -> bool {
    matches!(
        quality,
        QualityClass::Authoritative | QualityClass::Acceptable
    )
}

fn row_notes(summary: &BenchmarkSummary) -> String {
    if !summary.correctness.passed {
        return correctness_note(&summary.correctness);
    }
    if summary.budget_results.iter().any(|result| !result.passed) {
        return budget_note(summary);
    }
    if !summary.diagnostics.is_empty() {
        return summary
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic_detail(summary, diagnostic))
            .collect::<Vec<_>>()
            .join("; ");
    }
    match summary.quality {
        QualityClass::Authoritative | QualityClass::Acceptable => String::new(),
        QualityClass::Noisy => quality_note("noisy", summary),
        QualityClass::Untrustworthy => quality_note("untrustworthy", summary),
    }
}

fn budget_note(summary: &BenchmarkSummary) -> String {
    summary
        .budget_results
        .iter()
        .filter(|result| !result.passed)
        .map(|result| {
            result.reason.as_ref().map_or_else(
                || format!("{} failed", result.metric),
                |reason| {
                    let mut note = format!("{} {reason}", result.metric);
                    if allocation_budget_unavailable(result) {
                        note.push_str(
                            "; allocation budgets require cntryl_stress::stress_allocator!()",
                        );
                    }
                    note
                },
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn allocation_budget_unavailable(result: &crate::artifact::BudgetResult) -> bool {
    matches!(
        result.metric.as_str(),
        "max_allocs_per_op" | "max_bytes_per_op"
    ) && result.actual.is_none()
}

fn quality_note(label: &str, summary: &BenchmarkSummary) -> String {
    let mut parts = vec![label.to_string()];
    if summary.measured_samples < 2 {
        parts.push(format!("samples={}", summary.measured_samples));
    }
    if let Some(stats) = &summary.stats {
        parts.push(format!("rsd={}", format_percent(stats.relative_std_dev)));
    }
    if let Some(advice) = quality_advice(summary) {
        parts.push(format!("fix: {advice}"));
    }
    parts.join(", ")
}

fn quality_advice(summary: &BenchmarkSummary) -> Option<String> {
    if summary.measured_samples < 5 {
        return Some(format!(
            "collect more measured samples; avoid one-sample release gates; {}",
            tier_recipe(summary)
        ));
    }
    if summary
        .stats
        .as_ref()
        .is_some_and(|stats| stats.relative_std_dev > 0.10)
    {
        return Some(format!(
            "use deterministic fixtures and move setup outside measurement; {}",
            tier_recipe(summary)
        ));
    }
    (summary.quality == QualityClass::Noisy).then(|| {
        format!(
            "use deterministic fixtures and move setup outside measurement; {}",
            tier_recipe(summary)
        )
    })
}

fn diagnostic_note(summary: &BenchmarkSummary, code: &str) -> String {
    summary
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == code)
        .map_or_else(
            || code.to_string(),
            |diagnostic| diagnostic_detail(summary, diagnostic),
        )
}

fn diagnostic_detail(summary: &BenchmarkSummary, diagnostic: &BenchmarkDiagnostic) -> String {
    let mut detail = format!("{}: {}", diagnostic.code, diagnostic.reason);
    if !diagnostic.suggestions.is_empty() {
        detail.push_str(" fix: ");
        detail.push_str(&diagnostic.suggestions.join("; "));
    }
    if matches!(
        diagnostic.code.as_str(),
        "zero_completed_ops" | "single_op_throughput" | "too_fast" | "setup_dominates_measurement"
    ) {
        detail.push_str("; ");
        detail.push_str(tier_recipe(summary));
    }
    detail
}

fn tier_recipe(summary: &BenchmarkSummary) -> &'static str {
    match summary.tier {
        1 => "Tier 1 recipe: ctx.measure(\"name\", || hot_path())",
        2 => {
            "Tier 2 recipe: ctx.measure(\"name\", || one_operation()) or ctx.measure_batch(\"name\", n, || batch())"
        }
        3 => "Tier 3 recipe: #[stress(tier = 3)] with ctx.measure_batch(\"name\", n, || batch())",
        4 => "Tier 4 recipe: #[stress(tier = 4)] with ctx.measure_batch(\"name\", n, || batch()) or ctx.record_external(\"name\", duration, n)",
        5 => "Tier 5 recipe: #[stress(tier = 5)] with scale parameters and ctx.measure_batch(\"name\", n, || batch())",
        6 => "Tier 6 recipe: #[stress(tier = 6)] with ctx.measure_batch(\"name\", n, || batch()) or ctx.record_external(\"name\", duration, n)",
        _ => "undefined tier: cntryl-stress defines tiers 1 through 6; choose the closest defined tier before authoring the benchmark",
    }
}

fn correctness_note(correctness: &CorrectnessSummary) -> String {
    let counters = correctness.counters;
    let mut parts = vec![
        format!("attempted={}", counters.attempted),
        format!("completed={}", counters.completed),
    ];
    if counters.attempted > counters.completed {
        parts.push(format!("lost={}", counters.attempted - counters.completed));
    }
    for (label, value) in [
        ("failed", counters.failures),
        ("timed_out", counters.timeouts),
        ("duplicates", counters.duplicates),
        ("dropped", counters.dropped),
        ("validation_errors", counters.validation_errors),
    ] {
        if value != 0 {
            parts.push(format!("{label}={value}"));
        }
    }
    parts.join(" ")
}

fn quality_counts(summaries: &[BenchmarkSummary]) -> QualityCounts {
    summaries
        .iter()
        .fold(QualityCounts::default(), |mut acc, summary| {
            match summary.quality {
                QualityClass::Authoritative => acc.authoritative += 1,
                QualityClass::Acceptable => acc.acceptable += 1,
                QualityClass::Noisy => acc.noisy += 1,
                QualityClass::Untrustworthy => acc.untrustworthy += 1,
            }
            acc
        })
}

#[derive(Default)]
struct QualityCounts {
    authoritative: usize,
    acceptable: usize,
    noisy: usize,
    untrustworthy: usize,
}

#[derive(Default)]
struct TrustClassCounts {
    gate: usize,
    diagnostic: usize,
    experimental: usize,
    invalid: usize,
}

fn failed_correctness_count(summaries: &[BenchmarkSummary]) -> usize {
    summaries
        .iter()
        .filter(|summary| !summary.correctness.passed)
        .count()
}

fn budget_failure_count(summaries: &[BenchmarkSummary]) -> usize {
    summaries
        .iter()
        .filter(|summary| summary.budget_results.iter().any(|result| !result.passed))
        .count()
}

fn trust_class_counts(summaries: &[BenchmarkSummary]) -> TrustClassCounts {
    summaries
        .iter()
        .fold(TrustClassCounts::default(), |mut acc, summary| {
            match summary.trust_class {
                TrustClass::Gate => acc.gate += 1,
                TrustClass::Diagnostic => acc.diagnostic += 1,
                TrustClass::Experimental => acc.experimental += 1,
                TrustClass::Invalid => acc.invalid += 1,
            }
            acc
        })
}

fn quality_gate_failures(run: &StressRun) -> Vec<&BenchmarkSummary> {
    let profile_config = &run.environment.profile_config;
    if !profile_config.fail_on_quality {
        return Vec::new();
    }
    run.summaries
        .iter()
        .filter(|summary| summary.is_intended_gate())
        .filter(|summary| {
            !summary.is_gate()
                || quality_rank(summary.quality) < quality_rank(profile_config.min_quality)
        })
        .collect()
}

fn regression_gate_count(run: &StressRun) -> usize {
    run.rejected_gate_comparisons().len()
        + if run.environment.profile_config.fail_on_regression {
            run.regressions().len()
        } else {
            0
        }
}

fn diagnostic_gate_count(run: &StressRun) -> usize {
    run.diagnostic_gate_failures().len()
}

fn comparison_count(run: &StressRun, class: ComparisonClass) -> usize {
    let gate_rows = run
        .summaries
        .iter()
        .filter(|summary| summary.is_intended_gate() && summary.is_gate())
        .map(|summary| summary.benchmark_id.as_str())
        .collect::<BTreeSet<_>>();
    run.comparisons
        .iter()
        .filter(|comparison| comparison_is_trustworthy(comparison))
        .filter(|comparison| gate_rows.contains(comparison.benchmark_id.as_str()))
        .filter(|comparison| comparison.classification == class)
        .count()
}

fn gate_status(run: &StressRun) -> String {
    if run.metadata.contains_key("reporter_errors") {
        return "failed artifact publication".to_string();
    }
    if failed_correctness_count(&run.summaries) != 0 {
        return "failed correctness".to_string();
    }
    if budget_failure_count(&run.summaries) != 0 || !run.regression_budgets_passed() {
        return "failed budget".to_string();
    }
    if run.summaries.is_empty() {
        return "failed quality (no benchmark rows)".to_string();
    }
    let invalid_rows = run
        .summaries
        .iter()
        .filter(|summary| summary.trust_class == TrustClass::Invalid)
        .count();
    let smoke_profile = run.run_profile == RunProfile::Smoke
        || run.environment.profile_config.profile == RunProfile::Smoke;
    if invalid_rows != 0 && !smoke_profile {
        return if invalid_rows == 1 {
            "failed quality (1 invalid row)".to_string()
        } else {
            format!("failed quality ({invalid_rows} invalid rows)")
        };
    }
    let profile_config = &run.environment.profile_config;
    let performance_gate_enabled = run.run_profile == crate::artifact::RunProfile::Release
        || profile_config.fail_on_quality
        || profile_config.fail_on_regression;
    if performance_gate_enabled && !run.gate_obligations_satisfied() {
        let failures = run
            .summaries
            .iter()
            .filter(|summary| summary.is_intended_gate() && !summary.is_gate())
            .count();
        return if failures == 0 {
            "failed quality (no intended gate rows)".to_string()
        } else {
            format!("failed quality ({failures} intended gates are not trustworthy)")
        };
    }
    let regressions = regression_gate_count(run);
    if regressions != 0 {
        return format!("failed regression ({regressions})");
    }
    let diagnostic_failures = diagnostic_gate_count(run);
    if diagnostic_failures != 0 {
        let policy = &run.environment.profile_config;
        let denied = run
            .diagnostic_gate_failures()
            .into_iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .filter(|code| policy.deny_codes.iter().any(|denied| denied == code))
            .collect::<BTreeSet<_>>();
        if denied.is_empty() {
            let threshold = policy
                .deny_diagnostics
                .map_or("unknown".to_string(), |threshold| threshold.to_string());
            return format!("failed diagnostics ({diagnostic_failures} >= {threshold})");
        }
        let denied = denied.into_iter().collect::<Vec<_>>().join(", ");
        return format!("failed diagnostics ({diagnostic_failures}; denied codes: {denied})");
    }
    let quality_failures = quality_gate_failures(run).len();
    if quality_failures != 0 {
        return format!(
            "failed quality ({quality_failures} below {})",
            run.environment.profile_config.min_quality
        );
    }
    "passed".to_string()
}

const fn quality_rank(quality: QualityClass) -> u8 {
    match quality {
        QualityClass::Untrustworthy => 0,
        QualityClass::Noisy => 1,
        QualityClass::Acceptable => 2,
        QualityClass::Authoritative => 3,
    }
}

fn is_trustworthy(summary: &BenchmarkSummary) -> bool {
    summary.correctness.passed
        && summary.is_gate()
        && matches!(
            summary.quality,
            QualityClass::Authoritative | QualityClass::Acceptable
        )
}

fn truncate_name_to_width(name: &str, width: usize) -> String {
    let chars = name.chars().collect::<Vec<_>>();
    if chars.len() <= width {
        return name.to_string();
    }
    if width <= 2 {
        return chars.into_iter().take(width).collect();
    }

    let keep = width.saturating_sub(2);
    let head_len = keep.div_ceil(2);
    let tail_len = keep / 2;
    let head = chars[..head_len].iter().collect::<String>();
    let tail = chars[chars.len() - tail_len..].iter().collect::<String>();
    format!("{head}..{tail}")
}

fn format_metric_value(value: f64, summary: &BenchmarkSummary) -> String {
    if !value.is_finite() {
        return "n/a".to_string();
    }
    let unit = display_unit(summary);
    match summary.primary_metric {
        PrimaryMetric::Throughput => format!("{}/s", format_scaled_with_unit(value, &unit)),
        PrimaryMetric::LatencyP95 | PrimaryMetric::NsPerOp => {
            format!("{}/{}", format_duration_ns(f64_to_u128(value)), unit)
        }
    }
}

fn format_human_metric_value(value: f64, summary: &BenchmarkSummary) -> String {
    if !value.is_finite() {
        return "n/a".to_string();
    }
    match summary.primary_metric {
        PrimaryMetric::Throughput => format_scaled_number(value),
        PrimaryMetric::LatencyP95 | PrimaryMetric::NsPerOp => {
            format_duration_ns(f64_to_u128(value))
        }
    }
}

fn format_human_metric_stat<F>(
    summary: &BenchmarkSummary,
    stats: Option<&SummaryStats>,
    select: F,
) -> String
where
    F: FnOnce(&SummaryStats) -> f64,
{
    stats.map_or_else(
        || "n/a".to_string(),
        |stats| format_human_metric_value(select(stats), summary),
    )
}

fn format_scaled_number(value: f64) -> String {
    let abs = value.abs();
    if abs >= 1_000_000.0 {
        format!("{:.2}M", value / 1_000_000.0)
    } else if abs >= 1_000.0 {
        format!("{:.2}K", value / 1_000.0)
    } else if abs >= 10.0 {
        format!("{value:.2}")
    } else if abs >= 0.01 {
        format!("{value:.4}")
    } else if abs == 0.0 {
        "0.00".to_string()
    } else {
        format!("{value:.2e}")
    }
}

fn format_percent(ratio: f64) -> String {
    if ratio.is_finite() {
        format!("{:.1}%", ratio * 100.0)
    } else {
        "n/a".to_string()
    }
}

fn format_scaled_with_unit(value: f64, unit: &str) -> String {
    format!("{} {}", format_scaled_number(value), unit)
}

fn write_summary_line(output: &mut String, summary: &BenchmarkSummary) {
    let value = summary
        .primary_value()
        .map_or_else(|| "n/a".to_string(), |value| format_metric(value, summary));
    let _ = writeln!(
        output,
        "  {name:<NAME_WIDTH$} {value:>VALUE_WIDTH$}  tier={tier} trust={trust} mode={mode} quality={quality} samples={samples} wall={wall} question={question}",
        name = summary.name,
        tier = summary.tier,
        trust = summary.trust_class,
        mode = measurement_mode_label(summary),
        quality = summary.quality,
        samples = summary.measured_samples,
        wall = format_duration_ns(summary.total_wall_clock_ns),
        question = measurement_question(summary),
    );
}

fn write_comparison_section(
    output: &mut String,
    title: &str,
    run: &StressRun,
    class: ComparisonClass,
) {
    let rows = run
        .comparisons
        .iter()
        .filter(|comparison| comparison.classification == class)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return;
    }

    let _ = writeln!(output);
    let _ = writeln!(output, "{title}");
    output.push_str(&"-".repeat(title.len()));
    output.push('\n');
    for comparison in rows {
        let _ = writeln!(
            output,
            "  {} {:+.1}% ({:?})",
            comparison.benchmark_id,
            comparison.change_percent.unwrap_or_default(),
            comparison.primary_metric
        );
    }
}

fn write_quality_section(output: &mut String, run: &StressRun) {
    let rows = run
        .summaries
        .iter()
        .filter(|summary| {
            matches!(
                summary.quality,
                QualityClass::Noisy | QualityClass::Untrustworthy
            )
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return;
    }

    output.push_str("\nNoisy Or Untrustworthy\n");
    output.push_str("----------------------\n");
    for summary in rows {
        let _ = writeln!(
            output,
            "  {} quality={} correctness={}",
            summary.name, summary.quality, summary.correctness.passed
        );
    }
}

fn write_sweep_tables(output: &mut String, run: &StressRun) {
    let mut header_written = false;
    for group in sweep_groups(&run.summaries) {
        if group.points.len() < 2 {
            continue;
        }
        if !header_written {
            output.push_str("\nSweep Tables\n");
            output.push_str("------------\n");
            header_written = true;
        }
        let rows = group
            .points
            .iter()
            .map(|point| (point.x, point.y, &run.summaries[point.index]))
            .collect::<Vec<SweepRow<'_>>>();
        write_sweep_group(output, &group.parameter, &group.key, &rows);
    }
}

type SweepRow<'a> = (f64, f64, &'a BenchmarkSummary);

fn write_sweep_group(output: &mut String, key: &str, group: &SweepGroupKey, rows: &[SweepRow<'_>]) {
    let baseline_x = rows[0].0;
    let baseline_y = rows[0].1;
    let mut plateau = None;
    let _ = writeln!(
        output,
        "Parameter: {key} (benchmark: {}, measurement: {})",
        group.base_name, group.measurement
    );
    for (idx, (x, y, summary)) in rows.iter().enumerate() {
        let speedup = if summary.primary_metric.higher_is_better() {
            y / baseline_y
        } else {
            baseline_y / y
        };
        let efficiency = if baseline_x > 0.0 && *x > 0.0 {
            speedup / (*x / baseline_x)
        } else {
            0.0
        };
        if idx > 0 && plateau.is_none() {
            let previous_y = rows[idx - 1].1;
            let gain = if summary.primary_metric.higher_is_better() {
                (y - previous_y) / previous_y
            } else {
                (previous_y - y) / previous_y
            };
            if gain < 0.10 {
                plateau = Some(*x);
            }
        }
        let _ = writeln!(
            output,
            "  {}={} value={} speedup={:.2} efficiency={:.2}",
            key,
            x,
            format_metric(*y, summary),
            speedup,
            efficiency
        );
    }
    if let Some(point) = plateau {
        let _ = writeln!(
            output,
            "  plateau: first {key} where incremental gain < 10% is {point}"
        );
    }
}

fn format_metric(value: f64, summary: &BenchmarkSummary) -> String {
    format_metric_value(value, summary)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn f64_to_u128(value: f64) -> u128 {
    value.max(0.0).round() as u128
}

#[allow(clippy::cast_precision_loss)]
fn format_duration_ns(nanos: u128) -> String {
    let secs = nanos as f64 / 1_000_000_000.0;
    if secs >= 1.0 {
        format!("{secs:.2}s")
    } else if secs >= 0.001 {
        format!("{:.2}ms", secs * 1_000.0)
    } else if secs >= 0.000_001 {
        format!("{:.2}µs", secs * 1_000_000.0)
    } else {
        format!("{:.2}ns", secs * 1_000_000_000.0)
    }
}

fn measurement_mode_label(summary: &BenchmarkSummary) -> &'static str {
    match summary
        .parameters
        .get("measurement_mode")
        .map(String::as_str)
    {
        Some("micro") => "micro",
        Some("fixed_ops") => "fixed_ops",
        Some("duration") => "duration",
        _ if summary.tier == 1 => "micro",
        _ if summary.tier == 2 => "fixed_ops",
        _ => "duration",
    }
}

fn measurement_question(summary: &BenchmarkSummary) -> String {
    let mut parts = vec![format!("mode={}", measurement_mode_label(summary))];
    if let Some(basis) = normalization_basis(summary) {
        parts.push(basis);
    }
    parts.join("; ")
}

pub(crate) fn human_measurement_label(summary: &BenchmarkSummary) -> String {
    let unit = display_unit(summary);
    match summary.primary_metric {
        PrimaryMetric::Throughput => format!("{unit}/s"),
        PrimaryMetric::LatencyP95 | PrimaryMetric::NsPerOp => format!("time/{unit}"),
    }
}

pub(crate) fn display_unit(summary: &BenchmarkSummary) -> String {
    normalization_basis_unit(summary)
        .or_else(|| logical_unit(summary))
        .unwrap_or_else(|| "op".to_string())
}

fn logical_unit(summary: &BenchmarkSummary) -> Option<String> {
    summary.parameters.get("logical_unit").map(|value| {
        value
            .rsplit('_')
            .next()
            .map_or_else(|| value.clone(), ToString::to_string)
    })
}

fn normalization_basis_unit(summary: &BenchmarkSummary) -> Option<String> {
    summary.parameters.iter().find_map(|(key, _value)| {
        key.strip_suffix("_per_logical_operation")
            .map(singularize_unit)
    })
}

fn normalization_basis(summary: &BenchmarkSummary) -> Option<String> {
    summary.parameters.iter().find_map(|(key, value)| {
        key.strip_suffix("_per_logical_operation").map(|unit| {
            let display_unit = singularize_unit(unit);
            let logical_unit = logical_unit(summary).unwrap_or_else(|| "op".to_string());
            format!("{value} {unit}/{logical_unit}")
                .replace(unit, &display_unit.replace('_', " "))
                .replace('_', " ")
        })
    })
}

fn singularize_unit(value: &str) -> String {
    if let Some(stem) = value.strip_suffix("ies") {
        format!("{stem}y")
    } else if let Some(stem) = value.strip_suffix('s') {
        stem.to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{
        BenchmarkBudgets, BenchmarkMode, BenchmarkSpec, BudgetResult, ComparisonResult,
        ConsoleNameMode, CorrectnessCounters, CorrectnessSummary, DiagnosticSeverity,
        EnvironmentInfo, MeasurementIntent, Sample, SamplePhase, SummaryStats, SCHEMA_VERSION,
    };
    use std::sync::Arc;

    fn summary(name: &str, value: f64, quality: QualityClass) -> BenchmarkSummary {
        BenchmarkSummary {
            benchmark_id: name.to_string(),
            name: name.to_string(),
            tier: 2,
            intent: MeasurementIntent::General,
            primary_metric: PrimaryMetric::Throughput,
            measured_samples: 10,
            warmup_samples: 1,
            cooldown_samples: 0,
            stats: SummaryStats::from_values(&[value, value * 1.01])
                .or_else(|| SummaryStats::from_values(&[value])),
            wall_clock: SummaryStats::from_values(&[1_000_000.0]),
            total_wall_clock_ns: 1_000_000,
            ns_per_op: None,
            gross_ns_per_op: None,
            overhead_ns_per_op: None,
            allocs_per_op: None,
            bytes_per_op: None,
            observations: Vec::new(),
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
            source: None,
            peak_rss_bytes: None,
            latency_distribution: None,
        }
    }

    fn run_with_summaries(summaries: Vec<BenchmarkSummary>) -> StressRun {
        let profile_config =
            crate::config::StressRunnerConfig::for_profile(crate::artifact::RunProfile::Release)
                .profile_config();
        StressRun {
            schema_version: SCHEMA_VERSION.to_string(),
            tool_version: "0.4.0".to_string(),
            suite: "suite".to_string(),
            run_profile: profile_config.profile,
            environment: EnvironmentInfo::unknown(profile_config.clone()),
            benchmark_specs: vec![BenchmarkSpec {
                id: "bench".to_string(),
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
                benchmark_id: "bench".to_string(),
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
                observations: Vec::new(),
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
            confirmation_runs: Vec::new(),
            started_at: "123".to_string(),
            total_elapsed_ns: 1_000,
            metadata: BTreeMap::new(),
        }
    }

    fn diagnostic(code: &str, reason: &str, suggestion: &str) -> BenchmarkDiagnostic {
        BenchmarkDiagnostic {
            code: code.to_string(),
            severity: DiagnosticSeverity::Warning,
            reason: reason.to_string(),
            evidence: BTreeMap::new(),
            suggestions: vec![suggestion.to_string()],
        }
    }

    fn unique_test_path(label: &str) -> PathBuf {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "cntryl-stress-{label}-{}-{sequence}",
            std::process::id()
        ))
    }

    #[test]
    fn non_finite_samples_dropped_has_a_console_issue_group() {
        let mut summary = summary("dropped", 1_000.0, QualityClass::Acceptable);
        summary.diagnostics = vec![diagnostic(
            "non_finite_samples_dropped",
            "Non-finite metric values were dropped before computing statistics.",
            crate::diagnostics::catalog_fix("non_finite_samples_dropped"),
        )];
        let groups = suite_issue_groups(&[&summary], &BTreeMap::new());
        let group = groups
            .iter()
            .find(|group| group.title == "Non-finite samples")
            .expect("non-finite issue group");
        assert_eq!(
            group.fix.as_deref(),
            Some(crate::diagnostics::catalog_fix(
                "non_finite_samples_dropped"
            ))
        );
    }

    #[test]
    fn atomic_write_replaces_complete_files_without_leaving_staging_files() {
        let directory = unique_test_path("atomic-write");
        std::fs::create_dir_all(&directory).expect("test directory");
        let path = directory.join("latest.json");

        atomic_write(&path, b"first").expect("first write");
        atomic_write(&path, b"second").expect("replacement write");

        assert_eq!(std::fs::read(&path).expect("artifact"), b"second");
        assert!(std::fs::read_dir(&directory)
            .expect("directory")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
        std::fs::remove_dir_all(directory).expect("cleanup test directory");
    }

    #[test]
    fn json_reporter_propagates_artifact_write_failures() {
        let output_file = unique_test_path("reporter-error");
        std::fs::write(&output_file, b"not a directory").expect("blocking file");
        let reporter = JsonReporter::new(&output_file);
        let run = run_with_summaries(vec![summary(
            "queue::fast",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);

        let error = reporter
            .suite_end(&run)
            .expect_err("a file cannot be used as the output directory");

        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotADirectory
        ));
        std::fs::remove_file(output_file).expect("cleanup blocking file");
    }

    #[test]
    fn json_reporter_publishes_one_complete_eight_file_transaction() {
        let output_dir = unique_test_path("reporter-transaction-success");
        let reporter = JsonReporter::new(&output_dir);
        let run = run_with_summaries(vec![summary(
            "queue::fast",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);

        reporter.suite_end(&run).expect("publish artifact set");

        let suite_dir = output_dir.join("suite");
        let timestamp_json = std::fs::read(suite_dir.join(format!("{}.json", run.started_at)))
            .expect("timestamp JSON");
        let timestamp_text = std::fs::read(suite_dir.join(format!("{}.txt", run.started_at)))
            .expect("timestamp text");
        let timestamp_markdown = std::fs::read(suite_dir.join(format!("{}.md", run.started_at)))
            .expect("timestamp markdown");
        assert_eq!(
            std::fs::read(suite_dir.join("latest.json")).expect("latest JSON"),
            timestamp_json
        );
        assert_eq!(
            std::fs::read(suite_dir.join("latest.txt")).expect("latest text"),
            timestamp_text
        );
        assert_eq!(
            std::fs::read(suite_dir.join("latest.md")).expect("latest markdown"),
            timestamp_markdown
        );
        assert_eq!(
            std::fs::read_dir(&suite_dir)
                .expect("suite directory")
                .filter(|entry| {
                    !entry
                        .as_ref()
                        .expect("suite entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with('.')
                })
                .count(),
            8
        );
        let timestamp_csv =
            std::fs::read_to_string(suite_dir.join(format!("{}.csv", run.started_at)))
                .expect("timestamp CSV");
        assert_eq!(
            std::fs::read_to_string(suite_dir.join("latest.csv")).expect("latest CSV"),
            timestamp_csv
        );
        assert!(timestamp_csv.starts_with("suite,benchmark_id,name,parameters,"));
        assert!(timestamp_csv.contains("\r\nsuite,queue::fast,queue::fast,"));
        assert!(suite_dir.join(ARTIFACT_PUBLICATION_LOCK_FILE).is_file());
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn json_reporter_rejects_a_timestamp_collision_without_overwriting_history() {
        let output_dir = unique_test_path("reporter-timestamp-collision");
        let reporter = JsonReporter::new(&output_dir);
        let first = run_with_summaries(vec![summary(
            "queue::first",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);
        reporter.suite_end(&first).expect("publish first run");
        let suite_dir = output_dir.join("suite");
        let original = ["json", "txt", "md"]
            .into_iter()
            .map(|extension| {
                let path = suite_dir.join(format!("{}.{}", first.started_at, extension));
                (
                    path.clone(),
                    std::fs::read(path).expect("original timestamp artifact"),
                )
            })
            .collect::<Vec<_>>();

        let second = run_with_summaries(vec![summary(
            "queue::second",
            2_000_000.0,
            QualityClass::Authoritative,
        )]);
        let error = reporter
            .suite_end(&second)
            .expect_err("a timestamp stem is immutable once published");

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(error.to_string().contains(&first.started_at));
        for (path, contents) in original {
            assert_eq!(
                std::fs::read(path).expect("preserved timestamp artifact"),
                contents
            );
        }
        let latest: StressRun = serde_json::from_slice(
            &std::fs::read(suite_dir.join("latest.json")).expect("latest artifact"),
        )
        .expect("latest run");
        assert_eq!(latest.summaries[0].name, "queue::first");
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn json_reporter_serializes_same_suite_publication_before_committing() {
        let output_dir = unique_test_path("reporter-suite-lock");
        let mut first = run_with_summaries(vec![summary(
            "queue::first",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);
        first.started_at = "100".to_string();
        let mut second = run_with_summaries(vec![summary(
            "queue::second",
            2_000_000.0,
            QualityClass::Authoritative,
        )]);
        second.started_at = "200".to_string();
        let (first_entered_tx, first_entered_rx) = std::sync::mpsc::channel();
        let (release_first_tx, release_first_rx) = std::sync::mpsc::channel();
        let first_output = output_dir.clone();
        let first_thread = std::thread::spawn(move || {
            let reporter = JsonReporter::new(first_output);
            let mut paused = false;
            reporter.write_results_inner_with_hook(&first, |point| {
                if point == ArtifactPublicationPoint::BeforeCommit && !paused {
                    paused = true;
                    first_entered_tx.send(()).expect("signal first commit");
                    release_first_rx.recv().expect("release first commit");
                }
                Ok(())
            })
        });
        first_entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("first publisher reached commit");

        let start_second = std::sync::Arc::new(std::sync::Barrier::new(2));
        let second_start = std::sync::Arc::clone(&start_second);
        let (second_entered_tx, second_entered_rx) = std::sync::mpsc::channel();
        let second_output = output_dir.clone();
        let second_thread = std::thread::spawn(move || {
            let reporter = JsonReporter::new(second_output);
            let mut signalled = false;
            second_start.wait();
            reporter.write_results_inner_with_hook(&second, |_| {
                if !signalled {
                    signalled = true;
                    second_entered_tx.send(()).expect("signal second commit");
                }
                Ok(())
            })
        });
        start_second.wait();

        assert!(matches!(
            second_entered_rx.recv_timeout(std::time::Duration::from_millis(250)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        release_first_tx.send(()).expect("finish first publication");
        first_thread
            .join()
            .expect("join first publisher")
            .expect("first publication");
        second_entered_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("second publisher entered after first completed");
        second_thread
            .join()
            .expect("join second publisher")
            .expect("second publication");

        let suite_dir = output_dir.join("suite");
        let latest: StressRun = serde_json::from_slice(
            &std::fs::read(suite_dir.join("latest.json")).expect("latest artifact"),
        )
        .expect("latest run");
        assert_eq!(latest.started_at, "200");
        assert!(suite_dir.join("100.json").exists());
        assert!(suite_dir.join("200.json").exists());
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn next_publisher_recovers_an_interrupted_mixed_generation() {
        let output_dir = unique_test_path("reporter-crash-recovery");
        let reporter = JsonReporter::new(&output_dir);
        let mut first = run_with_summaries(vec![summary(
            "queue::first",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);
        first.started_at = "100".to_string();
        reporter.suite_end(&first).expect("publish first run");

        let mut interrupted = run_with_summaries(vec![summary(
            "queue::interrupted",
            2_000_000.0,
            QualityClass::Authoritative,
        )]);
        interrupted.started_at = "200".to_string();
        let publication = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut committed = 0;
            reporter
                .write_results_inner_with_hook(&interrupted, |point| {
                    if point == ArtifactPublicationPoint::BeforeCommit {
                        assert!(committed != 3, "injected process interruption");
                        committed += 1;
                    }
                    Ok(())
                })
                .expect("interrupted publication does not return");
        }));
        assert!(publication.is_err());

        let suite_dir = output_dir.join("suite");
        assert!(suite_dir.join("200.txt").exists());
        assert!(suite_dir.join("200.md").exists());
        assert!(!suite_dir.join("200.json").exists());
        assert!(std::fs::read_to_string(suite_dir.join("latest.txt"))
            .expect("mixed latest text")
            .contains("queue::interrupted"));
        let old_latest: StressRun = serde_json::from_slice(
            &std::fs::read(suite_dir.join("latest.json")).expect("old latest JSON"),
        )
        .expect("old latest run");
        assert_eq!(old_latest.started_at, "100");

        let mut next = run_with_summaries(vec![summary(
            "queue::next",
            3_000_000.0,
            QualityClass::Authoritative,
        )]);
        next.started_at = "300".to_string();
        reporter
            .suite_end(&next)
            .expect("recover and publish next run");

        for extension in ["json", "txt", "md", "csv"] {
            assert!(!suite_dir.join(format!("200.{extension}")).exists());
            assert!(suite_dir.join(format!("300.{extension}")).exists());
        }
        let latest: StressRun = serde_json::from_slice(
            &std::fs::read(suite_dir.join("latest.json")).expect("latest artifact"),
        )
        .expect("latest run");
        assert_eq!(latest.started_at, "300");
        assert!(!std::fs::read_dir(&suite_dir)
            .expect("suite directory")
            .any(|entry| entry
                .expect("suite entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".artifact-transaction.")));
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn next_publisher_recovers_an_interrupted_history_only_generation() {
        let output_dir = unique_test_path("reporter-history-only-recovery");
        let reporter = JsonReporter::new(&output_dir).announce(false);
        let mut newest = run_with_summaries(vec![summary(
            "queue::newest",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);
        newest.started_at = "300".to_string();
        reporter.suite_end(&newest).expect("publish newest run");

        // An older, slower run publishes only its history files; interrupt it.
        let mut older = run_with_summaries(vec![summary(
            "queue::older",
            2_000_000.0,
            QualityClass::Authoritative,
        )]);
        older.started_at = "200".to_string();
        let publication = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut committed = 0;
            reporter
                .write_results_inner_with_hook(&older, |point| {
                    if point == ArtifactPublicationPoint::BeforeCommit {
                        assert!(committed != 2, "injected process interruption");
                        committed += 1;
                    }
                    Ok(())
                })
                .expect("interrupted publication does not return");
        }));
        assert!(publication.is_err());

        let mut next = run_with_summaries(vec![summary(
            "queue::next",
            3_000_000.0,
            QualityClass::Authoritative,
        )]);
        next.started_at = "400".to_string();
        reporter
            .suite_end(&next)
            .expect("recover the history-only generation and publish");
        let suite_dir = output_dir.join("suite");
        for extension in ["json", "txt", "md", "csv"] {
            assert!(!suite_dir.join(format!("200.{extension}")).exists());
            assert!(suite_dir.join(format!("300.{extension}")).exists());
            assert!(suite_dir.join(format!("400.{extension}")).exists());
        }
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn csv_report_has_one_escaped_row_per_summary_with_comparison() {
        let mut fast = summary("=queue,fast", 1_000_000.0, QualityClass::Authoritative);
        fast.parameters
            .insert("size".to_string(), "1,024".to_string());
        let mut slow = summary("queue::slow", 10.0, QualityClass::Acceptable);
        slow.primary_metric = PrimaryMetric::NsPerOp;
        let mut run = run_with_summaries(vec![fast, slow]);
        let mut comparison = crate::artifact::ComparisonResult::new(
            "queue::slow",
            PrimaryMetric::NsPerOp,
            QualityClass::Acceptable,
            ComparisonClass::Regression,
            0.05,
        );
        comparison.change_percent = Some(-12.5);
        run.comparisons.push(comparison);

        let csv = crate::csv::format_csv_report(&run);
        let lines = csv.split("\r\n").collect::<Vec<_>>();
        assert_eq!(lines.len(), 4, "{csv}");
        assert_eq!(lines[0], crate::csv::CSV_HEADER.join(","));
        assert!(
            lines[1]
                .starts_with("suite,\"'=queue,fast\",\"'=queue,fast\",\"size=1,024\",throughput,"),
            "{}",
            lines[1]
        );
        assert!(lines[1].contains(",op/s,"), "{}", lines[1]);
        assert!(lines[1].ends_with(",authoritative,gate,,"), "{}", lines[1]);
        assert!(lines[2].contains(",ns_per_op,"), "{}", lines[2]);
        assert!(lines[2].contains(",ns/op,"), "{}", lines[2]);
        assert!(
            lines[2].ends_with(",acceptable,gate,regression,-12.5"),
            "{}",
            lines[2]
        );
        assert_eq!(lines[3], "");
    }

    #[test]
    fn recovery_accepts_legacy_generations_without_csv() {
        let suite_dir = Path::new("suite");
        let manifest = |names: &[&str]| ArtifactTransactionManifest {
            targets: names.iter().map(ToString::to_string).collect(),
        };
        for names in [
            &[
                "1.txt",
                "1.md",
                "latest.txt",
                "latest.md",
                "1.json",
                "latest.json",
            ][..],
            &["1.txt", "1.md", "1.json"][..],
            &[
                "1.txt",
                "1.md",
                "latest.txt",
                "latest.md",
                "1.csv",
                "latest.csv",
                "1.json",
                "latest.json",
            ][..],
            &["1.txt", "1.md", "1.csv", "1.json"][..],
        ] {
            assert!(
                recovered_artifact_targets(suite_dir, &manifest(names)).is_ok(),
                "{names:?}"
            );
        }
        for names in [
            &["1.txt", "1.md", "latest.txt", "1.json"][..],
            &["1.txt", "1.md", "1.csv", "latest.csv", "1.json"][..],
            &["1.txt", "1.json", "2.md"][..],
            &["1.txt", "1.txt", "1.md", "1.json"][..],
        ] {
            assert!(
                recovered_artifact_targets(suite_dir, &manifest(names)).is_err(),
                "{names:?}"
            );
        }
    }

    #[test]
    fn next_publisher_finishes_cleanup_for_committed_transactions() {
        let output_dir = unique_test_path("reporter-committed-recovery");
        let reporter = JsonReporter::new(&output_dir);
        let mut first = run_with_summaries(vec![summary(
            "queue::first",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);
        first.started_at = "100".to_string();
        reporter.suite_end(&first).expect("publish first run");

        let suite_dir = output_dir.join("suite");
        let renamed = suite_dir.join(format!("{ARTIFACT_COMMITTED_TRANSACTION_PREFIX}fixture"));
        std::fs::create_dir(&renamed).expect("create renamed committed transaction");
        std::fs::write(renamed.join("cleanup-debris"), b"debris")
            .expect("write committed cleanup debris");
        let marked = suite_dir.join(format!("{ARTIFACT_TRANSACTION_PREFIX}fixture"));
        std::fs::create_dir(&marked).expect("create marked transaction");
        std::fs::write(marked.join(ARTIFACT_TRANSACTION_COMMITTED), b"")
            .expect("write committed marker");

        let mut next = run_with_summaries(vec![summary(
            "queue::next",
            2_000_000.0,
            QualityClass::Authoritative,
        )]);
        next.started_at = "200".to_string();
        reporter
            .suite_end(&next)
            .expect("clean and publish next run");

        assert!(!renamed.exists());
        assert!(!marked.exists());
        assert!(suite_dir.join("100.json").exists());
        assert!(suite_dir.join("200.json").exists());
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn json_reporter_does_not_publish_json_after_a_human_artifact_failure() {
        let output_dir = unique_test_path("reporter-late-error");
        let suite_dir = output_dir.join("suite");
        std::fs::create_dir_all(suite_dir.join("latest.txt"))
            .expect("create path that blocks latest text publication");
        let reporter = JsonReporter::new(&output_dir);
        let run = run_with_summaries(vec![summary(
            "queue::fast",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);

        reporter
            .suite_end(&run)
            .expect_err("a directory cannot be replaced by latest text");

        assert!(!suite_dir.join("latest.json").exists());
        assert!(!suite_dir.join(format!("{}.json", run.started_at)).exists());
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn json_reporter_rolls_back_every_artifact_after_an_injected_finalization_failure() {
        let output_dir = unique_test_path("reporter-transaction-injected-error");
        let suite_dir = output_dir.join("suite");
        std::fs::create_dir_all(&suite_dir).expect("create suite directory");
        std::fs::write(suite_dir.join("latest.json"), b"previous json")
            .expect("previous latest json");
        std::fs::write(suite_dir.join("latest.txt"), b"previous text")
            .expect("previous latest text");
        std::fs::write(suite_dir.join("latest.md"), b"previous markdown")
            .expect("previous latest markdown");
        std::fs::write(suite_dir.join("latest.csv"), b"previous csv").expect("previous latest csv");
        let reporter = JsonReporter::new(&output_dir);
        let run = run_with_summaries(vec![summary(
            "queue::fast",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);

        let error = reporter
            .write_results_inner_with_hook(&run, |point| {
                if point == ArtifactPublicationPoint::BeforeFinalize {
                    Err(std::io::Error::other("injected finalization failure"))
                } else {
                    Ok(())
                }
            })
            .expect_err("finalization failure must fail publication");

        assert!(error.to_string().contains("injected finalization failure"));
        assert_eq!(
            std::fs::read(suite_dir.join("latest.json")).expect("restored latest json"),
            b"previous json"
        );
        assert_eq!(
            std::fs::read(suite_dir.join("latest.txt")).expect("restored latest text"),
            b"previous text"
        );
        assert_eq!(
            std::fs::read(suite_dir.join("latest.md")).expect("restored latest markdown"),
            b"previous markdown"
        );
        assert_eq!(
            std::fs::read(suite_dir.join("latest.csv")).expect("restored latest csv"),
            b"previous csv"
        );
        for extension in ["json", "txt", "md", "csv"] {
            assert!(!suite_dir
                .join(format!("{}.{extension}", run.started_at))
                .exists());
        }
        assert!(std::fs::read_dir(&suite_dir)
            .expect("suite directory")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".artifact-transaction.")));
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn json_reporter_rolls_back_after_a_real_final_path_failure() {
        let output_dir = unique_test_path("reporter-transaction-filesystem-error");
        let suite_dir = output_dir.join("suite");
        std::fs::create_dir_all(&suite_dir).expect("create suite directory");
        std::fs::write(suite_dir.join("latest.txt"), b"previous text")
            .expect("previous latest text");
        std::fs::write(suite_dir.join("latest.md"), b"previous markdown")
            .expect("previous latest markdown");
        std::fs::create_dir(suite_dir.join("latest.json"))
            .expect("directory that blocks final JSON publication");
        let reporter = JsonReporter::new(&output_dir);
        let run = run_with_summaries(vec![summary(
            "queue::fast",
            1_000_000.0,
            QualityClass::Authoritative,
        )]);

        reporter
            .suite_end(&run)
            .expect_err("a directory cannot be replaced by latest JSON");

        assert_eq!(
            std::fs::read(suite_dir.join("latest.txt")).expect("restored latest text"),
            b"previous text"
        );
        assert_eq!(
            std::fs::read(suite_dir.join("latest.md")).expect("restored latest markdown"),
            b"previous markdown"
        );
        assert!(suite_dir.join("latest.json").is_dir());
        assert!(!suite_dir.join("latest.csv").exists());
        for extension in ["json", "txt", "md", "csv"] {
            assert!(!suite_dir
                .join(format!("{}.{extension}", run.started_at))
                .exists());
        }
        assert!(std::fs::read_dir(&suite_dir)
            .expect("suite directory")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".artifact-transaction.")));
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn artifact_suite_directory_rejects_invalid_suite_names() {
        for invalid in ["..", ".", "", "a/b", "a\\b", "a b", "a|b", "../x"] {
            assert!(
                artifact_suite_directory_name(invalid).is_err(),
                "{invalid:?} must be rejected"
            );
        }
        assert_eq!(
            artifact_suite_directory_name("suite-1.v_2").expect("valid"),
            "suite-1.v_2"
        );
    }

    #[test]
    fn json_reporter_rejects_invalid_suite_name() {
        let output_dir = unique_test_path("reporter-invalid-suite");
        let mut run = run_with_summaries(Vec::new());
        run.suite = "../escape".to_string();
        let error = JsonReporter::new(&output_dir)
            .announce(false)
            .write_results_inner(&run)
            .expect_err("invalid suite name");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let _ = std::fs::remove_dir_all(output_dir);
    }

    #[test]
    fn older_run_does_not_replace_newer_latest_artifacts() {
        let output_dir = unique_test_path("reporter-stale-latest");
        let reporter = JsonReporter::new(&output_dir).announce(false);
        let mut newer = run_with_summaries(vec![summary(
            "queue::newer",
            1.0,
            QualityClass::Authoritative,
        )]);
        newer.started_at = "200".to_string();
        let mut older = run_with_summaries(vec![summary(
            "queue::older",
            1.0,
            QualityClass::Authoritative,
        )]);
        older.started_at = "100".to_string();
        reporter.write_results_inner(&newer).expect("newer");
        reporter.write_results_inner(&older).expect("older");

        let suite_dir = output_dir.join("suite");
        let latest: StressRun =
            serde_json::from_slice(&std::fs::read(suite_dir.join("latest.json")).expect("latest"))
                .expect("latest run");
        assert_eq!(latest.started_at, "200");
        let latest_md = std::fs::read_to_string(suite_dir.join("latest.md")).expect("md");
        assert!(latest_md.contains("queue::newer"));
        assert!(suite_dir.join("100.json").exists());
        assert!(suite_dir.join("100.md").exists());
        std::fs::remove_dir_all(output_dir).expect("cleanup reporter test directory");
    }

    #[test]
    fn markdown_escapes_table_cells() {
        assert_eq!(escape_markdown_cell("a|b\\c\nd\r\ne"), "a\\|b\\\\c d e");
        let run = run_with_summaries(vec![summary(
            "bad|name\nnext",
            1.0,
            QualityClass::Authoritative,
        )]);
        let markdown = format_markdown_report(&run);
        assert!(markdown.contains("| bad\\|name next |"), "{markdown}");
        assert!(!markdown.lines().any(|line| line.starts_with("next")));
    }

    #[test]
    fn sweep_tables_do_not_mix_unrelated_benchmarks_or_units() {
        let mut a1 = summary("alpha-1", 100.0, QualityClass::Acceptable);
        a1.parameters
            .insert("client_count".to_string(), "1".to_string());
        let mut a2 = summary("alpha-2", 200.0, QualityClass::Acceptable);
        a2.parameters
            .insert("client_count".to_string(), "2".to_string());
        let mut b4 = summary("beta-4", 10_000.0, QualityClass::Acceptable);
        b4.parameters
            .insert("client_count".to_string(), "4".to_string());
        let mut a8 = summary("alpha-8", 400.0, QualityClass::Acceptable);
        a8.parameters
            .insert("client_count".to_string(), "8".to_string());
        a8.parameters
            .insert("logical_unit".to_string(), "bytes".to_string());
        let run = run_with_summaries(vec![a1, a2, b4, a8]);

        let report = format_report(&run);

        assert!(report.contains("client_count=2"), "{report}");
        assert!(!report.contains("client_count=4"), "{report}");
        assert!(!report.contains("client_count=8"), "{report}");
    }

    #[test]
    fn formats_summary_and_noisy_rows() {
        let run = run_with_summaries(vec![
            summary("fast", 1_000_000.0, QualityClass::Authoritative),
            summary("weak", 10.0, QualityClass::Noisy),
        ]);

        let report = format_report(&run);

        assert!(report.contains("Summary"));
        assert!(report.contains("fast"));
        assert!(report.contains("Noisy Or Untrustworthy"));
        assert!(report.contains("weak quality=noisy"));
    }

    fn located_micro_summary() -> BenchmarkSummary {
        let mut located = summary("queue::tiny", 1_000_000.0, QualityClass::Acceptable);
        located.source = Some(crate::artifact::SourceLocation::new("benches/queue.rs", 42));
        located.diagnostics.push(BenchmarkDiagnostic::new(
            "likely_optimized_away",
            DiagnosticSeverity::Warning,
            "too fast",
        ));
        located
    }

    #[test]
    fn console_issue_groups_print_the_source_location() {
        let run = run_with_summaries(vec![located_micro_summary()]);

        let report = format_console_output(&run);

        assert!(
            report.contains("at benches/queue.rs:42"),
            "missing location in:\n{report}"
        );
    }

    #[test]
    fn markdown_attention_items_print_the_source_location() {
        let run = run_with_summaries(vec![located_micro_summary()]);

        let markdown = format_markdown_report(&run);

        assert!(
            markdown.contains("at `benches/queue.rs:42`"),
            "missing location in:\n{markdown}"
        );
    }

    #[test]
    fn tail_distributions_render_in_console_and_markdown() {
        let mut run = run_with_summaries(vec![
            summary("svc/request", 1_000.0, QualityClass::Acceptable),
            summary("svc/queue", 1_000.0, QualityClass::Acceptable),
        ]);
        assert!(!format_console_output(&run).contains("tail "));
        assert!(!format_markdown_report(&run).contains("## Tail Distributions"));

        run.summaries[0].latency_distribution = Some(crate::artifact::DistributionSummary {
            count: 2_000,
            p90: 1_500.0,
            p99: 2_500.0,
            p999: Some(4_000.0),
            max: 9_000.0,
        });
        let mut observation = crate::artifact::ObservationSummary::new(
            "depth",
            crate::artifact::ObservationUnit::Count,
            crate::artifact::ObservationDirection::LowerIsBetter,
            SummaryStats::from_values(&[1.0, 2.0]).unwrap(),
        );
        observation.distribution = Some(crate::artifact::DistributionSummary {
            count: 20,
            p90: 18.0,
            p99: 20.0,
            p999: None,
            max: 20.0,
        });
        run.summaries[1].observations.push(observation);

        let console = format_console_output(&run);
        assert!(
            console.contains(
                "tail svc/request latency (n=2000): p90=1.50µs p99=2.50µs p99.9=4.00µs max=9.00µs"
            ),
            "{console}"
        );
        assert!(
            console.contains("tail svc/queue depth (n=20): p90=18.000 p99=20.000 max=20.000"),
            "{console}"
        );
        let markdown = format_markdown_report(&run);
        assert!(markdown.contains("## Tail Distributions"), "{markdown}");
        assert!(
            markdown
                .contains("| svc/request | latency | 2000 | 1.50µs | 2.50µs | 4.00µs | 9.00µs |"),
            "{markdown}"
        );
        assert!(
            markdown.contains("| svc/queue | depth | 20 | 18.000 | 20.000 | - | 20.000 |"),
            "{markdown}"
        );
    }

    #[test]
    fn peak_rss_renders_in_console_and_markdown() {
        let mut run = run_with_summaries(vec![located_micro_summary(), located_micro_summary()]);
        run.summaries[1].benchmark_id = "suite/grower".to_string();
        assert!(!format_console_output(&run).contains("peak RSS"));
        assert!(!format_markdown_report(&run).contains("## Memory"));

        run.summaries[0].peak_rss_bytes = Some(200 * 1024 * 1024);
        assert!(
            format_console_output(&run).contains("peak RSS: 200.0 MiB (process-wide)\n"),
            "a lone first row carries the process baseline, not growth"
        );
        run.summaries[1].peak_rss_bytes = Some(300 * 1024 * 1024);
        let console = format_console_output(&run);
        assert!(
            console.contains("peak RSS: 300.0 MiB (process-wide; largest growth at suite/grower)"),
            "{console}"
        );
        let markdown = format_markdown_report(&run);
        assert!(markdown.contains("## Memory"), "{markdown}");
        assert!(
            markdown
                .contains("- Peak RSS: 300.0 MiB (process-wide; largest growth at `suite/grower`)"),
            "{markdown}"
        );
    }

    #[test]
    fn environment_observations_render_in_console_and_markdown() {
        let mut run = run_with_summaries(vec![located_micro_summary()]);
        assert!(!format_console_output(&run).contains("environment:"));
        assert!(!format_markdown_report(&run).contains("## Environment"));

        run.environment.observations = vec![
            crate::artifact::EnvironmentObservation::new(
                "cpu_governor",
                "powersave",
                true,
                "clocks may ramp",
            ),
            crate::artifact::EnvironmentObservation::new(
                "load_average",
                "0.10 (4 cores)",
                false,
                "",
            ),
        ];
        let console = format_console_output(&run);
        assert!(
            console.contains(
                "environment: cpu_governor=powersave (adverse), load_average=0.10 (4 cores)"
            ),
            "{console}"
        );
        assert!(
            console.contains("environment warning: cpu_governor: clocks may ramp"),
            "{console}"
        );

        let markdown = format_markdown_report(&run);
        assert!(markdown.contains("## Environment"), "{markdown}");
        assert!(
            markdown.contains("- `cpu_governor`: powersave (adverse: clocks may ramp)"),
            "{markdown}"
        );
        assert!(
            markdown.contains("- `load_average`: 0.10 (4 cores)\n"),
            "{markdown}"
        );
    }

    #[test]
    fn console_issue_groups_omit_location_when_unknown() {
        let mut unlocated = located_micro_summary();
        unlocated.source = None;
        let run = run_with_summaries(vec![unlocated]);

        let report = format_console_output(&run);

        assert!(
            !report.contains(" at "),
            "unexpected location in:\n{report}"
        );
    }

    #[test]
    fn console_reports_async_misuse_issue_group() {
        let mut row = summary("queue::async_row", 1_000.0, QualityClass::Acceptable);
        row.diagnostics.push(BenchmarkDiagnostic::new(
            "async_misuse",
            DiagnosticSeverity::Info,
            "no await overhead",
        ));
        let run = run_with_summaries(vec![row]);

        let report = format_console_output(&run);

        assert!(
            report.contains("Async"),
            "missing async group in:\n{report}"
        );
        assert!(report.contains("queue::async_row"));
        assert!(report.contains(catalog_fix("async_misuse")));
    }

    #[test]
    fn github_workflow_command_data_and_properties_are_escaped() {
        assert_eq!(escape_workflow_data("50% a\r\nb"), "50%25 a%0D%0Ab");
        assert_eq!(
            escape_workflow_property("C:\\a,b:c%\n"),
            "C%3A\\a%2Cb%3Ac%25%0A"
        );
    }

    fn github_reporter_for_test(
        step_summary: Option<PathBuf>,
    ) -> (GitHubActionsReporter, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let reporter = GitHubActionsReporter::new()
            .with_step_summary_path(step_summary)
            .with_annotation_buffer(Arc::clone(&buffer));
        (reporter, buffer)
    }

    fn annotations(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buffer.lock().expect("buffer").clone()).expect("utf8")
    }

    #[test]
    fn github_reporter_annotates_gate_failures_and_diagnostics() {
        let mut failing = summary("queue::broken", 1_000.0, QualityClass::Acceptable);
        failing.source = Some(crate::artifact::SourceLocation::new("benches/q,1.rs", 12));
        failing.correctness.passed = false;
        let mut warned = summary("queue::tiny", 1_000.0, QualityClass::Acceptable);
        warned.source = Some(crate::artifact::SourceLocation::new("benches/q.rs", 30));
        warned.diagnostics.push(BenchmarkDiagnostic::new(
            "likely_optimized_away",
            DiagnosticSeverity::Warning,
            "50% too\nfast",
        ));
        let mut informed = summary("queue::info", 1_000.0, QualityClass::Acceptable);
        informed.diagnostics.push(BenchmarkDiagnostic::new(
            "async_misuse",
            DiagnosticSeverity::Info,
            "no await",
        ));
        let mut errored = summary("queue::err", 1_000.0, QualityClass::Acceptable);
        errored.diagnostics.push(BenchmarkDiagnostic::new(
            "invalid_timing",
            DiagnosticSeverity::Error,
            "bad clock",
        ));
        let run = run_with_summaries(vec![failing, warned, informed, errored]);
        let (reporter, buffer) = github_reporter_for_test(None);

        reporter.suite_end(&run).expect("github reporter");

        let output = annotations(&buffer);
        assert!(
            output.contains("::error file=benches/q%2C1.rs,line=12,title=stress%3A correctness failed::queue::broken"),
            "{output}"
        );
        assert!(
            output.contains("::warning file=benches/q.rs,line=30,title=stress%3A likely_optimized_away::queue::tiny: 50%25 too%0Afast"),
            "{output}"
        );
        assert!(
            output.contains("::notice title=stress%3A async_misuse::queue::info: no await"),
            "{output}"
        );
        assert!(
            output.contains("::error title=stress%3A invalid_timing::queue::err: bad clock"),
            "{output}"
        );
        assert!(
            output.lines().all(|line| line.starts_with("::")),
            "{output}"
        );
    }

    #[test]
    fn github_reporter_step_summary_failure_warns_without_failing() {
        let run = run_with_summaries(vec![summary(
            "queue::fast",
            1_000.0,
            QualityClass::Acceptable,
        )]);
        let (reporter, buffer) = github_reporter_for_test(Some(std::env::temp_dir()));

        reporter
            .suite_end(&run)
            .expect("step summary errors are not reporter failures");

        assert!(annotations(&buffer).contains("::warning title=stress%3A step summary::"));
    }

    #[test]
    fn github_reporter_emits_a_run_level_error_when_the_gate_fails() {
        let run = run_with_summaries(Vec::new());
        assert_ne!(
            crate::runner::evaluate_run_gate(&run),
            crate::runner::RunGate::Passed
        );
        let (reporter, buffer) = github_reporter_for_test(None);

        reporter.suite_end(&run).expect("github reporter");

        assert!(
            annotations(&buffer).contains("::error title=stress%3A gate failed::suite"),
            "{}",
            annotations(&buffer)
        );
    }

    #[test]
    fn github_reporter_only_errors_on_regressions_that_fail_the_gate() {
        let mut row = summary("queue::row", 1_000.0, QualityClass::Acceptable);
        row.trust_class = TrustClass::Diagnostic;
        row.metadata
            .insert("trust_class".to_string(), "gate".to_string());
        let mut run = run_with_summaries(vec![row]);
        run.environment.profile_config.fail_on_regression = true;
        let mut comparison = ComparisonResult::new(
            "queue::row",
            PrimaryMetric::Throughput,
            QualityClass::Acceptable,
            ComparisonClass::Regression,
            5.0,
        );
        comparison.change_percent = Some(-20.0);
        run.comparisons.push(comparison);
        assert!(run.regressions().is_empty());
        let (reporter, buffer) = github_reporter_for_test(None);

        reporter.suite_end(&run).expect("github reporter");

        let output = annotations(&buffer);
        assert!(
            output.contains("::warning title=stress%3A regression::"),
            "{output}"
        );
    }

    fn noise_gating_run(cleared: bool) -> StressRun {
        let mut run = run_with_summaries(vec![summary(
            "queue::fast",
            1_000.0,
            QualityClass::Acceptable,
        )]);
        run.metadata
            .insert("baseline_runs_pooled".to_string(), "3".to_string());
        run.metadata.insert(
            "baseline_runs_skipped".to_string(),
            "old.json: CPU model differs".to_string(),
        );
        let mut attempt = crate::artifact::ConfirmationRun::new(1);
        attempt.benchmark_ids = vec!["suite/queue".to_string()];
        attempt.regressions_before = vec!["suite/queue/fast".to_string()];
        if !cleared {
            attempt.regressions_after = attempt.regressions_before.clone();
        }
        attempt.samples_added = 10;
        run.confirmation_runs.push(attempt);
        run
    }

    #[test]
    fn noise_gating_outcome_is_reported_everywhere() {
        let cleared = noise_gating_run(true);
        let lines = noise_gating_lines(&cleared);
        assert_eq!(
            lines,
            vec![
                "baseline: pooled 3 saved runs (skipped: old.json: CPU model differs)".to_string(),
                "confirmation attempt 1: re-ran 1 benchmark(s), +10 samples; regressions 1 -> 0"
                    .to_string(),
                "confirmation: regressions cleared after 1 attempt(s); samples pooled across attempts"
                    .to_string(),
            ]
        );
        assert!(format_console_run(&cleared).contains("confirmation: regressions cleared"));
        assert!(format_markdown_report(&cleared).contains("confirmation: regressions cleared"));
        assert!(
            format_github_annotations(&cleared).contains("::notice title=stress%3A confirmation::")
        );

        let persisted = noise_gating_run(false);
        let lines = noise_gating_lines(&persisted);
        assert!(lines
            .last()
            .is_some_and(|line| line.contains("regressions persisted after 1 attempt(s)")));

        let plain = run_with_summaries(vec![summary(
            "queue::fast",
            1_000.0,
            QualityClass::Acceptable,
        )]);
        assert!(noise_gating_lines(&plain).is_empty());

        let mut skipped = plain;
        skipped.metadata.insert(
            "confirmation_skipped".to_string(),
            "budget failed".to_string(),
        );
        assert_eq!(
            noise_gating_lines(&skipped),
            vec!["confirmation: skipped (budget failed)".to_string()]
        );
    }

    #[test]
    fn github_reporter_appends_markdown_to_the_step_summary() {
        let path = std::env::temp_dir().join(format!(
            "stress-step-summary-{}-{}.md",
            std::process::id(),
            line!()
        ));
        std::fs::write(&path, "previous step\n").expect("seed summary");
        let run = run_with_summaries(vec![summary(
            "queue::fast",
            1_000.0,
            QualityClass::Acceptable,
        )]);
        let (reporter, _buffer) = github_reporter_for_test(Some(path.clone()));

        reporter.suite_end(&run).expect("first append");
        reporter.suite_end(&run).expect("second append");

        let contents = std::fs::read_to_string(&path).expect("read summary");
        let _ = std::fs::remove_file(&path);
        assert!(contents.starts_with("previous step\n"), "{contents}");
        let markdown = format_markdown_report(&run);
        assert_eq!(contents.matches(markdown.as_str()).count(), 2, "{contents}");
        assert!(contents.contains("## Needs attention"));
    }

    #[test]
    fn formats_console_output_as_human_table() {
        let run = run_with_summaries(vec![
            summary("queue::fast", 1_000_000.0, QualityClass::Authoritative),
            summary("queue::slow", 10.0, QualityClass::Acceptable),
        ]);

        let report = format_console_output(&run);

        assert!(report.contains("@cntryl/stress v0.4.0"));
        assert!(report.contains("suite"));
        let header = report
            .lines()
            .find(|line| line.starts_with("benchmark"))
            .expect("human table header");
        assert_eq!(
            header.split_whitespace().collect::<Vec<_>>(),
            vec![
                "benchmark",
                "measurement",
                "value",
                "p50",
                "p95",
                "p99",
                "rsd",
                "trust",
                "mode"
            ]
        );
        assert!(report.contains("queue::fast"));
        assert!(report.contains("op/s"));
        assert!(report.contains("1.00M"));
        let fast_row = report
            .lines()
            .find(|line| line.starts_with("queue::fast"))
            .expect("fast benchmark row");
        assert!(fast_row.contains(" op/s "));
        assert_eq!(fast_row.matches("op/s").count(), 1);
        assert!(!report.contains("Summary"));
        assert!(!report.contains("Quality"));
        assert!(!report.contains("issues"));
        assert!(!report.contains("Run summary"));
        assert!(report.trim_end().ends_with("result: passed"));
    }

    #[test]
    fn human_console_middle_truncates_long_names() {
        assert_eq!(
            truncate_name_to_width("abcdefghijklmnopqrstuvwx", 10),
            "abcd..uvwx"
        );
    }

    #[test]
    fn human_console_uses_twelve_char_minimum_for_non_name_columns() {
        let rows = vec![HumanTableRow {
            name: "row".to_string(),
            measurement: "op/s".to_string(),
            value: "1".to_string(),
            p50: "1".to_string(),
            p95: "1".to_string(),
            p99: "1".to_string(),
            rsd: "0%".to_string(),
            trust: "gate".to_string(),
            mode: "micro",
        }];

        let layout = HumanTableLayout::for_rows(&rows, HUMAN_TABLE_NAME_DEFAULT_WIDTH);

        assert_eq!(layout.name, HUMAN_TABLE_NAME_DEFAULT_WIDTH);
        assert_eq!(layout.measurement, HUMAN_TABLE_COLUMN_MIN_WIDTH);
        assert_eq!(layout.metric, HUMAN_TABLE_COLUMN_MIN_WIDTH);
        assert_eq!(layout.rsd, HUMAN_TABLE_COLUMN_MIN_WIDTH);
        assert_eq!(layout.trust, HUMAN_TABLE_COLUMN_MIN_WIDTH);
        assert_eq!(layout.mode, HUMAN_TABLE_COLUMN_MIN_WIDTH);
    }

    #[test]
    fn compact_console_names_allow_64_characters() {
        let name = "a".repeat(64);

        assert_eq!(
            truncate_name_to_width(&name, HUMAN_TABLE_NAME_LABEL_MAX_WIDTH),
            name
        );
    }

    #[test]
    fn compact_console_names_are_64_character_labels() {
        let mut row = summary(
            "storage::reader::payload_lookup_for_large_client_group_with_expensive_projection",
            1_000_000.0,
            QualityClass::Acceptable,
        );
        row.parameters
            .insert("clients".to_string(), "16".to_string());

        let label = format_human_table_name(
            &row,
            ConsoleNameMode::Compact,
            HUMAN_TABLE_NAME_DEFAULT_WIDTH,
        );

        assert_eq!(label.chars().count(), HUMAN_TABLE_NAME_LABEL_MAX_WIDTH);
        assert!(label.contains(".."));
        assert!(!label.contains("clients"));
    }

    #[test]
    fn full_console_names_are_untruncated() {
        let mut row = summary(
            "storage::reader::payload_lookup_for_large_client_group",
            1_000_000.0,
            QualityClass::Acceptable,
        );
        row.parameters
            .insert("clients".to_string(), "16".to_string());
        let mut run = run_with_summaries(vec![row]);
        run.environment.profile_config.console_names = ConsoleNameMode::Full;

        let report = format_console_output(&run);

        assert!(
            report.contains("storage::reader::payload_lookup_for_large_client_group [clients=16]")
        );
    }

    #[test]
    fn human_console_lists_issues_after_table() {
        let mut noisy = summary("queue::noisy", 10.0, QualityClass::Noisy);
        noisy.diagnostics = vec![diagnostic(
            "high_variance",
            "Measured samples varied.",
            "Use deterministic fixtures.",
        )];
        let run = run_with_summaries(vec![
            summary("queue::fast", 1_000_000.0, QualityClass::Authoritative),
            noisy,
        ]);

        let report = format_console_output(&run);

        assert!(report.contains("queue::noisy"));
        assert!(report.contains("issues"));
        assert!(report.contains("  Variance"));
        assert!(report.contains("    • queue::noisy ("));
        assert!(report.contains("Fix: Use deterministic fixtures."));
        assert!(!report.contains("has elevated variance"));
        assert!(!report.contains("issue   "));
        assert!(report.find("issues").expect("issues") < report.find("result:").expect("result"));
    }

    #[test]
    fn console_explains_quality_gate_failures() {
        let run = run_with_summaries(
            (0..6)
                .map(|index| summary(&format!("queue::row_{index}"), 100.0, QualityClass::Noisy))
                .collect(),
        );

        let report = format_console_output(&run);

        assert!(report.contains("queue::row_0"));
        assert!(report.contains("issues"));
        assert!(report.contains("  Quality"));
        assert!(report.contains("    • 6 benchmarks have noisy results."));
        assert!(report.contains(
            "Fix: Collect more samples or make the measured workload more deterministic."
        ));
        assert!(!report.contains("summary: gate"));
        assert!(!report.contains("attention:"));
    }

    #[test]
    fn console_fails_when_an_intended_gate_loses_derived_trust() {
        let mut invalid = summary("queue::invalid", 100.0, QualityClass::Authoritative);
        invalid.trust_class = TrustClass::Invalid;
        let run = run_with_summaries(vec![invalid]);

        let report = format_console_output(&run);

        assert!(report
            .trim_end()
            .ends_with("result: failed (suite: failed quality (1 invalid row))"));
    }

    #[test]
    fn console_fails_release_with_no_intended_gate_rows() {
        let run = run_with_summaries(Vec::new());

        let report = format_console_output(&run);

        assert!(report
            .trim_end()
            .ends_with("result: failed (suite: failed quality (no benchmark rows))"));
    }

    #[test]
    fn human_console_groups_allocation_issues() {
        let mut first = summary("alloc_a", 100.0, QualityClass::Acceptable);
        first.diagnostics = vec![diagnostic(
            "high_allocations",
            "The benchmark allocated during measurement.",
            "Move reusable allocations into setup.",
        )];
        let mut second = summary("alloc_b", 100.0, QualityClass::Acceptable);
        second.diagnostics = vec![diagnostic(
            "high_allocations",
            "The benchmark allocated during measurement.",
            "Move reusable allocations into setup.",
        )];
        let run = run_with_summaries(vec![first, second]);

        let report = format_console_output(&run);

        assert!(report.contains("  Allocation"));
        assert!(report.contains("    • 2 benchmarks allocate during measurement."));
        assert!(report.contains("    Fix: Move reusable allocations into setup."));
        assert!(!report.contains("alloc,noise"));
    }

    #[test]
    fn markdown_explains_quality_gate_failures() {
        let run = run_with_summaries(
            (0..6)
                .map(|index| summary(&format!("queue::row_{index}"), 100.0, QualityClass::Noisy))
                .collect(),
        );

        let report = format_markdown_report(&run);

        assert!(report.contains("## Summary"));
        assert!(report.contains("gate:            failed quality (6 below acceptable)"));
        assert!(report.contains("quality_failed:  6"));
        assert!(report.contains("## Needs attention"));
        assert!(report.contains("quality gate failed"));
    }

    #[test]
    fn markdown_explains_budget_failures() {
        let mut budget = summary("budget", 100.0, QualityClass::Untrustworthy);
        budget.budget_results = vec![BudgetResult {
            metric: "max_allocs_per_op".to_string(),
            limit: 0.0,
            actual: Some(1.0),
            passed: false,
            reason: Some("1.0000 exceeds 0.0000".to_string()),
        }];
        let run = run_with_summaries(vec![budget]);

        let report = format_markdown_report(&run);

        assert!(report.contains("gate:            failed budget"));
        assert!(report.contains("budget_failed:   1"));
        assert!(report.contains("budget failed: max_allocs_per_op 1.0000 exceeds 0.0000"));
    }

    #[test]
    fn markdown_explains_tier_driven_recipe_misconfigurations() {
        let mut noisy = summary("tier3_noisy", 100.0, QualityClass::Noisy);
        noisy.tier = 3;
        let mut single_op = summary("tier3_single_op", 100.0, QualityClass::Acceptable);
        single_op.tier = 3;
        single_op.diagnostics = vec![diagnostic(
            "single_op_throughput",
            "A throughput-tier row completed only one operation per sample.",
            "Use measure_batch or record_external.",
        )];
        let mut zero = summary("tier4_zero", 100.0, QualityClass::Untrustworthy);
        zero.tier = 4;
        zero.diagnostics = vec![diagnostic(
            "zero_completed_ops",
            "At least one measured sample completed zero logical operations.",
            "Record completed logical work.",
        )];
        let mut overhead = summary("tier1_overhead", 4.0, QualityClass::Untrustworthy);
        overhead.tier = 1;
        overhead.diagnostics = vec![diagnostic(
            "setup_dominates_measurement",
            "Timing overhead or setup dominates the measured work.",
            "Increase measured work per iteration.",
        )];
        let mut suspicious = summary("tier1_suspicious", 4.0, QualityClass::Acceptable);
        suspicious.tier = 1;
        suspicious.diagnostics = vec![diagnostic(
            "tiny_micro_timing",
            "Tier 1 timing is below 15 ns/op without explicit validation.",
            "Batch more logical work per sample.",
        )];
        let mut allocation = summary("tier2_allocation", 100.0, QualityClass::Untrustworthy);
        allocation.tier = 2;
        allocation.budget_results = vec![BudgetResult {
            metric: "max_allocs_per_op".to_string(),
            limit: 0.0,
            actual: None,
            passed: false,
            reason: Some("required measurement is unavailable".to_string()),
        }];
        let run = run_with_summaries(vec![
            noisy, single_op, zero, overhead, suspicious, allocation,
        ]);

        let report = format_markdown_report(&run);

        assert!(report.contains("Tier 3 recipe: #[stress(tier = 3)] with ctx.measure_batch"));
        assert!(report.contains(
            "single_op_throughput: A throughput-tier row completed only one operation per sample"
        ));
        assert!(report.contains("Use measure_batch or record_external."));
        assert!(report.contains(
            "zero_completed_ops: At least one measured sample completed zero logical operations"
        ));
        assert!(report.contains("ctx.record_external(\"name\", duration, n)"));
        assert!(report.contains("setup_dominates_measurement: Timing overhead or setup dominates"));
        assert!(report.contains("Tier 1 recipe: ctx.measure(\"name\", || hot_path())"));
        assert!(report.contains("tiny_micro_timing: Tier 1 timing is below 15 ns/op"));
        assert!(report.contains("cntryl_stress::stress_allocator!()"));
    }

    #[test]
    fn markdown_explains_regression_failures() {
        let mut run =
            run_with_summaries(vec![summary("regressed", 80.0, QualityClass::Acceptable)]);
        run.comparisons = vec![ComparisonResult {
            benchmark_id: "regressed".to_string(),
            current_quality: QualityClass::Acceptable,
            baseline_quality: Some(QualityClass::Acceptable),
            primary_metric: PrimaryMetric::Throughput,
            baseline_value: Some(100.0),
            current_value: Some(80.0),
            change_percent: Some(-20.0),
            threshold: 0.05,
            confidence_intervals_overlap: Some(false),
            classification: ComparisonClass::Regression,
            reason: None,
        }];

        let report = format_markdown_report(&run);

        assert!(report.contains("gate:            failed regression (1)"));
        assert!(report.contains("regressions:     1"));
        assert!(report.contains("- ↓ regressed -20.0% regression"));
    }

    #[test]
    fn console_reports_explicit_regression_budget_failure_without_profile_gate() {
        let mut benchmark = summary("regressed", 80.0, QualityClass::Acceptable);
        benchmark.budgets.max_regression_pct = Some(5.0);
        let mut run = run_with_summaries(vec![benchmark]);
        let profile_config = crate::config::StressRunnerConfig::new().profile_config();
        run.run_profile = profile_config.profile;
        run.environment.profile_config = profile_config;
        run.comparisons = vec![ComparisonResult {
            benchmark_id: "regressed".to_string(),
            current_quality: QualityClass::Acceptable,
            baseline_quality: Some(QualityClass::Acceptable),
            primary_metric: PrimaryMetric::Throughput,
            baseline_value: Some(100.0),
            current_value: Some(80.0),
            change_percent: Some(-20.0),
            threshold: 0.05,
            confidence_intervals_overlap: Some(false),
            classification: ComparisonClass::Regression,
            reason: None,
        }];

        let report = format_console_output(&run);

        assert!(report.contains("regressed against baseline"));
        assert!(report
            .trim_end()
            .ends_with("result: failed (suite: failed budget)"));
    }

    #[test]
    fn human_console_lists_budget_regression_and_micro_issues() {
        let mut budget = summary("budget", 100.0, QualityClass::Untrustworthy);
        budget.budget_results = vec![BudgetResult {
            metric: "max_ns_per_op".to_string(),
            limit: 50.0,
            actual: Some(100.0),
            passed: false,
            reason: Some("100 exceeds 50".to_string()),
        }];
        let mut suspicious = summary("micro", 4.0, QualityClass::Acceptable);
        suspicious.diagnostics = vec![diagnostic(
            "tiny_micro_timing",
            "Tier 1 timing is below 15 ns/op without explicit validation.",
            "Batch more logical work per sample.",
        )];
        let mut run = run_with_summaries(vec![
            budget,
            summary("regressed", 80.0, QualityClass::Acceptable),
            suspicious,
        ]);
        run.comparisons = vec![ComparisonResult {
            benchmark_id: "regressed".to_string(),
            current_quality: QualityClass::Acceptable,
            baseline_quality: Some(QualityClass::Acceptable),
            primary_metric: PrimaryMetric::Throughput,
            baseline_value: Some(100.0),
            current_value: Some(80.0),
            change_percent: Some(-20.0),
            threshold: 0.05,
            confidence_intervals_overlap: Some(false),
            classification: ComparisonClass::Regression,
            reason: None,
        }];

        let report = format_console_output(&run);

        assert!(report.contains("  Budget"));
        assert!(report.contains("budget failed"));
        assert!(report.contains("  Regression"));
        assert!(report.contains("regressed against baseline"));
        assert!(report.contains("  Micro timing"));
        assert!(report.contains("micro is too small to trust as a gate-quality microbenchmark"));
        assert!(report.contains("Fix: Inspect the failing budget"));
        assert!(
            report.contains("Fix: Inspect the same benchmark row before updating the baseline.")
        );
        assert!(report.contains(&format!(
            "Fix: {}",
            crate::diagnostics::catalog_fix("regression")
        )));
        assert!(report.contains("Fix: Batch more logical work per sample."));
        assert!(report
            .trim_end()
            .ends_with("result: failed (suite: failed budget)"));
    }

    #[test]
    fn console_reports_measurement_trust_and_mode_columns() {
        let mut benchmark = summary("queue::row", 100.0, QualityClass::Acceptable);
        benchmark.trust_class = TrustClass::Diagnostic;
        benchmark
            .parameters
            .insert("measurement_mode".to_string(), "duration".to_string());
        benchmark
            .parameters
            .insert("logical_unit".to_string(), "transaction".to_string());
        let run = run_with_summaries(vec![benchmark]);

        let console = format_console_output(&run);
        let markdown = format_markdown_report(&run);

        assert!(console.contains("trust"));
        assert!(console.contains("mode"));
        assert!(console.contains("measurement"));
        assert!(console.contains("diagnostic"));
        assert!(console.contains("transaction/s"));
        assert!(console.contains("duration"));
        assert!(!console.contains("mode=duration"));
        assert!(markdown.contains("| Trust | Mode | Question |"));
        assert!(markdown.contains("diagnostic"));
    }

    #[test]
    fn console_preserves_fallible_benchmark_identity_and_error_message() {
        let mut benchmark = summary(
            "storage::load_fixture::benchmark error",
            0.0,
            QualityClass::Untrustworthy,
        );
        benchmark.trust_class = TrustClass::Invalid;
        benchmark.correctness.passed = false;
        benchmark.correctness.counters.attempted = 1;
        benchmark.correctness.counters.failures = 1;
        benchmark.metadata.insert(
            "benchmark_error".to_string(),
            "fixture is unavailable".to_string(),
        );
        let run = run_with_summaries(vec![benchmark]);

        let console = format_console_output(&run);

        assert!(console.contains("Benchmark error"));
        assert!(console.contains("storage::load_fixture::benchmark error"));
        assert!(console.contains("fixture is unavailable"));
        assert!(!console.contains("failed correctness checks"));
    }

    #[test]
    fn console_uses_batch_normalization_without_repeating_batch_shape() {
        let mut benchmark = summary("get_batch_hit_1000", 50_500.0, QualityClass::Acceptable);
        benchmark.primary_metric = PrimaryMetric::NsPerOp;
        benchmark
            .parameters
            .insert("logical_unit".to_string(), "cache_lookup_batch".to_string());
        benchmark.parameters.insert(
            "lookups_per_logical_operation".to_string(),
            "1000".to_string(),
        );
        let run = run_with_summaries(vec![benchmark]);

        let console = format_console_output(&run);
        let row = console
            .lines()
            .find(|line| line.starts_with("get_batch_hit_1000"))
            .expect("batch benchmark row");

        assert!(row.contains("time/lookup"));
        assert!(row.contains("µs"));
        assert!(!row.contains("1000 lookup/batch"));
    }

    #[test]
    fn formats_regressions_and_improvements() {
        let mut run = run_with_summaries(vec![summary("bench", 100.0, QualityClass::Acceptable)]);
        run.comparisons = vec![
            ComparisonResult {
                benchmark_id: "regressed".to_string(),
                current_quality: QualityClass::Acceptable,
                baseline_quality: Some(QualityClass::Acceptable),
                primary_metric: PrimaryMetric::Throughput,
                baseline_value: Some(100.0),
                current_value: Some(80.0),
                change_percent: Some(-20.0),
                threshold: 0.05,
                confidence_intervals_overlap: Some(false),
                classification: ComparisonClass::Regression,
                reason: None,
            },
            ComparisonResult {
                benchmark_id: "improved".to_string(),
                current_quality: QualityClass::Acceptable,
                baseline_quality: Some(QualityClass::Acceptable),
                primary_metric: PrimaryMetric::Throughput,
                baseline_value: Some(100.0),
                current_value: Some(130.0),
                change_percent: Some(30.0),
                threshold: 0.05,
                confidence_intervals_overlap: Some(false),
                classification: ComparisonClass::Improvement,
                reason: None,
            },
        ];

        let report = format_report(&run);

        assert!(report.contains("Regressions"));
        assert!(report.contains("regressed -20.0%"));
        assert!(report.contains("Improvements"));
        assert!(report.contains("improved +30.0%"));
    }

    #[test]
    fn sweep_group_removes_only_the_delimited_swept_value_from_the_name() {
        let mut s1 = summary("n_1_iter100", 100.0, QualityClass::Acceptable);
        s1.parameters.insert("n".to_string(), "1".to_string());
        let mut s2 = summary("n_2_iter100", 180.0, QualityClass::Acceptable);
        s2.parameters.insert("n".to_string(), "2".to_string());
        let run = run_with_summaries(vec![s1, s2]);

        let report = format_report(&run);

        assert!(report.contains("Parameter: n "), "{report}");
        assert!(report.contains("benchmark: n_*_iter100"), "{report}");
    }

    /// The `docs/bench-recipes.md` sweep recipe renders as one sweep table.
    #[test]
    fn builder_parameter_sweep_recipe_renders_a_sweep_table() {
        let config = crate::StressRunnerConfig::new()
            .samples(2)
            .warmup_samples(0)
            .cooldown_samples(0);
        let mut runner = crate::StressRunner::with_config("sweep", config);
        runner.reporters(Vec::new());
        runner.run("sum", |ctx| {
            for n in [64_u64, 256, 1024] {
                ctx.benchmark(format!("sum/size={n}"))
                    .parameter("size", n)
                    .measure(|| (0..crate::black_box(n)).sum::<u64>());
            }
        });
        let run = runner.finish();

        let report = format_report(&run);

        assert!(report.contains("Sweep Tables"), "{report}");
        assert!(
            report.contains("Parameter: size (benchmark: sum/size=*"),
            "{report}"
        );
    }

    #[test]
    fn formats_sweep_table_and_plateau() {
        let mut s1 = summary("client-1", 100.0, QualityClass::Acceptable);
        s1.parameters
            .insert("client_count".to_string(), "1".to_string());
        let mut s2 = summary("client-2", 180.0, QualityClass::Acceptable);
        s2.parameters
            .insert("client_count".to_string(), "2".to_string());
        let mut s4 = summary("client-4", 190.0, QualityClass::Acceptable);
        s4.parameters
            .insert("client_count".to_string(), "4".to_string());
        let run = run_with_summaries(vec![s1, s2, s4]);

        let report = format_report(&run);

        assert!(report.contains("Sweep Tables"));
        assert!(report.contains("Parameter: client_count"));
        assert!(report.contains("plateau: first client_count"));
    }
}
