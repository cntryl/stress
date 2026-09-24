//! Best-effort, read-only host observations captured at run start.
//!
//! Every read is optional: missing files, unreadable values, and failed
//! subprocesses are silently skipped. Observations are informational and never
//! enter baseline compatibility checks.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::artifact::EnvironmentObservation;

/// Upper bound for the macOS `pmset` probe so capture stays fast.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const SUBPROCESS_BUDGET: Duration = Duration::from_millis(40);

/// Load average per core above which the host is considered busy.
const BUSY_LOAD_PER_CORE: f64 = 0.5;

/// Capture observations for the current host.
pub(crate) fn capture_observations(core_count: Option<usize>) -> Vec<EnvironmentObservation> {
    #[cfg(target_os = "linux")]
    {
        linux_observations(Path::new("/"), core_count)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = core_count;
        bounded_stdout("pmset", &["-g", "batt"], SUBPROCESS_BUDGET)
            .and_then(|output| parse_pmset_batt(&output))
            .into_iter()
            .collect()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = core_count;
        Vec::new()
    }
}

/// Linux observations read below `root` (normally `/`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn linux_observations(
    root: &Path,
    core_count: Option<usize>,
) -> Vec<EnvironmentObservation> {
    [
        governor_observation(root),
        boost_observation(root),
        load_observation(root, core_count),
        cpu_quota_observation(root, core_count),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn governor_observation(root: &Path) -> Option<EnvironmentObservation> {
    let cpu_dir = root.join("sys/devices/system/cpu");
    let mut governors = std::fs::read_dir(cpu_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("cpu"))
                .is_some_and(|index| {
                    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())
                })
        })
        .filter_map(|entry| read_trimmed(&entry.path().join("cpufreq/scaling_governor")))
        .collect::<Vec<_>>();
    governors.sort();
    governors.dedup();
    if governors.is_empty() {
        return None;
    }
    let adverse = governors.iter().any(|governor| governor != "performance");
    Some(EnvironmentObservation::new(
        "cpu_governor",
        governors.join(","),
        adverse,
        if adverse {
            "CPU frequency governor is not `performance` on every core; clocks may ramp during measurement"
        } else {
            ""
        },
    ))
}

fn boost_observation(root: &Path) -> Option<EnvironmentObservation> {
    let enabled = read_trimmed(&root.join("sys/devices/system/cpu/intel_pstate/no_turbo"))
        .map(|value| value == "0")
        .or_else(|| {
            read_trimmed(&root.join("sys/devices/system/cpu/cpufreq/boost"))
                .map(|value| value == "1")
        })?;
    Some(EnvironmentObservation::new(
        "cpu_boost",
        if enabled { "enabled" } else { "disabled" },
        enabled,
        if enabled {
            "turbo/boost clocks vary with temperature and load, which adds run-to-run noise"
        } else {
            ""
        },
    ))
}

fn load_observation(root: &Path, core_count: Option<usize>) -> Option<EnvironmentObservation> {
    let content = read_trimmed(&root.join("proc/loadavg"))?;
    let load = content
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .filter(|load| load.is_finite() && *load >= 0.0)?;
    let cores = core_count.filter(|cores| *cores > 0);
    #[allow(clippy::cast_precision_loss)]
    let adverse = cores.is_some_and(|cores| load > cores as f64 * BUSY_LOAD_PER_CORE);
    let value = cores.map_or_else(
        || format!("{load:.2}"),
        |cores| format!("{load:.2} ({cores} cores)"),
    );
    Some(EnvironmentObservation::new(
        "load_average",
        value,
        adverse,
        if adverse {
            "1-minute load average exceeds half the logical cores; other work competes for CPU"
        } else {
            ""
        },
    ))
}

