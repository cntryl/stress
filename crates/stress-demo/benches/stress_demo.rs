use cntryl_stress::{black_box, stress, LogicalUnit, OperationOutcome, StressContext};
use std::collections::VecDeque;
use std::sync::mpsc;

cntryl_stress::stress_allocator!();

#[stress(
    tier = 1,
    max_allocs_per_op = 0,
    max_bytes_per_op = 0,
    max_regression_pct = 5,
    metadata(component = "routing", tier_name = "hot_path")
)]
fn tier1_hot_path_route_lookup(ctx: &mut StressContext) {
    let route = b"tenant-a.queue.primary.region-east-1.message-handler-v2.delivery-attempt-000042.customer-enterprise";
    ctx.parameter("route_len", route.len());

    ctx.measure("route hash", || {
        let mut hash = 5381_u64;
        for byte in black_box(route) {
            hash = hash.wrapping_mul(33).wrapping_add(u64::from(*byte));
        }
        black_box(hash)
    });
}

#[stress(tier = 2, metadata(component = "queue", tier_name = "subsystem"))]
fn tier2_subsystem_queue_round_trip(ctx: &mut StressContext) {
    let queue_depth = 1024_u64;
    ctx.parameter("queue_depth", queue_depth);

    ctx.benchmark("queue round trip")
        .operations_per_sample(2_048)
        .measure_with_setup(
            || (0..queue_depth).collect::<VecDeque<_>>(),
            |mut queue| {
                let value = queue.pop_front().unwrap_or_default();
                queue.push_back(value.wrapping_add(1));
                black_box(queue)
            },
        );
}

#[stress(tier = 3, metadata(component = "pipeline", tier_name = "system"))]
fn tier3_system_ingest_transform_commit(ctx: &mut StressContext) {
    let records = (0_u64..512).collect::<Vec<_>>();
    ctx.parameter("record_count", records.len());

    let outcome = ctx.measure_outcome(
        "ingest transform commit",
        LogicalUnit::new("record"),
        || {
            let mut completed = 0_u64;
            let transformed = records
                .iter()
                .map(|value| {
                    completed += 1;
                    value.rotate_left(7) ^ 0x5a5a
                })
                .fold(0_u64, u64::wrapping_add);
            black_box(transformed);
            OperationOutcome::new(records.len() as u64, completed)
        },
    );
    black_box(outcome);
}

#[stress(
    tier = 4,
    max_allocs_per_op = 0.1,
    max_bytes_per_op = 64,
    metadata(component = "transport", tier_name = "integration")
)]
fn tier4_integration_channel_round_trip(ctx: &mut StressContext) {
    let (tx, rx) = mpsc::channel::<u64>();
    let round_trips = 32_u64;
    ctx.parameter("transport", "mpsc");
    ctx.parameter("round_trips_per_iteration", round_trips);

    let outcome = ctx.measure_outcome("channel round trip", LogicalUnit::new("round_trip"), || {
        let mut attempted = 0_u64;
        let mut completed = 0_u64;
        let mut failures = 0_u64;
        for value in 0..round_trips {
            attempted += 1;
            if tx.send(value).is_err() {
                failures += 1;
                break;
            }
            if let Ok(response) = rx.recv() {
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
}

#[stress(
    tier = 5,
    role = "diagnostic",
    metadata(
        component = "queue",
        tier_name = "saturation_scaling",
        evidence_scope = "short_synthetic"
    )
)]
fn tier5_diagnostic_shard_fanout_shape(ctx: &mut StressContext) {
    let shard_count = 64_u64;
    ctx.parameter("shard_count", shard_count);

    let outcome = ctx.measure_outcome("shard fanout", LogicalUnit::new("shard"), || {
        let mut total = 0_u64;
        let mut completed = 0_u64;
        for shard in 0..shard_count {
            total = total.wrapping_add(shard.rotate_left(3));
            completed += 1;
        }
        black_box(total);
        OperationOutcome::new(shard_count, completed)
    });
    black_box(outcome);
}

#[stress(
    tier = 6,
    role = "diagnostic",
    metadata(
        component = "runtime",
        tier_name = "soak_endurance",
        evidence_scope = "short_synthetic"
    )
)]
fn tier6_diagnostic_state_churn_shape(ctx: &mut StressContext) {
    ctx.parameter("window", "demo_short");
    let iterations_per_batch = 1024_u64;

    let outcome = ctx.measure_outcome("state churn", LogicalUnit::new("state_transition"), || {
        let mut state = 1_u64;
        let mut completed = 0_u64;
        for _ in 0..iterations_per_batch {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            black_box(state);
            completed += 1;
        }
        OperationOutcome::new(iterations_per_batch, completed)
    });
    black_box(outcome);
}

cntryl_stress::stress_main!();
