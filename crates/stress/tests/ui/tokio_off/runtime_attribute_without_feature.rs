use cntryl_stress::{stress, StressContext};

#[stress(tier = 2, runtime = "tokio")]
async fn needs_tokio(ctx: &mut StressContext) {
    ctx.measure_async("noop", || async {}).await;
}

fn main() {}
