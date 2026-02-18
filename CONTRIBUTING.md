# Contributing to cntryl-stress

Thank you for your interest in contributing to cntryl-stress! This document provides guidelines and instructions for contributing.

## Code of Conduct

Be respectful and inclusive. We're building a community where everyone feels welcome to contribute.

## Getting Started

### Prerequisites

- Rust 1.70+ (uses `rustup update stable`)
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
cargo build
```

### Testing

Run all tests:
```bash
cargo test --all
```

Run tests in release mode:
```bash
cargo test --all --release
```

### Code Quality

We enforce high code standards using:

```bash
# Format code
cargo fmt --all

# Check formatting
cargo fmt --all -- --check

# Lint with clippy
cargo clippy --all-targets -- -D warnings

# Check documentation
cargo doc --all --no-deps
```

All of these must pass before submitting a PR.

### Running Benchmarks

Test the benchmarks to ensure they work correctly:

```bash
cargo bench --bench stress-demo1
cargo bench --bench stress-demo2
```

## Project Structure

- **core/** - Main library (`cntryl-stress`)
  - `src/harness.rs` - Test discovery and execution
  - `src/runner.rs` - BenchRunner API
  - `src/report.rs` - Output reporters (Console, JSON)
  - `src/result.rs` - Result data structures
  - `src/config.rs` - Configuration and CLI parsing

- **macros/** - Proc macros (`cntryl-stress-macros`)
  - `src/lib.rs` - `#[stress_test]` and `stress_main!()` macros

- **demo/** - Demo benchmarks (not published)
  - `benches/` - Example benchmark files

## Making Changes

### For Bug Fixes

1. Create an issue describing the bug (if one doesn't exist)
2. Create a branch: `git checkout -b fix/issue-description`
3. Make your changes
4. Add tests if applicable
5. Ensure all checks pass: `cargo test && cargo fmt --all && cargo clippy --all-targets`
6. Create a pull request with a clear description

### For Features

1. Discuss in an issue first - we want to ensure API changes align with the project goals
2. Create a branch: `git checkout -b feature/your-feature`
3. Implement the feature
4. Add tests for new functionality
5. Update documentation if it affects the public API
6. Ensure all checks pass
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
   cargo fmt --all
   cargo clippy --all-targets -- -D warnings
   cargo test --all
   cargo doc --all --no-deps
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

- ✅ Good: `Add duration format helper to reporter`
- ✅ Good: `Fix unicode handling in console output`
- ❌ Bad: `fix stuff`
- ❌ Bad: `wip`

## Semantic Versioning

This project follows [Semantic Versioning](https://semver.org/):

- **MAJOR** - Breaking API changes
- **MINOR** - New features (backwards compatible)
- **PATCH** - Bug fixes (backwards compatible)

## Licensing

By contributing, you agree that your contributions will be licensed under the same Apache-2.0 license as the project.

## Questions?

- Read existing issues and PRs
- Check the documentation in `docs/`
- Create a discussion issue if you have questions

Thank you for contributing! 🎉
