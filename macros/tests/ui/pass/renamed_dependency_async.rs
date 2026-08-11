#[stress_alias::stress(tier = 2)]
async fn lookup(ctx: &mut stress_alias::StressContext) {
    ctx.measure_async("lookup", || async { std::hint::black_box(1_u64) })
        .await;
}

#[allow(dead_code)]
mod renamed_entrypoint {
    stress_alias::stress_main!();
}

fn main() {}
