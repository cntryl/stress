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
    // `available_parallelism` already honors cgroup quotas and affinity, so
    // compare load and quota against the host's online CPUs when known.
    let core_count = read_trimmed(&root.join("sys/devices/system/cpu/online"))
        .and_then(|list| parse_cpu_list(&list))
        .or(core_count);
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

/// Count CPUs in a Linux CPU list such as `0-3,8,10-11`.
pub(crate) fn parse_cpu_list(list: &str) -> Option<usize> {
    list.trim().split(',').try_fold(0usize, |count, part| {
        let part = part.trim();
        let (start, end) = part.split_once('-').unwrap_or((part, part));
        let start = start.parse::<usize>().ok()?;
        let end = end.parse::<usize>().ok()?;
        (end >= start).then(|| count + (end - start + 1))
    })
}

/// Controller path for the current process from `/proc/self/cgroup`: the v2
/// unified path (`0::/path`), or `<controllers>/path` for the v1 hierarchy
/// whose controller list includes `cpu`.
fn process_cgroup_path(root: &Path, v2: bool) -> Option<String> {
    let content = std::fs::read_to_string(root.join("proc/self/cgroup")).ok()?;
    content.lines().find_map(|line| {
        let mut parts = line.splitn(3, ':');
        let _id = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        if v2 {
            controllers.is_empty().then(|| path.to_string())
        } else {
            controllers
                .split(',')
                .any(|controller| controller == "cpu")
                .then(|| format!("{controllers}{path}"))
        }
    })
}

/// Directories from `base/relative` up to `base`, nearest first.
fn ancestors_within(base: &Path, relative: &str) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![base.to_path_buf()];
    let mut current = base.to_path_buf();
    for component in relative
        .split('/')
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
    {
        current.push(component);
        dirs.push(current.clone());
    }
    dirs.reverse();
    dirs
}

fn quota_ratio(quota: &str, period: &str) -> Option<f64> {
    let quota = quota.trim().parse::<f64>().ok()?;
    let period = period.trim().parse::<f64>().ok()?;
    (quota > 0.0 && period > 0.0)
        .then(|| quota / period)
        .filter(|cpus| cpus.is_finite())
}

/// Tightest cgroup v2 `cpu.max` quota along the process cgroup's ancestry.
fn cgroup_v2_quota(root: &Path) -> Option<f64> {
    let base = root.join("sys/fs/cgroup");
    let relative = process_cgroup_path(root, true).unwrap_or_default();
    ancestors_within(&base, &relative)
        .iter()
        .filter_map(|dir| read_trimmed(&dir.join("cpu.max")))
        .filter_map(|max| {
            let mut parts = max.split_whitespace();
            quota_ratio(parts.next()?, parts.next()?)
        })
        .reduce(f64::min)
}

/// Tightest cgroup v1 CFS quota along the process cgroup's ancestry.
fn cgroup_v1_quota(root: &Path) -> Option<f64> {
    let base = root.join("sys/fs/cgroup");
    let candidates = match process_cgroup_path(root, false) {
        Some(path) => {
            let (controllers, relative) = path.split_once('/').unwrap_or((path.as_str(), ""));
            let mut candidates = ancestors_within(&base.join(controllers), relative);
            candidates.extend(ancestors_within(&base.join("cpu"), relative));
            candidates
        }
        None => vec![base.join("cpu")],
    };
    candidates
        .iter()
        .filter_map(|dir| {
            quota_ratio(
                &read_trimmed(&dir.join("cpu.cfs_quota_us"))?,
                &read_trimmed(&dir.join("cpu.cfs_period_us"))?,
            )
        })
        .reduce(f64::min)
}

/// CPU quota in CPUs from cgroup v2 `cpu.max` or v1 CFS files, when limited.
fn cgroup_cpu_quota(root: &Path) -> Option<f64> {
    cgroup_v2_quota(root).or_else(|| cgroup_v1_quota(root))
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
    fn online_host_cpus_are_the_denominator_not_the_quota_limited_parallelism() {
        // available_parallelism() already reflects the cgroup quota, so a
        // 2-CPU container on a 64-CPU host must still be flagged.
        let root = FakeRoot::new("online");
        root.write("sys/devices/system/cpu/online", "0-63\n")
            .write("sys/fs/cgroup/cpu.max", "200000 100000\n")
            .write("proc/loadavg", "40.00 1 1 1/1 1\n");
        let observations = linux_observations(&root.0, Some(2));
        let quota = find(&observations, "cpu_quota");
        assert_eq!(quota.value, "2.00 CPUs (64 cores)");
        assert!(quota.adverse);
        let load = find(&observations, "load_average");
        assert_eq!(load.value, "40.00 (64 cores)");
        assert!(load.adverse);

        assert_eq!(parse_cpu_list("0-3,8,10-11"), Some(7));
        assert_eq!(parse_cpu_list("0"), Some(1));
        assert_eq!(parse_cpu_list("x"), None);
    }

    #[test]
    fn cgroup_v2_reads_the_process_cgroup_and_takes_the_tightest_ancestor() {
        let root = FakeRoot::new("cg2-nested");
        root.write("proc/self/cgroup", "0::/user.slice/app.scope\n")
            .write("sys/fs/cgroup/user.slice/cpu.max", "100000 100000\n")
            .write("sys/fs/cgroup/user.slice/app.scope/cpu.max", "max 100000\n");
        let observations = linux_observations(&root.0, Some(4));
        let quota = find(&observations, "cpu_quota");
        assert_eq!(quota.value, "1.00 CPUs (4 cores)");
        assert!(quota.adverse);
    }

    #[test]
    fn cgroup_v1_reads_the_cpu_controller_path() {
        let root = FakeRoot::new("cg1-nested");
        root.write(
            "proc/self/cgroup",
            "12:memory:/docker/abc\n4:cpu,cpuacct:/docker/abc\n",
        )
        .write(
            "sys/fs/cgroup/cpu,cpuacct/docker/abc/cpu.cfs_quota_us",
            "50000\n",
        )
        .write(
            "sys/fs/cgroup/cpu,cpuacct/docker/abc/cpu.cfs_period_us",
            "100000\n",
        );
        let observations = linux_observations(&root.0, Some(4));
        assert_eq!(
            find(&observations, "cpu_quota").value,
            "0.50 CPUs (4 cores)"
        );
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
        // Reading a handful of small files; the bound only catches a hang and
        // leaves room for a loaded host descheduling the test thread.
        assert!(start.elapsed() < std::time::Duration::from_millis(500));

        // Real host capture kills its subprocess after `SUBPROCESS_BUDGET`;
        // the bound proves it cannot hang, with ample margin for slow process
        // spawning on a loaded machine.
        let start = std::time::Instant::now();
        let _ = capture_observations(Some(1));
        assert!(start.elapsed() < SUBPROCESS_BUDGET + std::time::Duration::from_secs(2));
    }
}