/// CPU quota in CPUs from cgroup v2 `cpu.max` or v1 CFS files, when limited.
fn cgroup_cpu_quota(root: &Path) -> Option<f64> {
    let cgroup = root.join("sys/fs/cgroup");
    let (quota, period) = if let Some(max) = read_trimmed(&cgroup.join("cpu.max")) {
        let mut parts = max.split_whitespace();
        let quota = parts.next()?;
        let period = parts.next()?;
        (quota.parse::<f64>().ok()?, period.parse::<f64>().ok()?)
    } else {
        let v1 = cgroup.join("cpu");
        (
            read_trimmed(&v1.join("cpu.cfs_quota_us"))?
                .parse::<f64>()
                .ok()?,
            read_trimmed(&v1.join("cpu.cfs_period_us"))?
                .parse::<f64>()
                .ok()?,
        )
    };
    (quota > 0.0 && period > 0.0)
        .then(|| quota / period)
        .filter(|cpus| cpus.is_finite())
}

fn cpu_quota_observation(root: &Path, core_count: Option<usize>) -> Option<EnvironmentObservation> {
    let cpus = cgroup_cpu_quota(root)?;
    let cores = core_count.filter(|cores| *cores > 0);
    #[allow(clippy::cast_precision_loss)]
    let adverse = cores.is_some_and(|cores| cpus < cores as f64);
    let value = cores.map_or_else(
        || format!("{cpus:.2} CPUs"),
        |cores| format!("{cpus:.2} CPUs ({cores} cores)"),
    );
    Some(EnvironmentObservation::new(
        "cpu_quota",
        value,
        adverse,
        if adverse {
            "cgroup CPU quota is below the visible core count; the run may be throttled"
        } else {
            ""
        },
    ))
}

/// Parse `pmset -g batt` output into a power-source observation.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_pmset_batt(output: &str) -> Option<EnvironmentObservation> {
    let first = output.lines().next()?;
    if first.contains("'Battery Power'") {
        Some(EnvironmentObservation::new(
            "power_source",
            "battery",
            true,
            "running on battery; power management may throttle the CPU",
        ))
    } else if first.contains("'AC Power'") {
        Some(EnvironmentObservation::new("power_source", "ac", false, ""))
    } else {
        None
    }
}

