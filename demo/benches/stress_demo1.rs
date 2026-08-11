use cntryl_stress::{black_box, stress, LogicalUnit, OperationOutcome, StressContext};
use std::collections::BTreeMap;

#[stress(tier = 1, max_regression_pct = 5, metadata(tier_name = "hot_path"))]
fn tier1_hot_path_header_parse(ctx: &mut StressContext) {
    let headers = b"content-type: application/json; charset=utf-8\r\n\
x-cntryl-route: tenant-a.queue.primary.region-east-1.message-handler-v2\r\n\
x-request-id: 018f8f9d-7c42-7ea8-9d4f-5f74b9201197\r\n\r\n";
    ctx.parameter("header_bytes", headers.len());

    ctx.measure("header terminator", || {
        let terminator = black_box(headers)
            .windows(4)
            .position(|window| window == b"\r\n\r\n");
        black_box(terminator)
    });
}

#[stress(tier = 2, metadata(tier_name = "subsystem"))]
fn tier2_subsystem_index_insert(ctx: &mut StressContext) {
    let initial_entries = 256_u64;
    ctx.parameter("initial_entries", initial_entries);

    ctx.benchmark("index insert")
        .operations_per_sample(256)
        .measure_with_setup(
            || {
                (0..initial_entries)
                    .map(|key| (key, key.rotate_left(5)))
                    .collect::<BTreeMap<_, _>>()
            },
            |mut index| {
                index.insert(initial_entries, initial_entries.rotate_left(5));
                black_box(index)
            },
        );
}

#[stress(tier = 2, metadata(tier_name = "subsystem", intent = "async"))]
async fn tier2_subsystem_ready_future(ctx: &mut StressContext) -> Result<(), &'static str> {
    let iterations = 8_192_u64;
    ctx.parameter("iterations", iterations);
    let value = ctx
        .measure_result_async("ready future", || async {
            let mut checksum = 42_u64;
            for input in 0..iterations {
                checksum = black_box(checksum.rotate_left(7) ^ input.wrapping_mul(31));
            }
            Ok::<_, &'static str>(black_box(checksum))
        })
        .await?;
    black_box(value);
    Ok(())
}

#[stress(tier = 3, metadata(tier_name = "system"))]
fn tier3_system_snapshot_projection(ctx: &mut StressContext) {
    let events = (0_u64..1024).collect::<Vec<_>>();
    ctx.parameter("event_count", events.len());

    let outcome = ctx.measure_outcome("snapshot projection", LogicalUnit::new("event"), || {
        let mut completed = 0_u64;
        let projected = events
            .iter()
            .enumerate()
            .fold(0_u64, |acc, (index, value)| {
                completed += 1;
                acc ^ value.wrapping_add(index as u64).rotate_left(11)
            });
        black_box(projected);
        OperationOutcome::new(events.len() as u64, completed)
    });
    black_box(outcome);
}

cntryl_stress::stress_main!();
