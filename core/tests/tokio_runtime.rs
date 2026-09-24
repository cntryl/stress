//! `#[stress(runtime = "tokio")]` drives async benchmarks on a tokio runtime.
#![cfg(feature = "tokio")]

use cntryl_stress::prelude::*;
use cntryl_stress::tokio;
use std::time::Duration;

#[stress(tier = 2, runtime = "tokio")]
async fn tokio_sleep(ctx: &mut StressContext) {
    ctx.measure_async("sleep", || async {
        tokio::time::sleep(Duration::from_millis(1)).await;
    })
    .await;
}

#[stress(tier = 2, runtime = "tokio-multi")]
async fn tokio_channel(ctx: &mut StressContext) {
    ctx.measure_async("channel round trip", || async {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u64>(1);
        let sender = tokio::spawn(async move { tx.send(7).await.expect("send") });
        let value = rx.recv().await.expect("recv");
        sender.await.expect("join");
        black_box(value)
    })
    .await;
}

fn config() -> StressRunnerConfig {
    StressRunnerConfig::new()
        .samples(3)
        .warmup_samples(1)
        .cooldown_samples(0)
        .operations_per_sample(2)
        .progress(false)
}

#[test]
fn tokio_timer_benchmark_completes() {
    let mut runner = StressRunner::with_config("tokio-suite", config());
    runner.reporters(Vec::new());
    runner.run("sleep", __stress_wrapper_tokio_sleep);
    let run = runner.finish();
    let ns = run.summaries[0].ns_per_op.as_ref().expect("measured");
    assert!(ns.median >= 1_000_000.0, "{ns:?}");
}

#[test]
fn tokio_multi_thread_channel_benchmark_completes() {
    let mut runner = StressRunner::with_config("tokio-suite", config());
    runner.reporters(Vec::new());
    runner.run("channel", __stress_wrapper_tokio_channel);
    let run = runner.finish();
    assert!(run.summaries[0].ns_per_op.is_some(), "{:?}", run.summaries);
}

#[test]
fn nested_runtime_panics_with_actionable_message() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let payload = rt.block_on(async {
        std::panic::catch_unwind(|| cntryl_stress::__private::block_on_tokio(async {}))
            .expect_err("nested runtime must panic")
    });
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(ToString::to_string))
        .unwrap_or_default();
    assert!(
        message.contains("already inside a tokio runtime"),
        "{message}"
    );
}

#[test]
fn caller_provided_runtime_drives_measure_async() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut runner = StressRunner::with_config("tokio-suite", config());
    runner.reporters(Vec::new());
    runner.run("caller runtime", |ctx| {
        rt.block_on(async {
            ctx.measure_async("sleep", || async {
                tokio::time::sleep(Duration::from_millis(1)).await;
            })
            .await;
        });
    });
    let run = runner.finish();
    assert!(run.summaries[0].ns_per_op.is_some());
}
