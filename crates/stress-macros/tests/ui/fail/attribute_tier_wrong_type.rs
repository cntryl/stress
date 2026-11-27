use stress_alias::stress;

#[stress(tier = "fast")]
fn benchmark(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
