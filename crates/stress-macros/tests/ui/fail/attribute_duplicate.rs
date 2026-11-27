use stress_alias::stress;

#[stress(name = "first", name = "second")]
fn duplicate_name(_ctx: &mut stress_alias::StressContext) {}

#[stress(tier = 1, tier = 2)]
fn duplicate_tier(_ctx: &mut stress_alias::StressContext) {}

#[stress(ignore, ignore)]
fn duplicate_ignore(_ctx: &mut stress_alias::StressContext) {}

#[stress(role = "gate", role = "diagnostic")]
fn duplicate_role(_ctx: &mut stress_alias::StressContext) {}

#[stress(max_ns_per_op = 10, max_ns_per_op = 20)]
fn duplicate_ns_budget(_ctx: &mut stress_alias::StressContext) {}

#[stress(max_allocs_per_op = 0, max_allocs_per_op = 1)]
fn duplicate_allocs_budget(_ctx: &mut stress_alias::StressContext) {}

#[stress(max_bytes_per_op = 0, max_bytes_per_op = 1)]
fn duplicate_bytes_budget(_ctx: &mut stress_alias::StressContext) {}

#[stress(max_regression_pct = 5, max_regression_pct = 100)]
fn duplicate_regression_budget(_ctx: &mut stress_alias::StressContext) {}

#[stress(max_rsd_pct = 5, max_rsd_pct = 100)]
fn duplicate_rsd_budget(_ctx: &mut stress_alias::StressContext) {}

#[stress(metadata(intent = "read", intent = "write"))]
fn duplicate_metadata_in_one_block(_ctx: &mut stress_alias::StressContext) {}

#[stress(metadata(validated_micro = "true"), metadata(validated_micro = "false"))]
fn duplicate_metadata_across_blocks(_ctx: &mut stress_alias::StressContext) {}

#[stress(name = "   ")]
fn blank_name(_ctx: &mut stress_alias::StressContext) {}

fn main() {}
