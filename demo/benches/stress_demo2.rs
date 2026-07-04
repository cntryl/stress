use cntryl_stress::{black_box, stress_test, StressContext};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[stress_test(tier = 4, metadata(tier_name = "integration"))]
fn tier4_integration_worker_exchange(ctx: &mut StressContext) {
    let (request_tx, request_rx) = mpsc::channel::<u64>();
    let (response_tx, response_rx) = mpsc::channel::<u64>();
    let worker = thread::spawn(move || {
        while let Ok(value) = request_rx.recv() {
            if response_tx.send(value.wrapping_mul(2)).is_err() {
                break;
            }
        }
    });

    ctx.measure(|| {
        request_tx.send(21).expect("send request");
        black_box(response_rx.recv().expect("receive response"))
    });

    drop(request_tx);
    worker.join().expect("worker exits");
}

#[stress_test(
    tier = 5,
    mode = "fixed_duration",
    metadata(tier_name = "saturation_scaling")
)]
fn tier5_saturation_scaling_client_fanout(ctx: &mut StressContext) {
    let clients = 32_u64;
    ctx.parameter("client_count", clients);

    let iterations = ctx.measure_for(Duration::from_millis(25), || {
        let mut total = 0_u64;
        for client in 0..clients {
            total = total.wrapping_add(client.wrapping_mul(17));
        }
        black_box(total);
    });
    let completed = iterations as u64;
    let _ = ctx.correctness().attempted(completed).completed(completed);
}

#[stress_test(
    tier = 6,
    mode = "fixed_duration",
    metadata(tier_name = "soak_endurance")
)]
fn tier6_soak_endurance_error_free_loop(ctx: &mut StressContext) {
    let mut checksum = 0_u64;
    ctx.parameter("window", "demo_short");

    let iterations = ctx.measure_for(Duration::from_millis(25), || {
        checksum = checksum.rotate_left(1) ^ 0xa5a5_a5a5_a5a5_a5a5;
        black_box(checksum);
    });
    let completed = iterations as u64;
    let _ = ctx.correctness().attempted(completed).completed(completed);
}

cntryl_stress::stress_main!();
