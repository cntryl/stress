use stress_alias::stress;

#[stress(tier = 1, mystery = "value")]
fn benchmark(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
