use stress_alias::stress;

#[stress]
unsafe fn benchmark(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
