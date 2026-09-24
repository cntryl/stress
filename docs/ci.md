# Running cntryl-stress in CI

This page is a copy-paste GitHub Actions setup for a performance gate:

1. Every push to `main` runs the suite and saves a baseline, keeping the five
   most recent runs in the Actions cache.
2. Every pull request restores that cache, runs against the pooled baseline,
   re-runs any regressed benchmark up to twice before failing, and posts a
   `cargo stress compare` table as a PR comment.

The template needs the 0.5 CLI (`cargo stress compare`, `--baseline-runs`, and
`--confirm-regressions`).

## Shared runners are noisy

GitHub-hosted runners are shared VMs. The same commit can vary by 5-20% between
runs because of neighbours, CPU model changes, and thermal state. Tight
regression thresholds on these hosts produce false failures, and false
failures teach people to ignore the gate.

On shared runners, prefer:

- **Diagnostic gating**, which does not depend on host speed:
  `--deny-code likely_optimized_away`, `--deny-code correctness_failure`,
  `--deny-diagnostics error`, and explicit `max_allocs_per_op` /
  `max_bytes_per_op` budgets. Allocation counts are deterministic, so they are
  the most reliable budget on any host. See the
  [diagnostic cookbook](diagnostics/) for every code.
- **Pooled baselines and confirmation** (`--baseline-runs 5`,
  `--confirm-regressions 2`) so a single slow run neither poisons the baseline
  nor fails a PR. Both are described in the README under
  [Noise-aware gating](../README.md#noise-aware-gating).
- **Generous timing thresholds** (the default 5%, or `--threshold-percent 10`)
  and treat the comparison comment as information for reviewers.

Tight timing thresholds belong on a dedicated, quiet, self-hosted runner.
Baselines are only compared on a compatible environment (same CPU model and
build inputs); a baseline from a different runner image is reported as
incompatible rather than as a regression.

## Workflow

Save as `.github/workflows/stress.yml`. Replace the `STRESS_VERSION` value
with the release you depend on. All actions are pinned to full commit SHAs.

```yaml
name: stress

on:
  push:
    branches: [main]
  pull_request:

# Default for every job: read-only token.
permissions:
  contents: read

concurrency:
  group: stress-${{ github.ref }}
  cancel-in-progress: true

env:
  CARGO_TERM_COLOR: always
  STRESS_VERSION: "0.5.0"
  # Paths passed to `cargo stress` are resolved from the repository root.
  STRESS_BASELINE_DIR: target/stress/baselines
  STRESS_OUTPUT_DIR: target/stress

jobs:
  baseline:
    if: github.event_name == 'push'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803 # v6.1.0
        with:
          persist-credentials: false

      - uses: dtolnay/rust-toolchain@02cb101ec7c40f2c49e1d9714d64511d8e1b74de # master
        with:
          toolchain: stable

      - name: Install cargo-stress
        run: cargo install cntryl-stress --locked --features cli --version "$STRESS_VERSION"

      # Restore the previous baselines so --baseline-runs can keep a history.
      - uses: actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: target/stress/baselines
          key: stress-baselines-${{ github.ref_name }}-${{ github.run_id }}
          restore-keys: stress-baselines-${{ github.ref_name }}-

      - name: Run and save baseline
        run: >-
          cargo stress --profile release
          --baseline-dir "$STRESS_BASELINE_DIR" --output-dir "$STRESS_OUTPUT_DIR"
          --save-baseline --baseline-runs 5

      # Cache entries are immutable, so each run saves under a new key and
      # the next run restores the newest one through the prefix above.
      - uses: actions/cache/save@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: target/stress/baselines
          key: stress-baselines-${{ github.ref_name }}-${{ github.run_id }}

  pr:
    if: github.event_name == 'pull_request'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803 # v6.1.0
        with:
          persist-credentials: false

      - uses: dtolnay/rust-toolchain@02cb101ec7c40f2c49e1d9714d64511d8e1b74de # master
        with:
          toolchain: stable

      - name: Install cargo-stress
        run: cargo install cntryl-stress --locked --features cli --version "$STRESS_VERSION"

      # Restore only: pull requests never write the base branch's baselines.
      - id: baselines
        uses: actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: target/stress/baselines
          key: stress-baselines-${{ github.base_ref }}-${{ github.run_id }}
          restore-keys: stress-baselines-${{ github.base_ref }}-

      # Annotations and the job summary are written automatically because
      # GITHUB_ACTIONS is set. This step is the gate.
      - name: Run against baseline
        shell: bash
        env:
          HAVE_BASELINE: ${{ steps.baselines.outputs.cache-matched-key != '' }}
        run: |
          compare=()
          if [ "$HAVE_BASELINE" = true ]; then
            compare=(--baseline latest --baseline-runs 5 --confirm-regressions 2)
          else
            echo "::notice::No baseline cache for the base branch yet; running without a comparison."
          fi
          cargo stress --profile release \
            --baseline-dir "$STRESS_BASELINE_DIR" --output-dir "$STRESS_OUTPUT_DIR" \
            --deny-code likely_optimized_away "${compare[@]}"

      - name: Compare with baseline
        if: ${{ !cancelled() }}
        shell: bash
        run: |
          {
            echo "## cntryl-stress comparison"
            found=0
            for candidate in "$STRESS_OUTPUT_DIR"/*/*/latest.json; do
              [ -f "$candidate" ] || continue
              suite_dir=$(dirname "$candidate")
              suite=$(basename "$suite_dir")
              package=$(basename "$(dirname "$suite_dir")")
              [ "$package" = baselines ] && continue
              baseline="$STRESS_BASELINE_DIR/$package/latest/$suite.json"
              echo
              echo "### $package / $suite"
              if [ -f "$baseline" ]; then
                found=1
                # Exit 1 (regression) and 2 (incompatible or incomplete) are
                # reported in the table; the gate is the previous step.
                cargo stress compare "$baseline" "$candidate" --format md || true
              else
                echo "No baseline yet; it is created by the next push to the base branch."
              fi
            done
            [ "$found" = 1 ] || echo "No saved baseline was restored from the cache."
          } > stress-compare.md

      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        if: ${{ !cancelled() }}
        with:
          name: stress-compare
          path: stress-compare.md
          retention-days: 7

  comment:
    needs: pr
    # Fork PRs get a read-only token, so only comment for same-repository PRs.
    if: >-
      ${{ !cancelled() && github.event_name == 'pull_request'
      && github.event.pull_request.head.repo.full_name == github.repository }}
    runs-on: ubuntu-latest
    # The only job with write access. It never checks out or runs PR code.
    permissions:
      pull-requests: write
    steps:
      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
        with:
          name: stress-compare

      - name: Post comparison
        env:
          GH_TOKEN: ${{ github.token }}
          GH_REPO: ${{ github.repository }}
          PR_NUMBER: ${{ github.event.pull_request.number }}
        run: gh pr comment "$PR_NUMBER" --body-file - < stress-compare.md
```

## Why it is shaped this way

- **Least privilege.** The workflow token is `contents: read` by default. Only
  the `comment` job gets `pull-requests: write`, and it only downloads the
  markdown produced by the `pr` job; it never checks out or executes PR code.
- **No `pull_request_target`.** PR code always runs with the unprivileged
  `pull_request` token. For fork PRs the `comment` job is skipped (GitHub
  gives forks a read-only token); the table is still in the uploaded
  `stress-compare` artifact and the job summary, and annotations still appear
  on the diff.
- **Cache keyed on the base branch.** Only pushes to `main` save
  `target/stress/baselines`, under `stress-baselines-main-<run id>`. Pull
  requests restore the newest `stress-baselines-<base branch>-*` entry and
  never save, so a PR cannot change the baseline other PRs compare against.
- **`--baseline-runs 5`** keeps the five newest saved runs per suite. On the PR
  side, `--baseline latest --baseline-runs 5` pools their raw samples, so one
  slow `main` run does not move the baseline.
- **`--confirm-regressions 2`** re-runs only regressed benchmarks, up to twice,
  and fails only if the regression persists in the pooled samples. Every
  attempt is recorded in the artifact's `confirmation_runs`.
- **`--deny-code likely_optimized_away`** is an example of a host-independent
  diagnostic gate. Add the codes you care about; `cargo stress explain --list`
  prints them all.
- **The comparison never gates by itself.** `cargo stress compare` exits `1`
  on a gating regression and `2` on an incompatible environment, invalid
  input, or incomplete coverage. The template records that in the comment and
  leaves the verdict to the `Run against baseline` step, which applies
  confirmation.

## Paths

`cargo stress` resolves relative `--output-dir` and `--baseline-dir` values
against the directory it runs in, and adds the package name as a namespace:

| File | Path |
| --- | --- |
| Current run | `target/stress/<package>/<suite>/latest.json` |
| Saved baseline | `target/stress/baselines/<package>/latest/<suite>.json` |
| Saved history | `target/stress/baselines/<package>/<timestamp>/<suite>.json` |

## Opting out and debugging

- `STRESS_GITHUB=0` turns off annotations and the step summary.
- `cargo stress --print-config` shows every resolved setting and where it came
  from.
- Before a push to the base branch has saved a cache, the PR job runs without
  `--baseline` and the comment says there is no baseline yet.
- `--baseline latest` fails the run when a selected suite has no saved
  baseline. Land a new bench target on `main` first, or expect that PR's gate
  to fail until it does.
