use crate::host::{read_cpu_ticks, steal_percent};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

const MIB: f64 = 1024.0 * 1024.0;
const CPU_PRIME: u32 = 20_000;
pub const MEMORY_TRANSFER_MIB: u32 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    EventsPerSecond,
    MibPerSecond,
    CpuStealPercent,
    Iops,
    LatencyP50Ms,
    LatencyP95Ms,
    LatencyP99Ms,
    SyncP95Ms,
    SyncP99Ms,
    MegabitsPerSecond,
    TcpRetransmits,
    TcpMeanRttMs,
    PacketsSent,
    PacketsReceived,
    PacketLossPercent,
    RttP50Ms,
    RttP95Ms,
    RttP99Ms,
}

impl MetricKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::EventsPerSecond => "Событий/с",
            Self::MibPerSecond => "Скорость, МиБ/с",
            Self::CpuStealPercent => "CPU steal, %",
            Self::Iops => "Операций/с (IOPS)",
            Self::LatencyP50Ms => "Задержка p50, мс",
            Self::LatencyP95Ms => "Задержка p95, мс",
            Self::LatencyP99Ms => "Задержка p99, мс",
            Self::SyncP95Ms => "fdatasync p95, мс",
            Self::SyncP99Ms => "fdatasync p99, мс",
            Self::MegabitsPerSecond => "Скорость, Мбит/с",
            Self::TcpRetransmits => "Повторные передачи TCP",
            Self::TcpMeanRttMs => "Средний TCP RTT, мс",
            Self::PacketsSent => "ICMP отправлено",
            Self::PacketsReceived => "ICMP получено",
            Self::PacketLossPercent => "Потери ICMP, %",
            Self::RttP50Ms => "RTT p50, мс",
            Self::RttP95Ms => "RTT p95, мс",
            Self::RttP99Ms => "RTT p99, мс",
        }
    }

    pub fn higher_is_better(self) -> bool {
        matches!(
            self,
            Self::EventsPerSecond | Self::MibPerSecond | Self::Iops | Self::MegabitsPerSecond
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Metric {
    pub kind: MetricKind,
    pub value: f64,
}

impl Metric {
    pub fn new(kind: MetricKind, value: f64) -> Self {
        Self { kind, value }
    }
}

pub fn command(
    tool: &str,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
    deadline: u32,
    dir: Option<&Path>,
) -> Result<String, String> {
    let mut command = timed_command(tool, deadline);
    command.args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .map_err(|err| format!("Не удалось запустить {tool} через timeout: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match output.status.code() {
        Some(0) if stdout.trim().is_empty() => return Ok(stderr.into_owned()),
        Some(0) => return Ok(stdout.into_owned()),
        Some(124 | 137) => return Err(format!("{tool}: превышен предел {deadline} с")),
        _ => {}
    }
    let json_error = serde_json::from_str::<Value>(&stdout)
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_owned));
    let detail = json_error
        .as_deref()
        .or_else(|| stderr.lines().find(|line| !line.trim().is_empty()))
        .or_else(|| stdout.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or_default();
    let detail: String = detail.chars().take(220).collect();
    Err(format!("{tool} ({}): {detail}", output.status))
}

pub fn timed_command(tool: &str, deadline: u32) -> Command {
    let mut command = Command::new("timeout");
    command
        .arg("--kill-after=2s")
        .arg(format!("{deadline}s"))
        .arg(tool)
        .env("LC_ALL", "C");
    command
}

pub fn cpu_rate(text: &str) -> Result<f64, String> {
    let rate = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("events per second:"));
    positive_rate(rate, "sysbench CPU: отсутствует число событий в секунду")
}

pub fn memory_rate(text: &str) -> Result<f64, String> {
    let rate = text
        .lines()
        .find_map(|line| line.split_once(" MiB transferred ("))
        .and_then(|(_, tail)| tail.strip_suffix(" MiB/sec)"));
    positive_rate(rate, "sysbench RAM: отсутствует пропускная способность")
}

fn positive_rate(text: Option<&str>, error: &str) -> Result<f64, String> {
    text.and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| error.into())
}

pub fn cpu(seconds: u32, threads: usize) -> Result<Vec<Metric>, String> {
    let before = read_cpu_ticks();
    let output = command(
        "sysbench",
        [
            format!("--time={seconds}"),
            format!("--threads={threads}"),
            format!("--cpu-max-prime={CPU_PRIME}"),
            "cpu".into(),
            "run".into(),
        ],
        seconds + 10,
        None,
    )?;
    let mut metrics = vec![Metric::new(MetricKind::EventsPerSecond, cpu_rate(&output)?)];
    if let Some(percent) = before
        .zip(read_cpu_ticks())
        .and_then(|(a, b)| steal_percent(a, b))
    {
        metrics.push(Metric::new(MetricKind::CpuStealPercent, percent));
    }
    Ok(metrics)
}

pub fn memory(seconds: u32, block_mib: u32, operation: &str) -> Result<Vec<Metric>, String> {
    let output = command(
        "sysbench",
        [
            format!("--time={seconds}"),
            "--threads=1".into(),
            format!("--memory-block-size={block_mib}M"),
            format!("--memory-total-size={MEMORY_TRANSFER_MIB}M"),
            "--memory-scope=local".into(),
            "--memory-access-mode=seq".into(),
            format!("--memory-oper={operation}"),
            "memory".into(),
            "run".into(),
        ],
        seconds + 10,
        None,
    )?;
    Ok(vec![Metric::new(
        MetricKind::MibPerSecond,
        memory_rate(&output)?,
    )])
}

