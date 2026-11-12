use crate::bench::{Metric, MetricKind};
use crate::host::Host;
use comfy_table::{Cell, Color, ContentArrangement, Table, modifiers, presets};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{IsTerminal, Write, stdout};
use std::path::Path;
use tempfile::NamedTempFile;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Case {
    CpuSingle,
    CpuMulti,
    RamRead,
    RamWrite,
    DiskSeqWrite,
    DiskSeqRead,
    DiskRandRead,
    DiskRandWrite,
    DiskSync,
    NetUp,
    NetDown,
    NetUpMulti,
    NetDownMulti,
    PingIdle,
    PingUpload,
    PingDownload,
}

impl Case {
    pub const ALL: [Self; 13] = [
        Self::CpuSingle,
        Self::CpuMulti,
        Self::RamRead,
        Self::RamWrite,
        Self::DiskSeqWrite,
        Self::DiskSeqRead,
        Self::DiskRandRead,
        Self::DiskRandWrite,
        Self::DiskSync,
        Self::NetUp,
        Self::NetDown,
        Self::NetUpMulti,
        Self::NetDownMulti,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::CpuSingle => "CPU · один поток",
            Self::CpuMulti => "CPU · многопоточный",
            Self::RamRead => "RAM · чтение",
            Self::RamWrite => "RAM · запись",
            Self::DiskSeqWrite => "Диск · запись 1 МиБ",
            Self::DiskSeqRead => "Диск · чтение 1 МиБ",
            Self::DiskRandRead => "Диск · случайное чтение 4 КиБ",
            Self::DiskRandWrite => "Диск · случайная запись 4 КиБ",
            Self::DiskSync => "Диск · запись с fdatasync",
            Self::NetUp => "TCP ↑ один поток",
            Self::NetDown => "TCP ↓ один поток",
            Self::NetUpMulti => "TCP ↑ несколько потоков",
            Self::NetDownMulti => "TCP ↓ несколько потоков",
            Self::PingIdle => "ICMP · простой",
            Self::PingUpload => "ICMP · нагрузка ↑",
            Self::PingDownload => "ICMP · нагрузка ↓",
        }
    }

    pub fn tool(self) -> &'static str {
        match self {
            Self::CpuSingle | Self::CpuMulti | Self::RamRead | Self::RamWrite => "sysbench",
            Self::DiskSeqWrite
            | Self::DiskSeqRead
            | Self::DiskRandRead
            | Self::DiskRandWrite
            | Self::DiskSync => "fio",
            Self::PingIdle | Self::PingUpload | Self::PingDownload => "ping",
            _ => "iperf3",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Measurement {
    pub test: Case,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub parameters: Value,
    #[serde(flatten)]
    pub outcome: Outcome,
}

fn test_label(test: Case, target: Option<&str>) -> String {
    target.map_or_else(
        || test.label().to_owned(),
        |host| format!("{} · {}", test.label(), clean(host)),
    )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Completed { metrics: Vec<Metric> },
    Skipped { reason: String },
    Failed { reason: String },
}

impl From<Result<Vec<Metric>, String>> for Outcome {
    fn from(result: Result<Vec<Metric>, String>) -> Self {
        match result {
            Ok(metrics) => Self::Completed { metrics },
            Err(reason) => Self::Failed { reason },
        }
    }
}

impl Outcome {
    fn status(&self) -> (&'static str, Color) {
        match self {
            Self::Completed { .. } => ("Готово", Color::Green),
            Self::Skipped { .. } => ("Пропущено", Color::Yellow),
            Self::Failed { .. } => ("Ошибка", Color::Red),
        }
    }

    fn reason(&self) -> &str {
        match self {
            Self::Completed { .. } => "",
            Self::Skipped { reason } | Self::Failed { reason } => reason,
        }
    }

    fn metrics(&self) -> &[Metric] {
        match self {
            Self::Completed { metrics } => metrics,
            _ => &[],
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    schema_version: u32,
    program_version: String,
    started_at_unix_ms: u64,
    pub host: Host,
    pub tools: BTreeMap<String, String>,
    pub disk_directory: Option<String>,
    pub disk_filesystem: Option<String>,
    pub disk_available_mib: Option<u64>,
    pub measurements: Vec<Measurement>,
}

impl Report {
    pub fn new(host: Host, started_at_unix_ms: u64) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            program_version: env!("CARGO_PKG_VERSION").into(),
            started_at_unix_ms,
            host,
            tools: BTreeMap::new(),
            disk_directory: None,
            disk_filesystem: None,
            disk_available_mib: None,
            measurements: Vec::new(),
        }
    }

    pub fn save(
        &self,
        mut file: NamedTempFile,
        path: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        serde_json::to_writer_pretty(&mut file, self)?;
        writeln!(file)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let read = || -> Result<Self, Box<dyn std::error::Error>> {
            let report: Self = serde_json::from_reader(std::io::BufReader::new(File::open(path)?))?;
            if report.schema_version != SCHEMA_VERSION {
                return Err(
                    format!("версия схемы {} не поддерживается", report.schema_version).into(),
                );
            }
            Ok(report)
        };
        read().map_err(|err| format!("{}: {err}", path.display()))
    }

    pub fn success(&self) -> bool {
        self.measurements
            .iter()
            .any(|row| matches!(row.outcome, Outcome::Completed { .. }))
            && !self
                .measurements
                .iter()
                .any(|row| matches!(row.outcome, Outcome::Failed { .. }))
    }

    pub fn print(&self, color: ColorMode) {
        println!("VPS Bench · {}", host_summary(&self.host));
        if let Some(path) = &self.disk_directory {
            let filesystem = clean(self.disk_filesystem.as_deref().unwrap_or("ФС неизвестна"));
            let available = self.disk_available_mib.map_or(String::new(), |mib| {
                format!(" · свободно {:.2} ГиБ", mib as f64 / 1024.0)
            });
            println!("Том для fio · {} ({filesystem}){available}", clean(path));
        }
        let mut table = table(color, &["Тест", "Метрика", "Результат", "Статус"]);
        for row in &self.measurements {
            let label = test_label(row.test, row.target.as_deref());
            if row.outcome.metrics().is_empty() {
                table.add_row([
                    Cell::new(&label),
                    Cell::new(""),
                    Cell::new(clean(row.outcome.reason())),
                    status_cell(Some(row)),
                ]);
                continue;
            }
            for (index, metric) in row.outcome.metrics().iter().enumerate() {
                let (label, status) = if index == 0 {
                    (label.as_str(), status_cell(Some(row)))
                } else {
                    ("", Cell::new(""))
                };
                table.add_row([
                    Cell::new(label),
                    Cell::new(metric.kind.label()),
                    metric_cell(metric.kind, metric.value),
                    status,
                ]);
            }
        }
        println!("{table}");
    }

    fn measurement(&self, test: Case, target: Option<&str>) -> Option<&Measurement> {
        self.measurements
            .iter()
            .find(|row| row.test == test && row.target.as_deref() == target)
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

fn table(color: ColorMode, headers: &[&str]) -> Table {
    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_FULL_CONDENSED)
        .apply_modifier(modifiers::UTF8_ROUND_CORNERS)
        .set_content_arrangement(ContentArrangement::Dynamic);
    let use_color = match color {
        ColorMode::Auto => {
            stdout().is_terminal()
                && std::env::var_os("NO_COLOR").is_none()
                && std::env::var("TERM").as_deref() != Ok("dumb")
        }
        ColorMode::Always => true,
        ColorMode::Never => false,
    };
    if use_color {
        table.enforce_styling();
    } else {
        table.force_no_tty();
    }
    table.set_header(
        headers
            .iter()
            .map(|title| Cell::new(title).fg(Color::Cyan))
            .collect::<Vec<_>>(),
    );
    table
}

fn status_cell(row: Option<&Measurement>) -> Cell {
    let (text, color) = row.map_or(("Нет замера", Color::Yellow), |r| {
        r.outcome.status()
    });
    Cell::new(text).fg(color)
}

fn host_summary(host: &Host) -> String {
    let mut summary = format!(
        "{} · {} · {} vCPU",
        clean(&host.label),
        clean(&host.arch),
        host.available_vcpus
    );
    if let Some(cpu) = &host.cpu_model {
        summary.push_str(&format!(" · {}", clean(cpu)));
    }
    if let Some(mib) = host.ram_mib {
        summary.push_str(&format!(" · RAM {:.2} ГиБ", mib as f64 / 1024.0));
    }
    if let Some(mib) = host.used_swap_mib.filter(|mib| *mib > 0) {
        summary.push_str(&format!(" · занято swap {mib} МиБ"));
    }
    summary
}

pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn format_value(value: f64) -> String {
    if value != 0.0 && value.abs() < 0.001 {
        format!("{value:.3e}")
    } else if value.abs() < 10.0 {
        format!("{value:.3}")
    } else {
        format!("{value:.2}")
    }
}

pub fn comparable_parameters(test: Case, left: &Value, right: &Value) -> bool {
    if test == Case::CpuMulti {
        let (Some(left), Some(right)) = (left.as_object(), right.as_object()) else {
            return false;
        };
        return left
            .iter()
            .filter(|(key, _)| key.as_str() != "threads")
            .eq(right.iter().filter(|(key, _)| key.as_str() != "threads"));
    }
    left == right
}

pub fn compare(left: &Report, right: &Report, color: ColorMode) {
    println!("A · {}", host_summary(&left.host));
    println!("B · {}", host_summary(&right.host));
    for (tool, version_a) in &left.tools {
        if let Some(version_b) = right.tools.get(tool)
            && version_a != version_b
        {
            println!(
                "Разные версии {tool} · A {} · B {}",
                clean(version_a),
                clean(version_b)
            );
        }
    }
    let mut table = table(color, &["Тест", "Метрика", "A", "B", "B к A"]);
    let tests: BTreeSet<_> = left
        .measurements
        .iter()
        .chain(&right.measurements)
        .map(|row| (row.test, row.target.as_deref()))
        .collect();
    for (test, target) in tests {
        let (a, b) = (
            left.measurement(test, target),
            right.measurement(test, target),
        );
        let metrics_a = a.map_or(&[][..], |row| row.outcome.metrics());
        let metrics_b = b.map_or(&[][..], |row| row.outcome.metrics());
        if metrics_a.is_empty() && metrics_b.is_empty() {
            if a.is_some() || b.is_some() {
                table.add_row([
                    Cell::new(test_label(test, target)),
                    Cell::new(""),
                    status_cell(a),
                    status_cell(b),
                    Cell::new("—"),
                ]);
            }
            continue;
        }
        let mut kinds: Vec<_> = metrics_a.iter().map(|metric| metric.kind).collect();
        for metric in metrics_b {
            if !kinds.contains(&metric.kind) {
                kinds.push(metric.kind);
            }
        }
        let mismatch = a
            .zip(b)
            .is_some_and(|(a, b)| !comparable_parameters(test, &a.parameters, &b.parameters));
        let label = match a.zip(b) {
            Some((a, b))
                if test == Case::CpuMulti && a.parameters["threads"] != b.parameters["threads"] =>
            {
                format!(
                    "{} ({} → {} потоков)",
                    test.label(),
                    a.parameters["threads"],
                    b.parameters["threads"]
                )
            }
            _ => test_label(test, target),
        };
        for (index, kind) in kinds.into_iter().enumerate() {
            let get = |metrics: &[Metric]| {
                metrics
                    .iter()
                    .find(|metric| metric.kind == kind)
                    .map(|metric| metric.value)
            };
            let (value_a, value_b) = (get(metrics_a), get(metrics_b));
            let display = |value: Option<f64>, row: Option<&Measurement>| match value {
                Some(value) => metric_cell(kind, value),
                None if row.is_some_and(|row: &Measurement| !row.outcome.metrics().is_empty()) => {
                    Cell::new("—")
                }
                None => status_cell(row),
            };
            let change = match value_a.zip(value_b) {
                Some(_) if mismatch => Cell::new("Разные параметры").fg(Color::Yellow),
                Some((a, b)) => delta(kind, a, b),
                None => Cell::new("—"),
            };
            table.add_row([
                Cell::new(if index == 0 { &label } else { "" }),
                Cell::new(kind.label()),
                display(value_a, a),
                display(value_b, b),
                change,
            ]);
        }
    }
    println!("{table}");
}

fn delta(kind: MetricKind, before: f64, after: f64) -> Cell {
    if matches!(kind, MetricKind::PacketsSent | MetricKind::PacketsReceived) {
        return Cell::new("—");
    }
    let diff = after - before;
    if diff == 0.0 {
        return Cell::new("0");
    }
    let percentage_points = matches!(
        kind,
        MetricKind::CpuStealPercent | MetricKind::PacketLossPercent
    );
    let text = if before > 0.0 && !percentage_points {
        format!("{:+.2}%", 100.0 * diff / before)
    } else {
        let sign = if diff > 0.0 { "+" } else { "" };
        let unit = if percentage_points { " п.п." } else { "" };
        format!("{sign}{}{unit}", format_value(diff))
    };
    let better = (diff > 0.0) == kind.higher_is_better();
    Cell::new(text).fg(if better { Color::Green } else { Color::Red })
}

fn metric_cell(kind: MetricKind, value: f64) -> Cell {
    if matches!(kind, MetricKind::PacketsSent | MetricKind::PacketsReceived) {
        return Cell::new(format!("{value:.0}"));
    }
    let cell = Cell::new(format_value(value));
    if kind != MetricKind::PacketLossPercent {
        return cell;
    }
    let color = if value >= 100.0 {
        Color::Red
    } else if value > 0.0 {
        Color::Yellow
    } else {
        Color::Green
    };
    cell.fg(color)
}
