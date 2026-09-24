//! `cargo stress init`: scaffold a stress bench target in a Cargo package.
//!
//! The manifest is edited textually so existing formatting and comments are
//! preserved; every edit is re-parsed and checked to change nothing except
//! the added dependency and `[[bench]]` entry before anything is written.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Published crate name the scaffold depends on.
const CRATE_NAME: &str = "cntryl-stress";

/// Scaffolded benchmark source. `cntryl_stress` is rewritten to the crate
/// identifier the package already uses when the dependency is renamed.
pub(crate) const SCAFFOLD: &str = r#"//! Stress benchmarks for this package.
//!
//! Run with `cargo stress` (or `cargo bench --bench __BENCH__`). Results are
//! written under `target/stress/`. See https://docs.rs/cntryl-stress.
use cntryl_stress::{black_box, stress, stress_main, StressContext};

// Count allocations so allocation budgets and diagnostics work.
cntryl_stress::stress_allocator!();

/// A tier-2 benchmark: each sample times a fixed batch of operations.
#[stress(tier = 2)]
fn sum_squares(ctx: &mut StressContext) {
    // Build fixtures outside the measured closure.
    let input: Vec<u64> = (0..1_024).collect();
    ctx.parameter("len", input.len());

    ctx.benchmark("sum of squares")
        .operations_per_sample(10_000)
        .measure(|| black_box(&input).iter().map(|value| value * value).sum::<u64>());
}

stress_main!();
"#;

/// Default dependency requirement: the running tool's `major.minor`.
pub(crate) fn default_dependency_spec() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut parts = version.split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) => format!("\"{major}.{minor}\""),
        _ => format!("\"{version}\""),
    }
}

/// Validate a `--name` value: a plain Cargo target name.
pub(crate) fn validate_bench_name(name: &str) -> Result<(), String> {
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(format!(
            "invalid bench name `{name}`: use ASCII letters, digits, `-`, or `_`"
        ))
    }
}

/// Result of planning a manifest edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManifestEdit {
    /// The new manifest text (equal to the input when nothing changed).
    pub text: String,
    pub added_dependency: bool,
    pub added_bench: bool,
    /// Bench source path relative to the manifest directory.
    pub bench_path: String,
    /// Rust identifier of the stress crate within this package.
    pub crate_ident: String,
}

/// Find an unconditional, non-optional dependency on the stress crate that
/// benches can use, returning its Rust identifier.
fn find_stress_dependency(manifest: &toml::Table) -> Option<String> {
    ["dev-dependencies", "dependencies"]
        .into_iter()
        .filter_map(|key| manifest.get(key).and_then(toml::Value::as_table))
        .find_map(|table| {
            table.iter().find_map(|(key, value)| {
                let spec = value.as_table();
                let optional = spec
                    .and_then(|spec| spec.get("optional"))
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false);
                let package = spec
                    .and_then(|spec| spec.get("package"))
                    .and_then(toml::Value::as_str)
                    .unwrap_or(key);
                (package == CRATE_NAME && !optional).then(|| key.replace('-', "_"))
            })
        })
}

/// Name of an existing target (other than `bench`) whose source is `path`.
fn target_using_path(manifest: &toml::Table, path: &str, bench: &str) -> Option<String> {
    let normalize = |value: &str| {
        value
            .replace('\\', "/")
            .trim_start_matches("./")
            .to_string()
    };
    let wanted = normalize(path);
    ["bench", "test", "example", "bin"]
        .into_iter()
        .find_map(|kind| {
            manifest
                .get(kind)
                .and_then(toml::Value::as_array)?
                .iter()
                .filter_map(toml::Value::as_table)
                .find_map(|entry| {
                    let name = entry.get("name").and_then(toml::Value::as_str)?;
                    let used = entry.get("path").and_then(toml::Value::as_str)?;
                    (normalize(used) == wanted && !(kind == "bench" && name == bench))
                        .then(|| format!("[[{kind}]] `{name}`"))
                })
        })
}

fn manual_instructions(bench: &str, dependency_spec: &str) -> String {
    format!(
        "add these entries to Cargo.toml by hand:\n\n[dev-dependencies]\n{CRATE_NAME} = {dependency_spec}\n\n[[bench]]\nname = \"{bench}\"\npath = \"benches/{bench}.rs\"\nharness = false\n"
    )
}

