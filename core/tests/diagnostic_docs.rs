//! Every catalog code has exactly one cookbook page, and every page a code.

use cntryl_stress::diagnostics::DIAGNOSTIC_CATALOG;
use std::collections::BTreeSet;
use std::path::PathBuf;

fn docs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs")
}

#[test]
fn diagnostic_pages_match_the_catalog_one_to_one() {
    let pages = std::fs::read_dir(docs_dir().join("diagnostics"))
        .expect("docs/diagnostics exists")
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".md").map(str::to_string))
        .collect::<BTreeSet<_>>();
    let codes = DIAGNOSTIC_CATALOG
        .iter()
        .map(|info| info.code.to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        pages, codes,
        "docs/diagnostics pages must match DIAGNOSTIC_CATALOG"
    );
}

#[test]
fn each_page_is_titled_with_its_code_and_explains_allowing_it() {
    for info in DIAGNOSTIC_CATALOG {
        let path = docs_dir().join(info.docs_anchor);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let title = text.lines().next().unwrap_or_default();
        assert_eq!(title, format!("# `{}`", info.code), "{}", path.display());
        assert!(
            text.contains("--allow-code"),
            "{} must explain how to allow the code",
            path.display()
        );
    }
}
