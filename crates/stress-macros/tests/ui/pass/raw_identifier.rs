use stress_alias::{stress, StressContext};

#[stress(tier = 1)]
fn r#match(ctx: &mut StressContext) {
    ctx.measure("match", || std::hint::black_box(1_u64));
}

fn main() {}
