use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct Host {
    pub label: String,
    pub arch: String,
    pub kernel: Option<String>,
    pub cpu_model: Option<String>,
    pub available_vcpus: usize,
    pub ram_mib: Option<u64>,
    pub available_ram_mib: Option<u64>,
    pub swap_mib: Option<u64>,
    pub used_swap_mib: Option<u64>,
}

impl Host {
    pub fn read(label: Option<String>) -> Self {
        let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let mem_kib = |key| {
            field(&meminfo, key)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        };
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let read = |path| fs::read_to_string(path).ok().map(|s| s.trim().to_owned());
        Self {
            label: label
                .or_else(|| read("/proc/sys/kernel/hostname"))
                .unwrap_or_else(|| "VPS".into()),
            arch: std::env::consts::ARCH.into(),
            kernel: read("/proc/sys/kernel/osrelease"),
            cpu_model: field(&cpuinfo, "model name")
                .or_else(|| field(&cpuinfo, "Hardware"))
                .map(str::to_owned),
            available_vcpus: std::thread::available_parallelism().map_or(1, usize::from),
            ram_mib: mem_kib("MemTotal").map(|n| n / 1024),
            available_ram_mib: mem_kib("MemAvailable").map(|n| n / 1024),
            swap_mib: mem_kib("SwapTotal").map(|n| n / 1024),
            used_swap_mib: mem_kib("SwapTotal")
                .zip(mem_kib("SwapFree"))
                .map(|(total, free)| total.saturating_sub(free) / 1024),
        }
    }
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| (name.trim() == key).then(|| value.trim()))
}

pub fn memory_headroom_mib() -> Option<u64> {
    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    let available = field(&meminfo, "MemAvailable")?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        / 1024;
    Some(cgroup_memory_headroom().map_or(available, |bytes| available.min(bytes / MIB)))
}

fn cgroup_memory_headroom() -> Option<u64> {
    let cgroups = fs::read_to_string("/proc/self/cgroup").ok()?;
    let relative = Path::new(cgroups.lines().find_map(|line| line.strip_prefix("0::/"))?);
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    relative
        .ancestors()
        .filter_map(|path| {
            let dir = Path::new("/sys/fs/cgroup").join(path);
            let read = |file| {
                fs::read_to_string(dir.join(file))
                    .ok()?
                    .trim()
                    .parse::<u64>()
                    .ok()
            };
            Some(read("memory.max")?.saturating_sub(read("memory.current")?))
        })
        .min()
}

pub fn read_cpu_ticks() -> Option<(u64, u64)> {
    cpu_ticks(&fs::read_to_string("/proc/stat").ok()?)
}

pub fn cpu_ticks(text: &str) -> Option<(u64, u64)> {
    // guest and guest_nice are already counted in user and nice.
    let fields = text.lines().next()?.strip_prefix("cpu ")?;
    let ticks: Vec<u64> = fields
        .split_whitespace()
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    Some((ticks.iter().sum(), *ticks.get(7)?))
}

pub fn steal_percent(before: (u64, u64), after: (u64, u64)) -> Option<f64> {
    let total = after.0.checked_sub(before.0)?;
    let steal = after.1.checked_sub(before.1)?;
    (total > 0 && steal <= total).then(|| 100.0 * steal as f64 / total as f64)
}
