use stress_alias::prelude::*;

#[stress]
fn located(ctx: &mut StressContext) {
    ctx.measure("located", || black_box(1_u64));
}

fn main() {
    let entry = stress_alias::__private::STRESS_BENCHMARKS
        .iter()
        .find(|entry| entry.function_name == "located")
        .expect("registered benchmark");
    assert!(entry.file.ends_with("source_location.rs"), "{}", entry.file);
    assert_eq!(entry.line, 3);
}
