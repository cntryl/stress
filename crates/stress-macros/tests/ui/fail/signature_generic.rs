use stress_alias::stress;

#[stress]
fn benchmark<T>(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
