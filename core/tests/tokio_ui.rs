//! Compile-time checks for `runtime = "tokio"` with and without the feature.

#[test]
fn tokio_runtime_attribute_ui() {
    let tests = trybuild::TestCases::new();
    if cfg!(feature = "tokio") {
        tests.pass("tests/ui/tokio_on/*.rs");
    } else {
        tests.compile_fail("tests/ui/tokio_off/*.rs");
    }
}
