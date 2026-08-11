use stress_alias::stress;

#[stress]
extern "C" fn benchmark(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
