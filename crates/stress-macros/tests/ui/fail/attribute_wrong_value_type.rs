use stress_alias::stress;

#[stress(name = 42)]
fn benchmark(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
