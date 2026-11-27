use stress_alias::stress;

#[stress(tier = 0)]
fn benchmark(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
