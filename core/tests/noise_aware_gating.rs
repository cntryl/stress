//! End-to-end check of multi-run baselines and confirm-on-regression.
//!
//! Runs without the libtest harness so the re-executed child owns stdout.
//! The benchmark reports fixed external durations, so classifications do not
//! depend on host speed: the first `CHILD_SLOW` invocations take 10.6ms and
//! later ones 10ms.

use cntryl_stress::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const CHILD_ENV: &str = "CNTRYL_STRESS_NOISE_CHILD";
const SLOW_ENV: &str = "CNTRYL_STRESS_NOISE_SLOW";

static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);

#[stress(tier = 2)]
fn noise_probe(ctx: &mut StressContext) {
    let slow = std::env::var(SLOW_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let invocation = INVOCATIONS.fetch_add(1, Ordering::Relaxed);
    let elapsed = if invocation < slow {
        Duration::from_micros(10_600)
    } else {
        Duration::from_millis(10)
    };
    ctx.record_external("work", elapsed, 1_000);
}

fn main() {
    if std::env::var_os(CHILD_ENV).is_some() {
        cntryl_stress::__private::stress_binary_main();
        return;
    }
    let scratch = scratch_dir();
    let baselines = scratch.join("baselines");

    for _ in 0..3 {
        let saved = run_child(&scratch, 0, &["--save-baseline", "--baseline-runs", "2"]);
        assert!(saved.status.success(), "{}", stderr(&saved));
    }
    let suite_dir_runs = std::fs::read_dir(&baselines)
        .expect("baseline dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != "latest")
        .count();
    assert_eq!(
        suite_dir_runs, 2,
        "--baseline-runs 2 retains two saved runs"
    );

    let flaky = run_child(
        &scratch,
        10,
        &[
            "--baseline",
            "latest",
            "--baseline-runs",
            "2",
            "--confirm-regressions",
            "2",
            // Confirmation re-runs share the isolated-worker timeout path.
            "--timeout-secs",
            "60",
        ],
    );
    let run = parse(&flaky);
    if run["environment"]["cpu_model"] == "unknown" {
        // Baselines cannot be compatible without a known CPU model.
        println!("noise_aware_gating: skipped (CPU model unavailable)");
        let _ = std::fs::remove_dir_all(&scratch);
        return;
    }
    assert!(flaky.status.success(), "{}", stderr(&flaky));
    assert_eq!(run["metadata"]["baseline_runs_pooled"], "2");
    let attempts = run["confirmation_runs"].as_array().expect("attempts");
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    assert_eq!(attempts[0]["regressions_after"], serde_json::json!([]));
    assert!(stderr(&flaky).contains("confirm"), "{}", stderr(&flaky));

    let persistent = run_child(
        &scratch,
        usize::MAX,
        &["--baseline", "latest", "--confirm-regressions", "2"],
    );
    assert!(!persistent.status.success(), "a true regression must fail");
    let run = parse(&persistent);
    assert_eq!(
        run["confirmation_runs"].as_array().expect("attempts").len(),
        2
    );

    let unconfirmed = run_child(&scratch, 10, &["--baseline", "latest"]);
    assert!(
        !unconfirmed.status.success(),
        "without confirmation it fails"
    );
    assert!(parse(&unconfirmed).get("confirmation_runs").is_none());

    let _ = std::fs::remove_dir_all(&scratch);
    println!("noise_aware_gating: ok");
}

fn parse(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be JSON ({error}):\n{stdout}\n{}",
            stderr(output)
        )
    })
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn run_child(scratch: &Path, slow: usize, extra: &[&str]) -> std::process::Output {
    let mut command = Command::new(std::env::current_exe().expect("current exe"));
    command
        .args([
            "--json",
            "--samples",
            "10",
            "--warmup-samples",
            "0",
            "--cooldown-samples",
            "0",
            "--output-dir",
        ])
        .arg(scratch.join("out"))
        .arg("--baseline-dir")
        .arg(scratch.join("baselines"))
        .args(extra)
        .env(CHILD_ENV, "1")
        .env(SLOW_ENV, slow.to_string())
        .env("STRESS_SUITE", "noise-gating")
        .env("STRESS_FAIL_ON_REGRESSION", "true")
        .env("STRESS_GITHUB", "0");
    for key in [
        "STRESS_BASELINE",
        "STRESS_SAVE_BASELINE",
        "STRESS_BASELINE_RUNS",
        "STRESS_CONFIRM_REGRESSIONS",
        "STRESS_PROFILE",
        "STRESS_FILTER",
        "STRESS_WORKLOAD",
        "STRESS_TIER",
        "STRESS_SAMPLES",
        "STRESS_THRESHOLD",
        "STRESS_ARTIFACT_NAMESPACE",
    ] {
        command.env_remove(key);
    }
    command.output().expect("run child")
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stress-noise-gating-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}