/// Plan the manifest edit that adds the stress dev-dependency and bench
/// target when they are missing. Never returns text that parses differently
/// from the input except for those two additions.
pub(crate) fn edit_manifest(
    source: &str,
    bench: &str,
    dependency_spec: &str,
) -> Result<ManifestEdit, String> {
    let original: toml::Table = source
        .parse()
        .map_err(|error| format!("Cargo.toml is not valid TOML: {error}"))?;
    if !original.contains_key("package") {
        return Err(
            "Cargo.toml has no [package] (virtual workspace manifest); pass -p <package> or --manifest-path <member>/Cargo.toml"
                .to_string(),
        );
    }
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut text = source.to_string();
    let existing_ident = find_stress_dependency(&original);
    let added_dependency = existing_ident.is_none();
    let crate_ident = existing_ident.unwrap_or_else(|| CRATE_NAME.replace('-', "_"));

    let existing_bench = original
        .get("bench")
        .and_then(toml::Value::as_array)
        .and_then(|benches| {
            benches
                .iter()
                .filter_map(toml::Value::as_table)
                .find(|entry| entry.get("name").and_then(toml::Value::as_str) == Some(bench))
        });
    let default_path = format!("benches/{bench}.rs");
    let bench_path = match existing_bench {
        Some(entry) => {
            if entry.get("harness").and_then(toml::Value::as_bool) != Some(false) {
                return Err(format!(
                    "[[bench]] `{bench}` already exists without `harness = false`; set it by hand or choose another --name"
                ));
            }
            entry
                .get("path")
                .and_then(toml::Value::as_str)
                .unwrap_or(&default_path)
                .to_string()
        }
        None => default_path.clone(),
    };
    let added_bench = existing_bench.is_none();
    if let Some(owner) = target_using_path(&original, &bench_path, bench) {
        return Err(format!(
            "{bench_path} is already the source of {owner}; choose another --name"
        ));
    }

    if added_dependency {
        let line = format!("{CRATE_NAME} = {dependency_spec}");
        let header = text.split_inclusive('\n').scan(0, |offset, raw| {
            let start = *offset;
            *offset += raw.len();
            Some((start + raw.len(), raw))
        });
        let insert_at = header
            .into_iter()
            .find(|(_, raw)| {
                let content = raw.split('#').next().unwrap_or_default().trim();
                content == "[dev-dependencies]"
            })
            .map(|(end, _)| end);
        if let Some(end) = insert_at {
            let mut addition = String::new();
            if !text[..end].ends_with('\n') {
                addition.push_str(newline);
            }
            addition.push_str(&line);
            addition.push_str(newline);
            text.insert_str(end, &addition);
        } else {
            ensure_trailing_newline(&mut text, newline);
            let _ = write!(text, "{newline}[dev-dependencies]{newline}{line}{newline}");
        }
    }
    if added_bench {
        ensure_trailing_newline(&mut text, newline);
        let _ = write!(
            text,
            "{newline}[[bench]]{newline}name = \"{bench}\"{newline}path = \"{bench_path}\"{newline}harness = false{newline}"
        );
    }

    verify_edit(&original, &text, bench, added_dependency, added_bench)
        .map_err(|reason| format!("{reason}; {}", manual_instructions(bench, dependency_spec)))?;
    Ok(ManifestEdit {
        text,
        added_dependency,
        added_bench,
        bench_path,
        crate_ident,
    })
}

fn ensure_trailing_newline(text: &mut String, newline: &str) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push_str(newline);
    }
}

/// Re-parse the edited manifest and require that removing the additions
/// yields exactly the original document.
fn verify_edit(
    original: &toml::Table,
    text: &str,
    bench: &str,
    added_dependency: bool,
    added_bench: bool,
) -> Result<(), String> {
    let mut edited: toml::Table = text
        .parse()
        .map_err(|_| "cannot safely edit this Cargo.toml automatically".to_string())?;
    let unsafe_edit = || "cannot safely edit this Cargo.toml automatically".to_string();
    if added_dependency {
        let deps = edited
            .get_mut("dev-dependencies")
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(unsafe_edit)?;
        deps.remove(CRATE_NAME).ok_or_else(unsafe_edit)?;
        if deps.is_empty() && !original.contains_key("dev-dependencies") {
            edited.remove("dev-dependencies");
        }
    }
    if added_bench {
        let benches = edited
            .get_mut("bench")
            .and_then(toml::Value::as_array_mut)
            .ok_or_else(unsafe_edit)?;
        let last = benches.pop().ok_or_else(unsafe_edit)?;
        if last.get("name").and_then(toml::Value::as_str) != Some(bench) {
            return Err(unsafe_edit());
        }
        if benches.is_empty() && !original.contains_key("bench") {
            edited.remove("bench");
        }
    }
    if &edited == original {
        Ok(())
    } else {
        Err(unsafe_edit())
    }
}

/// What `init` did, for reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitReport {
    pub bench_file: PathBuf,
    pub overwrote_bench_file: bool,
    pub added_dependency: bool,
    pub added_bench: bool,
}

