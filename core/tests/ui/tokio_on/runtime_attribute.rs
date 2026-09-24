use cntryl_stress::{stress, StressContext};

#[stress(tier = 2, runtime = "tokio")]
async fn current_thread(ctx: &mut StressContext) {
    ctx.measure_async("sleep", || async {
        cntryl_stress::tokio::time::sleep(std::time::Duration::from_micros(1)).await;
    })
    .await;
}

#[stress(tier = 2, runtime = "tokio-multi")]
async fn multi_thread(ctx: &mut StressContext) {
    ctx.measure_async("yield", || async {
        cntryl_stress::tokio::task::yield_now().await;
    })
    .await;
}

fn main() {}
