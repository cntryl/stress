use cntryl_stress::{stress_test, StressContext};
use std::hint::black_box;
use std::time::Duration;

#[stress_test]
fn sort_large_vector(ctx: &mut StressContext) {
    let mut data = vec![42u32; 1_000_000];
    ctx.parameter("item_count", data.len());

    ctx.measure(|| {
        data.sort_unstable();
        black_box(&data);
    });
}

#[stress_test]
fn hash_string_throughput(ctx: &mut StressContext) {
    use std::collections::HashSet;
    let strings: Vec<_> = (0..10_000).map(|i| format!("key_{i}")).collect();
    let iterations = ctx.measure_for(Duration::from_secs(3), || {
        let mut set = HashSet::new();
        for s in &strings {
            set.insert(s.clone());
        }
        black_box(&set);
    });

    let operations = (strings.len() * iterations) as u64;
    let _ = ctx
        .correctness()
        .attempted(operations)
        .completed(operations);
}

#[stress_test]
fn memory_copy_1mb(ctx: &mut StressContext) {
    let src = vec![1u8; 1024 * 1024];
    ctx.parameter("payload_size", src.len());

    ctx.measure(|| {
        let dst = src.clone();
        black_box(&dst);
    });
}

#[stress_test]
fn recursive_sum(ctx: &mut StressContext) {
    ctx.measure(|| {
        let _ = sum_range(0, 1000);
    });
}

fn sum_range(start: u64, end: u64) -> u64 {
    if start >= end {
        0
    } else {
        start + sum_range(start + 1, end)
    }
}

cntryl_stress::stress_main!();
