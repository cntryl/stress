use cntryl_stress::{stress_test, StressContext};
use std::hint::black_box;

#[stress_test]
fn sort_large_vector(ctx: &mut StressContext) {
    let mut data = vec![42u32; 1_000_000];
    ctx.set_elements(data.len() as u64);

    ctx.measure(|| {
        data.sort_unstable();
        black_box(&data);
    });
}

#[stress_test]
fn hash_string_throughput(ctx: &mut StressContext) {
    use std::collections::HashSet;
    let strings: Vec<_> = (0..10_000).map(|i| format!("key_{}", i)).collect();
    ctx.set_elements(strings.len() as u64);

    ctx.measure(|| {
        let mut set = HashSet::new();
        for s in &strings {
            set.insert(s.clone());
        }
        black_box(&set);
    });
}

#[stress_test]
fn memory_copy_1mb(ctx: &mut StressContext) {
    let src = vec![1u8; 1024 * 1024];
    ctx.set_bytes(src.len() as u64);

    ctx.measure(|| {
        let _dst = src.clone();
        black_box(&_dst);
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
