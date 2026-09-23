use stress_alias::{stress, StressContext};

#[stress(tier = 1)]
#[cfg_attr(all(), inline)]
fn cfg_attr_inline_benchmark(ctx: &mut StressContext) {
    ctx.measure("inline", || std::hint::black_box(1_u64));
}

fn main() {}
