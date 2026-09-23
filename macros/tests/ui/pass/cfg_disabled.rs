use stress_alias::{stress, StressContext};

#[stress(tier = 1)]
#[cfg(any())]
fn disabled(ctx: &mut StressContext) {
    ctx.measure("disabled", || std::hint::black_box(1_u64));
}

#[stress(tier = 1)]
#[cfg_attr(any(), cfg(any()))]
#[cfg(all())]
fn enabled(ctx: &mut StressContext) {
    ctx.measure("enabled", || std::hint::black_box(1_u64));
}

fn main() {}