/// Run a command and return stdout, killing it if it exceeds `budget`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn bounded_stdout(program: &str, args: &[&str], budget: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut stdout = String::new();
                child.stdout.take()?.read_to_string(&mut stdout).ok()?;
                return Some(stdout);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct FakeRoot(PathBuf);

    impl FakeRoot {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "stress-env-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("fake root");
            Self(path)
        }

        fn write(&self, relative: &str, content: &str) -> &Self {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(path, content).expect("write");
            self
        }
    }

    impl Drop for FakeRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn find<'a>(
        observations: &'a [EnvironmentObservation],
        key: &str,
    ) -> &'a EnvironmentObservation {
        observations
            .iter()
            .find(|observation| observation.key == key)
            .unwrap_or_else(|| panic!("missing {key}: {observations:?}"))
    }

    #[test]
    fn empty_root_yields_no_observations() {
        let root = FakeRoot::new("empty");
        assert!(linux_observations(&root.0, Some(4)).is_empty());
    }

    #[test]
    fn governor_is_adverse_unless_every_cpu_uses_performance() {
        let root = FakeRoot::new("governor");
        root.write(
            "sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
            "performance\n",
        )
        .write(
            "sys/devices/system/cpu/cpu1/cpufreq/scaling_governor",
            "powersave\n",
        )
        .write(
            "sys/devices/system/cpu/cpufreq/policy0/scaling_governor",
            "ignored\n",
        );
        let observations = linux_observations(&root.0, Some(2));
        let governor = find(&observations, "cpu_governor");
        assert_eq!(governor.value, "performance,powersave");
        assert!(governor.adverse);
        assert!(!governor.detail.is_empty());

        let root = FakeRoot::new("governor-ok");
        root.write(
            "sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
            "performance\n",
        );
        let observations = linux_observations(&root.0, Some(1));
        assert!(!find(&observations, "cpu_governor").adverse);
    }

    #[test]
    fn boost_reads_intel_pstate_and_cpufreq_boost() {
        let root = FakeRoot::new("no-turbo");
        root.write("sys/devices/system/cpu/intel_pstate/no_turbo", "0\n");
        let observations = linux_observations(&root.0, Some(1));
        let boost = find(&observations, "cpu_boost");
        assert_eq!(boost.value, "enabled");
        assert!(boost.adverse);

        let root = FakeRoot::new("boost-off");
        root.write("sys/devices/system/cpu/cpufreq/boost", "0\n");
        let observations = linux_observations(&root.0, Some(1));
        let boost = find(&observations, "cpu_boost");
        assert_eq!(boost.value, "disabled");
        assert!(!boost.adverse);
    }

    #[test]
    fn load_average_is_adverse_above_half_the_cores() {
        let root = FakeRoot::new("load");
        root.write("proc/loadavg", "3.10 2.00 1.00 2/300 1234\n");
        let observations = linux_observations(&root.0, Some(4));
        let load = find(&observations, "load_average");
        assert_eq!(load.value, "3.10 (4 cores)");
        assert!(load.adverse);

        let observations = linux_observations(&root.0, Some(8));
        assert!(!find(&observations, "load_average").adverse);

        let root = FakeRoot::new("load-garbage");
        root.write("proc/loadavg", "nope\n");
        assert!(linux_observations(&root.0, Some(4)).is_empty());
    }

    #[test]
    fn cgroup_v2_and_v1_quotas_are_reported_when_limited() {
        let root = FakeRoot::new("cg2");
        root.write("sys/fs/cgroup/cpu.max", "150000 100000\n");
        let observations = linux_observations(&root.0, Some(4));
        let quota = find(&observations, "cpu_quota");
        assert_eq!(quota.value, "1.50 CPUs (4 cores)");
        assert!(quota.adverse);

        let root = FakeRoot::new("cg2-max");
        root.write("sys/fs/cgroup/cpu.max", "max 100000\n");
        assert!(linux_observations(&root.0, Some(4)).is_empty());

        let root = FakeRoot::new("cg1");
        root.write("sys/fs/cgroup/cpu/cpu.cfs_quota_us", "400000\n")
            .write("sys/fs/cgroup/cpu/cpu.cfs_period_us", "100000\n");
        let observations = linux_observations(&root.0, Some(4));
        let quota = find(&observations, "cpu_quota");
        assert_eq!(quota.value, "4.00 CPUs (4 cores)");
        assert!(!quota.adverse);

        let root = FakeRoot::new("cg1-unlimited");
        root.write("sys/fs/cgroup/cpu/cpu.cfs_quota_us", "-1\n")
            .write("sys/fs/cgroup/cpu/cpu.cfs_period_us", "100000\n");
        assert!(linux_observations(&root.0, Some(4)).is_empty());
    }

    #[test]
    fn pmset_output_reports_battery_power_as_adverse() {
        let battery = parse_pmset_batt(
            "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t80%; discharging",
        )
        .expect("battery");
        assert_eq!(battery.key, "power_source");
        assert_eq!(battery.value, "battery");
        assert!(battery.adverse);
        let ac = parse_pmset_batt("Now drawing from 'AC Power'\n").expect("ac");
        assert_eq!(ac.value, "ac");
        assert!(!ac.adverse);
        assert!(parse_pmset_batt("garbage").is_none());
    }

    #[test]
    fn capture_is_fast() {
        let root = FakeRoot::new("fast");
        root.write(
            "sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
            "powersave\n",
        )
        .write("proc/loadavg", "0.10 0.10 0.10 1/1 1\n")
        .write("sys/fs/cgroup/cpu.max", "100000 100000\n");
        let start = std::time::Instant::now();
        let _ = linux_observations(&root.0, Some(1));
        assert!(start.elapsed() < std::time::Duration::from_millis(50));

        // Real host capture, including any bounded subprocess, stays well
        // under a loose bound even on slow CI machines.
        let start = std::time::Instant::now();
        let _ = capture_observations(Some(1));
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
    }
}
