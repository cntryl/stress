use stress_alias::stress;

#[stress(metadata(trust_class = "diagnostic"))]
fn reserved_metadata(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
