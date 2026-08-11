use stress_alias::{stress, StressContext};

#[stress(tier = 1, metadata(component = "parser"))]
fn parse(ctx: &mut StressContext) {
    ctx.measure("parse", || std::hint::black_box(1_u64));
}

#[allow(dead_code)]
mod renamed_entrypoint {
    stress_alias::stress_main!();
}

fn main() {}
