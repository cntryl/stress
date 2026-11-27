use stress_alias::{stress, StressContext};

#[stress(tier = 2, runtime = "async-std")]
async fn unknown_runtime(ctx: &mut StressContext) {
    ctx.measure_async("noop", || async {}).await;
}

#[stress(tier = 2, runtime = "tokio")]
fn sync_with_runtime(ctx: &mut StressContext) {
    ctx.measure("noop", || ());
}

fn main() {}
