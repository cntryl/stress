use cntryl_stress::{black_box, stress, StressContext};
use std::collections::BTreeMap;

#[stress(tier = 1, max_regression_pct = 5, metadata(tier_name = "hot_path"))]
fn tier1_hot_path_header_parse(ctx: &mut StressContext) {
    let header = b"content-type:application/json";
    ctx.parameter("header_len", header.len());

    ctx.measure("header separator", || {
        let separator = header.iter().position(|byte| *byte == b':');
        black_box(separator)
    });
}

#[stress(tier = 2, metadata(tier_name = "subsystem"))]
fn tier2_subsystem_index_insert(ctx: &mut StressContext) {
    let mut index = BTreeMap::<u64, u64>::new();
    ctx.parameter("initial_entries", 256);

    ctx.measure("index insert", || {
        for key in 0_u64..256 {
            index.insert(key, key.rotate_left(5));
        }
        black_box(index.len())
    });
}

#[stress(tier = 2, metadata(tier_name = "subsystem", intent = "async"))]
async fn tier2_subsystem_ready_future(ctx: &mut StressContext) {
    ctx.measure_async("ready future", || async { black_box(42_u64) })
        .await;
}

#[stress(tier = 3, metadata(tier_name = "system"))]
fn tier3_system_snapshot_projection(ctx: &mut StressContext) {
    let events = (0_u64..1024).collect::<Vec<_>>();
    ctx.parameter("event_count", events.len());

    let completed = ctx.measure_batch("snapshot projection", events.len() as u64, || {
        let projected = events
            .iter()
            .enumerate()
            .fold(0_u64, |acc, (index, value)| {
                acc ^ value.wrapping_add(index as u64).rotate_left(11)
            });
        black_box(projected);
    });
    black_box(completed);
}

cntryl_stress::stress_main!();
