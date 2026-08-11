# Contributing to cntryl-stress

Thank you for your interest in contributing to cntryl-stress. The project
optimizes for low-ceremony benchmark authoring, trustworthy artifacts, and
clear diagnostics. Changes should preserve that workflow unless the discussion
explicitly chooses a new direction.

## Organization-wide contribution expectations

cntryl-stress follows the [cntryl contribution standards](https://github.com/cntryl/.github/blob/main/CONTRIBUTING.md).
AI-assisted contributions are welcome, but material AI or generative-tool
assistance must be disclosed. Explain how you encountered the problem or need,
why the contribution matters, what the tool assisted, and how its output was
reviewed and validated. You remain responsible for understanding, testing,
explaining, and revising the complete submission.

Substantial work requires a linked issue and maintainer agreement on scope before
implementation. Maintainers may close context-free, duplicate, speculative,
misleading, mass-produced, or unsupported submissions to protect review capacity.

## Code of Conduct

Be respectful and inclusive. We're building a community where everyone feels welcome to contribute.

## Getting Started

### Prerequisites

- Rust 1.85+ (the declared minimum supported Rust version)
- Git

### Development Setup

1. Fork the repository on GitHub
2. Clone your fork locally:
   ```bash
   git clone https://github.com/YOUR_USERNAME/stress.git
   cd stress
   ```

3. Add upstream remote:
   ```bash
   git remote add upstream https://github.com/cntryl/stress.git
   ```

4. Create a feature branch:
   ```bash
   git checkout -b feature/your-feature-name
   ```

## Development Workflow

### Building

```bash
cargo build --locked --workspace --all-targets --all-features
```

### Testing

Run all tests:
```bash
cargo test --locked --workspace --all-targets --all-features
```

Run tests in release mode:
```bash
cargo test --locked --workspace --all-targets --all-features --release
```

### Code Quality

We enforce high code standards using:

```bash
# Format code
cargo fmt --all

# Check formatting
cargo fmt --all -- --check

# Lint with clippy
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -D clippy::pedantic

# Check documentation
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
```

All of these must pass before submitting a PR.

### Running Benchmarks

Run the demo benchmarks when changing benchmark authoring, reporting, or
artifact behavior:

```bash
cargo bench --locked -p cntryl-stress-demo --bench stress-demo1
cargo bench --locked -p cntryl-stress-demo --bench stress-demo2
```

Reporting and baseline-publication changes must preserve the six public artifact
files, immutable history, cross-process serialization and recovery, and final
receipt ordering. Add focused regressions for publication failures and
interrupted transactions when changing those paths.

When changing packaging or proc-macro tests, also package the macros crate and
run tests from Cargo's normalized extracted archive. Workspace-only trybuild
fixtures must not make the published crate depend on an unpublished sibling:

```bash
cargo package --locked -p cntryl-stress-macros --allow-dirty
macro_package="$(find target/package -maxdepth 1 -type d -name 'cntryl-stress-macros-*' -print -quit)"
cargo test --locked --manifest-path "$macro_package/Cargo.toml" --all-targets
cargo test --locked --manifest-path "$macro_package/Cargo.toml" --doc
```

## Project Structure

- **core/** - Main library (`cntryl-stress`)
  - `src/harness.rs` - Test discovery and execution
  - `src/runner.rs` - StressRunner API
  - `src/reporting.rs` - Output reporters and human formatting
  - `src/artifact.rs` - Artifact schema, summaries, diagnostics, and comparisons
  - `src/context.rs` - Benchmark authoring context
  - `src/config.rs` - Configuration and CLI parsing
  - `src/bin/cargo-stress.rs` - Optional `cargo stress` wrapper behind the `cli` feature

- **macros/** - Proc macros (`cntryl-stress-macros`)
  - `src/lib.rs` - `#[stress]` and `stress_main!()` macros

- **demo/** - Demo benchmarks (not published)
  - `benches/` - Example benchmark files

## Making Changes

### For Bug Fixes

1. Create an issue describing the bug (if one doesn't exist)
2. Create a branch: `git checkout -b fix/issue-description`
3. Make your changes
4. Add tests if applicable
5. Ensure all checks pass:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -D clippy::pedantic
   cargo test --locked --workspace --all-targets --all-features
   RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
   ```
6. Create a pull request with a clear description

### For Features

1. Discuss in an issue first - we want to ensure API changes align with the project goals
2. Create a branch: `git checkout -b feature/your-feature`
3. Implement the feature
4. Add tests for new functionality
5. Update documentation if it affects the public API
6. Ensure the full check set passes
7. Create a pull request

### For Documentation

1. Create a branch: `git checkout -b docs/what-you-are-improving`
2. Make documentation changes in `docs/` or README
3. Create a pull request

## Pull Request Process

1. Update documentation for any user-facing changes
2. Add tests for new functionality
3. Ensure all checks pass:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -D clippy::pedantic
   cargo test --locked --workspace --all-targets --all-features
   RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
   ```
4. Write a clear PR description:
   - What problem does it solve?
   - How does it solve it?
   - Any breaking changes?
   - Any related issues?

5. Request review from maintainers
6. Address feedback and push updates

## Commit Messages

Use clear, descriptive commit messages:

- Good: `Add duration format helper to reporter`
- Good: `Fix unicode handling in console output`
- Avoid: `fix stuff`
- Avoid: `wip`

## Semantic Versioning

This project follows [Semantic Versioning](https://semver.org/):

- **MAJOR** - Breaking API changes
- **MINOR** - New features within the current major line
- **PATCH** - Bug fixes within the current major line

## Licensing

By contributing, you agree that your contributions will be licensed under the same Apache-2.0 license as the project.

## Questions?

- Read existing issues and PRs
- Check the documentation in `docs/`
- Create a discussion issue if you have questions

Thank you for contributing.
