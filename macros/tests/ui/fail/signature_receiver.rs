use stress_alias::stress;

struct Suite;

impl Suite {
    #[stress]
    fn benchmark(&mut self) {}
}

fn main() {}