pub fn disk(dir: &Path, seconds: u32, size_mib: u32, mode: &str) -> Result<Vec<Metric>, String> {
    let create_file = mode == "write";
    let sync = mode == "sync";
    let fio_mode = if sync { "randwrite" } else { mode };
    let random = fio_mode.starts_with("rand");
    let read = fio_mode.ends_with("read");
    let block = if random { "4k" } else { "1m" };
    let mut args = vec![
        "--name=vps-bench".into(),
        "--filename=data.bin".into(),
        "--ioengine=psync".into(),
        "--iodepth=1".into(),
        format!("--direct={}", u8::from(!sync)),
        format!("--size={size_mib}m"),
        format!("--rw={fio_mode}"),
        format!("--bs={block}"),
        "--lat_percentiles=1".into(),
        "--percentile_list=50:95:99".into(),
        "--output-format=json".into(),
    ];
    if create_file {
        // A complete sequential write also initializes the working set for the reads.
        args.extend([
            "--create_on_open=1".into(),
            "--fallocate=none".into(),
            "--end_fsync=1".into(),
        ]);
    } else {
        args.extend([
            "--allow_file_create=0".into(),
            format!("--runtime={seconds}"),
        ]);
    }
    if read {
        args.extend(["--readonly".into(), "--time_based".into()]);
    } else {
        args.push("--refill_buffers=1".into());
    }
    if sync {
        args.push("--fdatasync=1".into());
    }
    let output = command(
        "fio",
        args,
        if create_file {
            seconds + 45
        } else {
            seconds + 15
        },
        Some(dir),
    )?;
    let value = parse_json("fio", &output)?;
    if create_file
        && value
            .pointer("/jobs/0/write/io_bytes")
            .and_then(Value::as_u64)
            != Some(u64::from(size_mib) * MIB as u64)
    {
        return Err("fio: не удалось полностью записать тестовый файл".into());
    }
    let mut metrics = fio_metrics(&value, if read { "read" } else { "write" }, random && !sync)?;
    if sync {
        let stats = &value["jobs"][0]["sync"]["lat_ns"]["percentile"];
        for (key, kind) in [
            ("95.000000", MetricKind::SyncP95Ms),
            ("99.000000", MetricKind::SyncP99Ms),
        ] {
            metrics.push(Metric::new(
                kind,
                number(&stats[key], "fio fdatasync")? / 1_000_000.0,
            ));
        }
    }
    Ok(metrics)
}

pub fn fio_metrics(value: &Value, direction: &str, latency: bool) -> Result<Vec<Metric>, String> {
    let job = &value["jobs"][0];
    if job["error"].as_u64() != Some(0) || job[direction]["io_bytes"].as_u64().unwrap_or(0) == 0 {
        return Err("fio: тест не завершил операции ввода-вывода".into());
    }
    let stats = &job[direction];
    let mut metrics = vec![
        Metric::new(
            MetricKind::MibPerSecond,
            number(&stats["bw_bytes"], "fio bw_bytes")? / MIB,
        ),
        Metric::new(MetricKind::Iops, number(&stats["iops"], "fio IOPS")?),
    ];
    if latency {
        for (key, kind) in [
            ("50.000000", MetricKind::LatencyP50Ms),
            ("95.000000", MetricKind::LatencyP95Ms),
            ("99.000000", MetricKind::LatencyP99Ms),
        ] {
            let ns = number(&stats["lat_ns"]["percentile"][key], "fio latency")?;
            metrics.push(Metric::new(kind, ns / 1_000_000.0));
        }
    }
    Ok(metrics)
}

pub fn network(
    host: &str,
    port: u16,
    seconds: u32,
    streams: u32,
    reverse: bool,
) -> Result<Vec<Metric>, String> {
    let mut args = vec![
        format!("--client={host}"),
        format!("--port={port}"),
        format!("--time={seconds}"),
        format!("--parallel={streams}"),
        "--connect-timeout=3000".into(),
        "--json".into(),
    ];
    if reverse {
        args.push("--reverse".into());
    }
    iperf_metrics(&parse_json(
        "iperf3",
        &command("iperf3", args, seconds + 10, None)?,
    )?)
}

pub fn iperf_metrics(value: &Value) -> Result<Vec<Metric>, String> {
    if let Some(error) = value["error"].as_str() {
        return Err(format!("iperf3: {error}"));
    }
    let end = &value["end"];
    let bps = number(&end["sum_received"]["bits_per_second"], "iperf3 throughput")?;
    let mut metrics = vec![Metric::new(
        MetricKind::MegabitsPerSecond,
        bps / 1_000_000.0,
    )];
    if let Some(retransmits) = end["sum_sent"]["retransmits"].as_u64() {
        metrics.push(Metric::new(MetricKind::TcpRetransmits, retransmits as f64));
    }
    let rtts = end["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|stream| stream["sender"]["mean_rtt"].as_u64())
        .filter(|&rtt| rtt > 0);
    let (sum, count) = rtts.fold((0.0, 0), |(sum, count), rtt| (sum + rtt as f64, count + 1));
    if count > 0 {
        metrics.push(Metric::new(
            MetricKind::TcpMeanRttMs,
            sum / f64::from(count) / 1000.0,
        ));
    }
    Ok(metrics)
}

fn number(value: &Value, field: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or_else(|| format!("Отсутствует числовое поле {field}"))
}

fn parse_json(tool: &str, output: &str) -> Result<Value, String> {
    serde_json::from_str(output).map_err(|err| format!("{tool}: не удалось разобрать JSON: {err}"))
}
