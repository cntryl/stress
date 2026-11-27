#[stress_alias::stress(tier = 1, role = "gate")]
fn explicit_gate(ctx: &mut stress_alias::StressContext) {
    ctx.measure("gate", || std::hint::black_box(1_u64));
}

#[stress_alias::stress(tier = 2, role = "diagnostic")]
fn diagnostic(ctx: &mut stress_alias::StressContext) {
    ctx.measure("diagnostic", || std::hint::black_box(2_u64));
}

#[stress_alias::stress(tier = 3, role = "experimental")]
fn experimental(ctx: &mut stress_alias::StressContext) {
    ctx.measure("experimental", || std::hint::black_box(3_u64));
}

#[stress_alias::stress(
    metadata(component = "storage", scenario = "fanout"),
    metadata(owner = "performance")
)]
fn distinct_metadata_blocks(ctx: &mut stress_alias::StressContext) {
    ctx.measure("metadata", || std::hint::black_box(4_u64));
}

fn main() {}
