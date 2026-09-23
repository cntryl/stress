#![allow(non_snake_case)]

use stress_alias::{stress, StressContext};

#[stress(tier = 1)]
fn fetch(ctx: &mut StressContext) {
    ctx.measure("fetch", || std::hint::black_box(1_u64));
}

#[stress(tier = 1)]
fn FETCH(ctx: &mut StressContext) {
    ctx.measure("FETCH", || std::hint::black_box(2_u64));
}

fn main() {}
