use crate::{BenchRunner, BenchContext};

/// Register example or user-defined benchmarks here.
///
/// Projects consuming this crate can add their own `benches` module or
/// call these registration helpers to populate a runner.
pub fn register_benchmarks(runner: &mut BenchRunner) {
    runner.run("example/black_box", |ctx: &mut BenchContext| {
        let data = vec![0u8; 1024 * 1024];
        ctx.set_bytes(data.len() as u64);
        ctx.measure(|| {
            // simple no-op to keep compiler from optimising away
            std::hint::black_box(&data);
        });
    });
}
