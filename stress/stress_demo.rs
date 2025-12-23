use cntryl_stress::{stress_test, StressContext};

#[stress_test]
fn write_1kb_file(ctx: &mut StressContext) {
    let data = vec![0u8; 1024];
    ctx.set_bytes(data.len() as u64);

    ctx.measure(|| {
        let _ = std::fs::write("target/stress_test", &data);
    });

    std::fs::remove_file("target/stress_test").ok();
}

