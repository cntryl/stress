#[stress_alias::stress]
fn sync_result(_ctx: &mut stress_alias::StressContext) -> Result<(), std::io::Error> {
    Ok(())
}

#[stress_alias::stress]
fn standard_result_alias(_ctx: &mut stress_alias::StressContext) -> std::io::Result<()> {
    Ok(())
}

mod application {
    pub type Result<T> = std::result::Result<T, &'static str>;
}

type BenchOutcome = Result<(), &'static str>;

#[stress_alias::stress]
fn application_result_alias(_ctx: &mut stress_alias::StressContext) -> application::Result<()> {
    Ok(())
}

#[stress_alias::stress]
fn arbitrarily_named_result_alias(_ctx: &mut stress_alias::StressContext) -> BenchOutcome {
    Ok(())
}

#[stress_alias::stress]
fn sync_stress_result(_ctx: &mut stress_alias::StressContext) -> stress_alias::StressResult {
    Ok(())
}

#[stress_alias::stress]
async fn async_result(_ctx: &mut stress_alias::StressContext) -> Result<(), &'static str> {
    Ok(())
}

#[stress_alias::stress]
async fn async_stress_result(_ctx: &mut stress_alias::StressContext) -> stress_alias::StressResult {
    Ok(())
}

fn main() {}
