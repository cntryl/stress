use stress_alias::stress;

#[stress(max_regression_pct = 101)]
fn invalid_budget(ctx: &mut stress_alias::StressContext) {
    ctx.measure("work", || {});
}

fn main() {}
