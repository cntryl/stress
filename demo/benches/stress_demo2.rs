use cntryl_stress::{black_box, stress_test, StressContext};
use std::sync::mpsc;
use std::thread;

#[stress_test(tier = 4, metadata(tier_name = "integration"))]
fn tier4_integration_worker_exchange(ctx: &mut StressContext) {
    let (request_tx, request_rx) = mpsc::channel::<u64>();
    let (response_tx, response_rx) = mpsc::channel::<u64>();
    let exchanges = 32_u64;
    ctx.parameter("exchanges_per_iteration", exchanges);
    let worker = thread::spawn(move || {
        while let Ok(value) = request_rx.recv() {
            if response_tx.send(value.wrapping_mul(2)).is_err() {
                break;
            }
        }
    });

    let completed = ctx.measure_batch(exchanges, || {
        for value in 0..exchanges {
            request_tx.send(value).expect("send request");
            black_box(response_rx.recv().expect("receive response"));
        }
    });
    black_box(completed);

    drop(request_tx);
    worker.join().expect("worker exits");
}

#[stress_test(tier = 5, metadata(tier_name = "saturation_scaling"))]
fn tier5_saturation_scaling_client_fanout(ctx: &mut StressContext) {
    let clients = 32_u64;
    ctx.parameter("client_count", clients);

    let completed = ctx.measure_batch(clients, || {
        let mut total = 0_u64;
        for client in 0..clients {
            total = total.wrapping_add(client.wrapping_mul(17));
        }
        black_box(total);
    });
    let _ = ctx.correctness().attempted(completed).completed(completed);
}

#[stress_test(tier = 6, metadata(tier_name = "soak_endurance"))]
fn tier6_soak_endurance_error_free_loop(ctx: &mut StressContext) {
    let mut checksum = 0_u64;
    ctx.parameter("window", "demo_short");
    let iterations_per_batch = 1024_u64;

    let completed = ctx.measure_batch(iterations_per_batch, || {
        for _ in 0..iterations_per_batch {
            checksum = checksum.rotate_left(1) ^ 0xa5a5_a5a5_a5a5_a5a5;
            black_box(checksum);
        }
    });
    let _ = ctx.correctness().attempted(completed).completed(completed);
}

cntryl_stress::stress_main!();
