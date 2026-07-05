use cntryl_stress::{black_box, stress, StressContext};
use std::collections::VecDeque;
use std::sync::mpsc;

cntryl_stress::stress_allocator!();

#[stress(
    tier = 1,
    max_allocs_per_op = 0,
    max_bytes_per_op = 0,
    max_regression_pct = 5,
    max_rsd_pct = 20,
    metadata(component = "routing", tier_name = "hot_path")
)]
fn tier1_hot_path_route_lookup(ctx: &mut StressContext) {
    let route = b"tenant-a.queue.primary";
    ctx.parameter("route_len", route.len());

    ctx.measure("route hash", || {
        let mut hash = 0_u64;
        for byte in route {
            hash = hash.wrapping_mul(31).wrapping_add(u64::from(*byte));
        }
        black_box(hash)
    });
}

#[stress(tier = 2, metadata(component = "queue", tier_name = "subsystem"))]
fn tier2_subsystem_queue_round_trip(ctx: &mut StressContext) {
    let mut queue = (0_u64..1024).collect::<VecDeque<_>>();
    ctx.parameter("queue_depth", queue.len());

    ctx.measure("queue round trip", || {
        let value = queue.pop_front().unwrap_or_default();
        queue.push_back(value.wrapping_add(1));
        black_box(queue.front().copied())
    });
}

#[stress(tier = 3, metadata(component = "pipeline", tier_name = "system"))]
fn tier3_system_ingest_transform_commit(ctx: &mut StressContext) {
    let records = (0_u64..512).collect::<Vec<_>>();
    ctx.parameter("record_count", records.len());

    let completed = ctx.measure_batch("ingest transform commit", records.len() as u64, || {
        let transformed = records
            .iter()
            .map(|value| value.rotate_left(7) ^ 0x5a5a)
            .fold(0_u64, u64::wrapping_add);
        black_box(transformed);
    });
    black_box(completed);
}

#[stress(tier = 4, metadata(component = "transport", tier_name = "integration"))]
fn tier4_integration_channel_round_trip(ctx: &mut StressContext) {
    let (tx, rx) = mpsc::channel::<u64>();
    let round_trips = 32_u64;
    ctx.parameter("transport", "mpsc");
    ctx.parameter("round_trips_per_iteration", round_trips);

    let completed = ctx.measure_batch("channel round trip", round_trips, || {
        for value in 0..round_trips {
            tx.send(value).expect("send");
            black_box(rx.recv().expect("recv"));
        }
    });
    black_box(completed);
}

#[stress(
    tier = 5,
    metadata(component = "queue", tier_name = "saturation_scaling")
)]
fn tier5_saturation_scaling_shards(ctx: &mut StressContext) {
    let shard_count = 64_u64;
    ctx.parameter("shard_count", shard_count);

    let completed = ctx.measure_batch("shard fanout", shard_count, || {
        let mut total = 0_u64;
        for shard in 0..shard_count {
            total = total.wrapping_add(shard.rotate_left(3));
        }
        black_box(total);
    });
    black_box(completed);
}

#[stress(
    tier = 6,
    metadata(component = "runtime", tier_name = "soak_endurance")
)]
fn tier6_soak_endurance_state_churn(ctx: &mut StressContext) {
    ctx.parameter("window", "demo_short");
    let iterations_per_batch = 1024_u64;

    let mut state = 1_u64;
    let completed = ctx.measure_batch("state churn", iterations_per_batch, || {
        for _ in 0..iterations_per_batch {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            black_box(state);
        }
    });
    let _ = ctx.correctness().attempted(completed).completed(completed);
}

cntryl_stress::stress_main!();
