use stress_alias::stress;

#[stress]
fn benchmark(_ctx: &mut stress_alias::StressContext) -> usize {
    1
}

fn main() {}
