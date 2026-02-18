use cntryl_stress::{stress_test, StressContext};
use std::hint::black_box;

#[stress_test]
fn write_1kb_file(ctx: &mut StressContext) {
    let data = vec![0u8; 1024];
    ctx.set_bytes(data.len() as u64);

    ctx.measure(|| {
        let _ = std::fs::write("target/stress_test", &data);
    });

    std::fs::remove_file("target/stress_test").ok();
}

#[stress_test]
fn allocate_large_buffer(ctx: &mut StressContext) {
    let size = 10 * 1024 * 1024; // 10 MB
    ctx.set_bytes(size as u64);

    ctx.measure(|| {
        let mut buffer = vec![0u8; size];
        buffer[0] = 1;
        buffer[size - 1] = 1;
        black_box(&buffer);
    });
}

#[stress_test]
fn compute_fibonacci(ctx: &mut StressContext) {
    ctx.measure(|| {
        let _ = fibonacci(30);
    });
}

fn fibonacci(n: u32) -> u64 {
    match n {
        0 => 0,
        1 => 1,
        _ => fibonacci(n - 1) + fibonacci(n - 2),
    }
}

cntryl_stress::stress_main!();
