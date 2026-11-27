use stress_alias::stress;

#[stress(role = "invalid")]
fn invalid_role(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
