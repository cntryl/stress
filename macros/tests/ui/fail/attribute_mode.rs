use stress_alias::stress;

#[stress(mode = "micro")]
fn benchmark(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