/// Scaffold `benches/<name>.rs` and update the manifest. Plans every change
/// before writing; refuses to overwrite an existing bench file unless
/// `force` is set.
pub(crate) fn init_package(
    manifest_path: &Path,
    bench: &str,
    force: bool,
    dependency_spec: &str,
) -> Result<InitReport, String> {
    validate_bench_name(bench)?;
    let source = fs::read_to_string(manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let edit = edit_manifest(&source, bench, dependency_spec)?;
    let package_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let bench_file = package_dir.join(&edit.bench_path);
    let exists = bench_file.exists();
    if exists && !force {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            bench_file.display()
        ));
    }
    let contents = SCAFFOLD
        .replace("cntryl_stress", &edit.crate_ident)
        .replace("__BENCH__", bench);
    if let Some(parent) = bench_file.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let previous = if exists {
        Some(
            fs::read(&bench_file)
                .map_err(|error| format!("cannot read {}: {error}", bench_file.display()))?,
        )
    } else {
        None
    };
    fs::write(&bench_file, contents)
        .map_err(|error| format!("cannot write {}: {error}", bench_file.display()))?;
    if edit.text != source {
        if let Err(error) = fs::write(manifest_path, &edit.text) {
            // Roll back so a failed init leaves the package as it was.
            let _ = fs::write(manifest_path, &source);
            let _ = match &previous {
                Some(bytes) => fs::write(&bench_file, bytes),
                None => fs::remove_file(&bench_file),
            };
            return Err(format!("cannot write {}: {error}", manifest_path.display()));
        }
    }
    Ok(InitReport {
        bench_file,
        overwrote_bench_file: exists,
        added_dependency: edit.added_dependency,
        added_bench: edit.added_bench,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PKG: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

    #[test]
    fn appends_dependency_and_bench_when_missing() {
        let edit = edit_manifest(PKG, "stress", "\"0.4\"").unwrap();
        assert!(edit.added_dependency && edit.added_bench);
        assert_eq!(
            edit.text,
            format!("{PKG}\n[dev-dependencies]\ncntryl-stress = \"0.4\"\n\n[[bench]]\nname = \"stress\"\npath = \"benches/stress.rs\"\nharness = false\n")
        );
        assert_eq!(edit.bench_path, "benches/stress.rs");
    }

    #[test]
    fn inserts_under_existing_dev_dependencies_preserving_comments() {
        let source = format!(
            "{PKG}\n# keep me\n[dev-dependencies] # tests\nserde = \"1\"\n\n[features]\nx = []\n"
        );
        let edit = edit_manifest(&source, "stress", "\"0.4\"").unwrap();
        assert!(edit
            .text
            .contains("[dev-dependencies] # tests\ncntryl-stress = \"0.4\"\nserde = \"1\"\n"));
        assert!(edit.text.contains("# keep me\n"));
        assert!(edit.text.ends_with(
            "[[bench]]\nname = \"stress\"\npath = \"benches/stress.rs\"\nharness = false\n"
        ));
    }

    #[test]
    fn idempotent_when_everything_present() {
        let source = format!("{PKG}\n[dev-dependencies]\ncntryl-stress = \"0.4\"\n\n[[bench]]\nname = \"stress\"\nharness = false\n");
        let edit = edit_manifest(&source, "stress", "\"0.4\"").unwrap();
        assert_eq!(edit.text, source);
        assert!(!edit.added_dependency && !edit.added_bench);
    }

    #[test]
    fn renamed_or_normal_dependency_is_reused() {
        let source = format!("{PKG}\n[dependencies]\nstress-kit = {{ package = \"cntryl-stress\", version = \"0.4\" }}\n");
        let edit = edit_manifest(&source, "stress", "\"0.4\"").unwrap();
        assert!(!edit.added_dependency);
        assert_eq!(edit.crate_ident, "stress_kit");
    }

    #[test]
    fn existing_bench_path_is_respected_and_harness_required() {
        let source =
            format!("{PKG}\n[[bench]]\nname = \"stress\"\npath = \"perf/s.rs\"\nharness = false\n");
        assert_eq!(
            edit_manifest(&source, "stress", "\"0.4\"")
                .unwrap()
                .bench_path,
            "perf/s.rs"
        );
        let harnessed = format!("{PKG}\n[[bench]]\nname = \"stress\"\n");
        assert!(edit_manifest(&harnessed, "stress", "\"0.4\"")
            .unwrap_err()
            .contains("harness = false"));
    }

    #[test]
    fn virtual_manifest_is_rejected() {
        let error = edit_manifest("[workspace]\nmembers = []\n", "stress", "\"0.4\"").unwrap_err();
        assert!(error.contains("-p"));
    }

    #[test]
    fn inline_dev_dependencies_fall_back_to_manual_instructions() {
        let source = format!("dev-dependencies = {{ serde = \"1\" }}\n{PKG}");
        let error = edit_manifest(&source, "stress", "\"0.4\"").unwrap_err();
        assert!(error.contains("by hand"), "{error}");
    }

    #[test]
    fn crlf_and_missing_trailing_newline_are_handled() {
        let source = "[package]\r\nname = \"demo\"\r\nversion = \"0.1.0\"";
        let edit = edit_manifest(source, "stress", "\"0.4\"").unwrap();
        assert!(edit
            .text
            .contains("\r\n[dev-dependencies]\r\ncntryl-stress = \"0.4\"\r\n"));
        assert!(!edit.text.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn target_specific_or_optional_dependencies_do_not_count() {
        for deps in [
            "[target.'cfg(unix)'.dev-dependencies]\ncntryl-stress = \"0.4\"\n",
            "[dependencies]\ncntryl-stress = { version = \"0.4\", optional = true }\n",
        ] {
            let source = format!("{PKG}\n{deps}");
            let edit = edit_manifest(&source, "stress", "\"0.4\"").unwrap();
            assert!(edit.added_dependency, "{deps}");
            assert_eq!(edit.crate_ident, "cntryl_stress");
        }
    }

    #[test]
    fn new_bench_path_used_by_another_target_is_rejected() {
        for kind in ["bench", "test", "example", "bin"] {
            let source = format!(
                "{PKG}\n[[{kind}]]\nname = \"other\"\npath = \"benches/stress.rs\"\nharness = false\n"
            );
            let error = edit_manifest(&source, "stress", "\"0.4\"").unwrap_err();
            assert!(error.contains("other"), "{kind}: {error}");
        }
    }

    #[test]
    fn bench_names_are_validated() {
        assert!(validate_bench_name("stress-io_2").is_ok());
        for bad in ["", "../x", "a b", "x.rs"] {
            assert!(validate_bench_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn default_spec_is_major_minor() {
        let spec = default_dependency_spec();
        assert_eq!(spec.matches('.').count(), 1, "{spec}");
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cargo-stress-init-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn init_refuses_to_overwrite_without_force() {
        let dir = Dir::new("force");
        let manifest = dir.0.join("Cargo.toml");
        fs::write(&manifest, PKG).unwrap();
        fs::create_dir_all(dir.0.join("benches")).unwrap();
        fs::write(dir.0.join("benches/stress.rs"), "mine").unwrap();
        let error = init_package(&manifest, "stress", false, "\"0.4\"").unwrap_err();
        assert!(error.contains("--force"));
        assert_eq!(
            fs::read_to_string(dir.0.join("benches/stress.rs")).unwrap(),
            "mine"
        );
        assert_eq!(
            fs::read_to_string(&manifest).unwrap(),
            PKG,
            "manifest untouched on refusal"
        );

        let report = init_package(&manifest, "stress", true, "\"0.4\"").unwrap();
        assert!(report.overwrote_bench_file);
        assert!(fs::read_to_string(dir.0.join("benches/stress.rs"))
            .unwrap()
            .contains("stress_main!()"));
    }

    #[cfg(unix)]
    #[test]
    fn failed_manifest_write_restores_the_bench_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir::new("rollback");
        let manifest = dir.0.join("Cargo.toml");
        fs::write(&manifest, PKG).unwrap();
        fs::create_dir_all(dir.0.join("benches")).unwrap();
        fs::write(dir.0.join("benches/stress.rs"), "mine").unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&manifest, fs::Permissions::from_mode(0o444)).unwrap();
        if fs::OpenOptions::new().append(true).open(&manifest).is_ok() {
            // Running as a privileged user: permissions cannot force a failure.
            fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let result = init_package(&manifest, "stress", true, "\"0.4\"");
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(dir.0.join("benches/stress.rs")).unwrap(),
            "mine"
        );
        assert_eq!(fs::read_to_string(&manifest).unwrap(), PKG);

        fs::remove_file(dir.0.join("benches/stress.rs")).unwrap();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o555)).unwrap();
        let result = init_package(&manifest, "stress", false, "\"0.4\"");
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert!(
            !dir.0.join("benches/stress.rs").exists(),
            "new bench file removed"
        );
    }

    #[test]
    fn init_uses_custom_name_and_renamed_crate() {
        let dir = Dir::new("name");
        let manifest = dir.0.join("Cargo.toml");
        fs::write(
            &manifest,
            format!("{PKG}\n[dev-dependencies]\nkit = {{ package = \"cntryl-stress\", version = \"0.4\" }}\n"),
        )
        .unwrap();
        let report = init_package(&manifest, "io-perf", false, "\"0.4\"").unwrap();
        assert_eq!(report.bench_file, dir.0.join("benches/io-perf.rs"));
        assert!(!report.added_dependency && report.added_bench);
        let source = fs::read_to_string(&report.bench_file).unwrap();
        assert!(source.contains("use kit::{") && source.contains("kit::stress_allocator!()"));
        assert!(!source.contains("cntryl_stress"));
        assert!(source.contains("--bench io-perf"));
    }
}
