//! End-to-end check that GitHub Actions output never corrupts `--json` stdout.
//!
//! Runs without the libtest harness so the re-executed child owns stdout.
//! Environment is injected into the child process only; the parent's process
//! environment is never mutated.

use cntryl_stress::prelude::*;
use std::path::PathBuf;
use std::process::Command;

const CHILD_ENV: &str = "CNTRYL_STRESS_GITHUB_JSON_CHILD";

#[stress(tier = 2)]
fn github_json_probe(ctx: &mut StressContext) {
    ctx.measure("probe", || black_box(1_u64));
}

fn main() {
    if std::env::var_os(CHILD_ENV).is_some() {
        cntryl_stress::__private::stress_binary_main();
        return;
    }
    let scratch = scratch_dir();
    let summary_path = scratch.join("step-summary.md");
    std::fs::write(&summary_path, "earlier step\n").expect("seed step summary");

    let enabled = run_child(&scratch, &summary_path, None);
    let stdout = String::from_utf8(enabled.stdout).expect("utf8 stdout");
    let stderr = String::from_utf8_lossy(&enabled.stderr);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout must be JSON ({error}):\n{stdout}\n{stderr}"));
    assert_eq!(parsed["suite"], "github-json");
    assert!(
        !stdout.lines().any(|line| line.starts_with("::")),
        "annotations leaked to stdout"
    );
    let summary = std::fs::read_to_string(&summary_path).expect("read step summary");
    assert!(summary.starts_with("earlier step\n"), "{summary}");
    assert!(summary.contains("# github-json"), "{summary}");
    assert!(summary.contains("## Needs attention"), "{summary}");

    std::fs::write(&summary_path, "").expect("reset step summary");
    let opted_out = run_child(&scratch, &summary_path, Some("0"));
    let stdout = String::from_utf8(opted_out.stdout).expect("utf8 stdout");
    serde_json::from_str::<serde_json::Value>(&stdout).expect("opt-out stdout is JSON");
    let summary = std::fs::read_to_string(&summary_path).expect("read step summary");
    assert!(
        summary.is_empty(),
        "STRESS_GITHUB=0 must not write: {summary}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
    println!("github_actions_json: ok");
}

fn run_child(
    scratch: &std::path::Path,
    summary_path: &std::path::Path,
    stress_github: Option<&str>,
) -> std::process::Output {
    let mut command = Command::new(std::env::current_exe().expect("current exe"));
    command
        .args([
            "--json",
            "--samples",
            "2",
            "--warmup-samples",
            "0",
            "--cooldown-samples",
            "0",
            "--output-dir",
        ])
        .arg(scratch.join("out"))
        .env(CHILD_ENV, "1")
        .env("STRESS_SUITE", "github-json")
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_STEP_SUMMARY", summary_path)
        .env_remove("STRESS_BASELINE")
        .env_remove("STRESS_SAVE_BASELINE")
        .env_remove("STRESS_PROFILE")
        .env_remove("STRESS_FILTER")
        .env_remove("STRESS_WORKLOAD")
        .env_remove("STRESS_TIER")
        .env_remove("STRESS_SAMPLES");
    match stress_github {
        Some(value) => command.env("STRESS_GITHUB", value),
        None => command.env_remove("STRESS_GITHUB"),
    };
    command.output().expect("run child")
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stress-github-json-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}
