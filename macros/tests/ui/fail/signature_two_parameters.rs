use stress_alias::stress;

#[stress]
fn benchmark(_first: &mut stress_alias::StressContext, _second: &mut stress_alias::StressContext) {}

fn main() {}
