use stress_alias::stress;

#[stress(tier = 7)]
fn benchmark(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
