#![allow(non_upper_case_globals, non_snake_case, dead_code)]

mod shadowed {
    use stress_alias::{stress, StressContext};

    const None: u8 = 0;

    fn Some(_value: f64) -> u8 {
        0
    }

    macro_rules! module_path {
        () => {
            42_u8
        };
    }

    #[stress(tier = 1, max_ns_per_op = 10)]
    fn shadowed(ctx: &mut StressContext) {
        ctx.measure("shadowed", || std::hint::black_box(1_u64));
    }
}

fn main() {}
