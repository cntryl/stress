stress_alias::stress_allocator!();

#[stress_alias::stress(tier = 1, max_allocs_per_op = 0)]
fn allocation_budget(ctx: &mut stress_alias::StressContext) {
    ctx.measure("allocation free", || std::hint::black_box(1_u64));
}

fn main() {}
