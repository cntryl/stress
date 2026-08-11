use cntryl_stress::{black_box, stress, LogicalUnit, OperationOutcome, StressContext};
use std::sync::mpsc;
use std::thread;

#[stress(tier = 4, metadata(tier_name = "integration"))]
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

    let outcome = ctx.measure_outcome("worker exchange", LogicalUnit::new("exchange"), || {
        let mut attempted = 0_u64;
        let mut completed = 0_u64;
        let mut failures = 0_u64;
        for value in 0..exchanges {
            attempted += 1;
            if request_tx.send(value).is_err() {
                failures += 1;
                break;
            }
            if let Ok(response) = response_rx.recv() {
                black_box(response);
                completed += 1;
            } else {
                failures += 1;
                break;
            }
        }
        OperationOutcome::new(attempted, completed).failures(failures)
    });
    black_box(outcome);

    drop(request_tx);
    worker.join().expect("worker exits");
}

#[stress(
    tier = 5,
    role = "diagnostic",
    metadata(tier_name = "saturation_scaling", evidence_scope = "short_synthetic")
)]
fn tier5_diagnostic_client_fanout_shape(ctx: &mut StressContext) {
    let clients = 32_u64;
    ctx.parameter("client_count", clients);

    let outcome = ctx.measure_outcome("client fanout", LogicalUnit::new("client_request"), || {
        let mut total = 0_u64;
        let mut completed = 0_u64;
        for client in 0..clients {
            total = total.wrapping_add(client.wrapping_mul(17));
            completed += 1;
        }
        black_box(total);
        OperationOutcome::new(clients, completed)
    });
    black_box(outcome);
}

#[stress(
    tier = 6,
    role = "diagnostic",
    metadata(tier_name = "soak_endurance", evidence_scope = "short_synthetic")
)]
fn tier6_diagnostic_error_free_loop_shape(ctx: &mut StressContext) {
    ctx.parameter("window", "demo_short");
    let iterations_per_batch = 1024_u64;

    let outcome = ctx.measure_outcome("error free loop", LogicalUnit::new("iteration"), || {
        let mut checksum = 0_u64;
        let mut completed = 0_u64;
        for _ in 0..iterations_per_batch {
            checksum = checksum.rotate_left(1) ^ 0xa5a5_a5a5_a5a5_a5a5;
            black_box(checksum);
            completed += 1;
        }
        OperationOutcome::new(iterations_per_batch, completed)
    });
    black_box(outcome);
}

cntryl_stress::stress_main!();
