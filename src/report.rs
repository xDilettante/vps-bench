use crate::bench::{CPU_PRIME, Metric, MetricKind};
use crate::disk::BlockDevice;
use crate::host::Host;
use crate::latency::{LoadRttP95, annotate_rtt_p95_load};
use crate::quality::rating_cell;
use crate::stability::CpuSeries;
use comfy_table::{Cell, Color, ContentArrangement, Table, modifiers, presets};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{IsTerminal, Write, stdout};
use std::path::Path;
use tempfile::NamedTempFile;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Section {
    Cpu,
    Stability,
    Crypto,
    Memory,
    Disk,
    Network,
    Latency,
}

impl Section {
    pub const ORDER: [Self; 7] = [
        Self::Cpu,
        Self::Stability,
        Self::Crypto,
        Self::Memory,
        Self::Disk,
        Self::Latency,
        Self::Network,
    ];

    pub fn heading(self, color: bool) -> String {
        let title = match self {
            Self::Cpu => "CPU · Процессор",
            Self::Stability => "Стабильность · CPU",
            Self::Crypto => "Crypto · Криптография",
            Self::Memory => "RAM · Оперативная память",
            Self::Disk => "Диск · Накопитель",
            Self::Network => "Сеть · Пропускная способность TCP",
            Self::Latency => "Сеть · Задержка и потери ICMP",
        };
        if color {
            format!("\x1b[1;36m{title}\x1b[0m")
        } else {
            title.to_owned()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Case {
    CpuSingle,
    CpuMulti,
    CpuStability,
    CryptoSha256,
    CryptoAes256Gcm,
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
    pub const ALL: [Self; 16] = [
        Self::CpuSingle,
        Self::CpuMulti,
        Self::CpuStability,
        Self::CryptoSha256,
        Self::CryptoAes256Gcm,
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
            Self::CpuSingle => "Однопоточная производительность",
            Self::CpuMulti => "Многопоточная производительность",
            Self::CpuStability => "Длительная нагрузка CPU",
            Self::CryptoSha256 => "SHA-256 · Хеширование",
            Self::CryptoAes256Gcm => "AES-256-GCM · Шифрование",
            Self::RamRead | Self::DiskSeqRead => "Последовательное чтение",
            Self::RamWrite | Self::DiskSeqWrite => "Последовательная запись",
            Self::DiskRandRead => "Случайное чтение",
            Self::DiskRandWrite => "Случайная запись",
            Self::DiskSync => "Случайная запись с fdatasync",
            Self::NetUp => "Отправка с VPS · один поток",
            Self::NetDown => "Приём на VPS · один поток",
            Self::NetUpMulti => "Отправка с VPS · несколько потоков",
            Self::NetDownMulti => "Приём на VPS · несколько потоков",
            Self::PingIdle => "В простое",
            Self::PingUpload => "При отправке TCP",
            Self::PingDownload => "При приёме TCP",
        }
    }

    pub fn section(self) -> Section {
        match self {
            Self::CpuSingle | Self::CpuMulti => Section::Cpu,
            Self::CpuStability => Section::Stability,
            Self::CryptoSha256 | Self::CryptoAes256Gcm => Section::Crypto,
            Self::RamRead | Self::RamWrite => Section::Memory,
            Self::DiskSeqWrite
            | Self::DiskSeqRead
            | Self::DiskRandRead
            | Self::DiskRandWrite
            | Self::DiskSync => Section::Disk,
            Self::NetUp | Self::NetDown | Self::NetUpMulti | Self::NetDownMulti => Section::Network,
            Self::PingIdle | Self::PingUpload | Self::PingDownload => Section::Latency,
        }
    }

    pub fn workload(self, parameters: &Value) -> String {
        let amount = |key| {
            parameters[key]
                .as_u64()
                .map_or("—".to_owned(), |n| n.to_string())
        };
        let seconds = amount("seconds");
        match self.section() {
            Section::Cpu => format!(
                "sysbench cpu · простые числа до {CPU_PRIME} · {} · {seconds} с",
                threads(&parameters["threads"])
            ),
            Section::Stability => format!(
                "sysbench cpu · простые числа до {} · {} · {seconds} с · шаг {} с",
                amount("cpu_max_prime"),
                threads(&parameters["threads"]),
                amount("sample_seconds")
            ),
            Section::Crypto => {
                let mode = if parameters["aead"].as_bool() == Some(true) {
                    " -aead"
                } else {
                    ""
                };
                format!(
                    "openssl speed{mode} · блок {} КиБ · {} · {seconds} с, шаг {} с",
                    amount("block_kib"),
                    threads(&parameters["threads"]),
                    amount("sample_seconds")
                )
            }
            Section::Memory => format!(
                "sysbench memory · буфер {} МиБ · {} · до {seconds} с",
                amount("block_mib"),
                threads(&parameters["threads"])
            ),
            Section::Disk => {
                let block = match parameters["block_kib"].as_u64() {
                    Some(1024) => "1 МиБ".to_owned(),
                    Some(kib) => format!("{kib} КиБ"),
                    None => "—".to_owned(),
                };
                let io = match parameters["direct"].as_bool() {
                    Some(true) => "прямой I/O",
                    Some(false) => "буферизованный I/O",
                    None => "режим I/O неизвестен",
                };
                let limit = match self {
                    Self::DiskSeqWrite => "один проход · fsync в конце".to_owned(),
                    Self::DiskSync => format!("до {seconds} с · fdatasync после блока"),
                    Self::DiskSeqRead | Self::DiskRandRead => format!("{seconds} с · циклически"),
                    _ => format!("до {seconds} с"),
                };
                format!(
                    "fio · блок {block} · очередь {} · {io} · файл {} МиБ · {limit}",
                    amount("iodepth"),
                    amount("size_mib")
                )
            }
            Section::Network => match endpoint(parameters) {
                Some(server) => format!(
                    "iperf3 · {server} · {} · {seconds} с",
                    threads(&parameters["streams"])
                ),
                None => "iperf3 · укажите сервер через --iperf-host".to_owned(),
            },
            Section::Latency => {
                let mut workload = format!(
                    "ping · {} проб на узел · интервал {} мс",
                    amount("count"),
                    amount("interval_ms")
                );
                if let Some(address) = parameters["resolved_address"].as_str() {
                    workload.push_str(&format!(" · IP {}", clean(address)));
                }
                if let Some(server) = endpoint(&parameters["load"]) {
                    workload.push_str(&format!(
                        " · нагрузка iperf3 {server} · {} · {} с",
                        threads(&parameters["load"]["streams"]),
                        parameters["load"]["seconds"]
                    ));
                }
                workload
            }
        }
    }

    pub fn tool(self) -> &'static str {
        match self.section() {
            Section::Cpu | Section::Stability | Section::Memory => "sysbench",
            Section::Crypto => "openssl",
            Section::Disk => "fio",
            Section::Latency => "ping",
            Section::Network => "iperf3",
        }
    }
}

fn threads(value: &Value) -> String {
    let Some(count) = value.as_u64() else {
        return "число потоков неизвестно".into();
    };
    let unit = match (count % 100, count % 10) {
        (11..=14, _) => "потоков",
        (_, 1) => "поток",
        (_, 2..=4) => "потока",
        _ => "потоков",
    };
    format!("{count} {unit}")
}

fn endpoint(parameters: &Value) -> Option<String> {
    let host = clean(parameters["host"].as_str()?);
    let port = parameters["port"].as_u64()?;
    if host.contains(':') {
        Some(format!("[{host}]:{port}"))
    } else {
        Some(format!("{host}:{port}"))
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Measurement {
    pub test: Case,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_p95_load: Option<LoadRttP95>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workload: Option<String>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl Measurement {
    pub fn new(test: Case, target: Option<String>, parameters: Value, outcome: Outcome) -> Self {
        Self {
            test,
            target,
            rtt_p95_load: None,
            workload: Some(test.workload(&parameters)),
            parameters,
            outcome,
        }
    }

    fn workload(&self) -> String {
        let workload = self
            .workload
            .as_deref()
            .map_or_else(|| self.test.workload(&self.parameters), clean);
        if let Outcome::Completed {
            cpu_series: Some(series),
            ..
        } = &self.outcome
        {
            format!("{workload}\n{}", series.coverage())
        } else {
            workload
        }
    }
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
    Completed {
        metrics: Vec<Metric>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cpu_series: Option<CpuSeries>,
    },
    Skipped {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

impl Outcome {
    pub fn completed(metrics: Vec<Metric>) -> Self {
        Self::Completed {
            metrics,
            cpu_series: None,
        }
    }

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

    pub(crate) fn metrics(&self) -> &[Metric] {
        match self {
            Self::Completed { metrics, .. } => metrics,
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
    pub disk_source: Option<String>,
    pub disk_device: Option<BlockDevice>,
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
            disk_source: None,
            disk_device: None,
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
            let mut report: Self =
                serde_json::from_reader(std::io::BufReader::new(File::open(path)?))?;
            if report.schema_version != SCHEMA_VERSION {
                return Err(
                    format!("версия схемы {} не поддерживается", report.schema_version).into(),
                );
            }
            annotate_rtt_p95_load(&mut report.measurements);
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

    pub fn section_context(&self, section: Section) -> Option<String> {
        match section {
            Section::Cpu | Section::Stability => Some(format!(
                "{} · доступно {} vCPU",
                clean(self.host.cpu_model.as_deref().unwrap_or(&self.host.arch)),
                self.host.available_vcpus
            )),
            Section::Crypto => self.tools.get("openssl").map(|version| clean(version)),
            Section::Memory => {
                let mut details = Vec::new();
                if let Some(mib) = self.host.ram_mib {
                    details.push(format!("всего {:.2} ГиБ", mib as f64 / 1024.0));
                }
                if let Some(mib) = self.host.available_ram_mib {
                    details.push(format!("доступно {:.2} ГиБ", mib as f64 / 1024.0));
                }
                if let Some(mib) = self.host.used_swap_mib {
                    details.push(format!("занято swap {mib} МиБ"));
                }
                (!details.is_empty()).then(|| details.join(" · "))
            }
            Section::Disk => self.disk_context(),
            Section::Network | Section::Latency => None,
        }
    }

    fn disk_context(&self) -> Option<String> {
        let directory = self.disk_directory.as_deref()?;
        let mut mount = Vec::new();
        if let Some(source) = &self.disk_source {
            mount.push(format!("том {}", clean(source)));
        }
        if let Some(filesystem) = &self.disk_filesystem {
            mount.push(format!("ФС {}", clean(filesystem)));
        }
        if let Some(mib) = self.disk_available_mib {
            mount.push(format!("свободно {:.2} ГиБ", mib as f64 / 1024.0));
        }
        mount.push(format!("каталог {}", clean(directory)));
        let mut context = mount.join(" · ");
        if let Some(device) = &self.disk_device {
            let mut details = Vec::new();
            if let Some(model) = &device.model {
                details.push(clean(model));
            }
            if let Some(transport) = &device.transport {
                details.push(clean(transport));
            }
            if let Some(size) = device.size_bytes {
                details.push(format!("объём {:.1} ГиБ", size as f64 / 1024_f64.powi(3)));
            }
            if let Some(rotational) = device.rotational {
                let kind = if rotational {
                    "вращающийся"
                } else {
                    "без вращения"
                };
                details.push(format!(
                    "{kind} по данным гостя (ROTA={})",
                    u8::from(rotational)
                ));
            }
            if !details.is_empty() {
                context.push_str("\n  ");
                context.push_str(&details.join(" · "));
            }
        }
        Some(context)
    }

    pub fn print(&self, color: ColorMode, baseline: Option<&Report>) {
        println!(
            "VPS Bench · {} · {}",
            clean(&self.host.label),
            clean(&self.host.arch)
        );
        if let Some(baseline) = baseline {
            println!("Эталон · {}", host_summary(&baseline.host));
            print_tool_changes(baseline, self);
            print_scale_legend();
        } else {
            println!("Ориентиры для VPS · пороги под шкалой «Норма / Хорошо»");
            if self.measurements.iter().any(|row| {
                matches!(row.test.section(), Section::Network | Section::Latency)
                    && !row.outcome.metrics().is_empty()
            }) {
                println!("Сеть · оценка маршрута до указанного узла");
            }
        }
        for section in Section::ORDER {
            let mut rows = self
                .measurements
                .iter()
                .filter(|row| row.test.section() == section)
                .peekable();
            if rows.peek().is_none() {
                continue;
            }
            println!(
                "\n{}",
                section.heading(color.enabled(stdout().is_terminal()))
            );
            if let Some(context) = self.section_context(section) {
                println!("  {context}");
            }
            if section == Section::Latency {
                print_rtt_p95_load(self, baseline, color);
            }
            let headers: &[&str] = if baseline.is_some() {
                &[
                    "Замер и параметры",
                    "Показатель",
                    "Значение",
                    "Статус",
                    "К эталону",
                ]
            } else {
                &["Замер и параметры", "Показатель", "Значение", "Оценка"]
            };
            let mut table = table(color, headers);
            for row in rows {
                add_measurement(&mut table, row, baseline);
            }
            println!("{table}");
        }
    }

    fn measurement(&self, test: Case, target: Option<&str>) -> Option<&Measurement> {
        self.measurements
            .iter()
            .find(|row| row.test == test && row.target.as_deref() == target)
    }
}

fn add_measurement(table: &mut Table, row: &Measurement, baseline: Option<&Report>) {
    let label = format!(
        "{}\n{}",
        test_label(row.test, row.target.as_deref()),
        row.workload()
    );
    if row.outcome.metrics().is_empty() {
        let mut cells = vec![
            Cell::new(&label),
            Cell::new(""),
            Cell::new(clean(row.outcome.reason())),
            status_cell(Some(row)),
        ];
        if baseline.is_some() {
            cells.push(Cell::new("—"));
        }
        table.add_row(cells);
        return;
    }
    let reference = baseline.and_then(|report| report.measurement(row.test, row.target.as_deref()));
    let mismatch = reference
        .is_some_and(|base| !comparable_parameters(row.test, &base.parameters, &row.parameters));
    for (index, metric) in row.outcome.metrics().iter().enumerate() {
        let mut cells = vec![
            Cell::new(if index == 0 { &label } else { "" }),
            Cell::new(metric.kind.label()),
            metric_cell(metric.kind, metric.value),
        ];
        if baseline.is_some() {
            cells.push(if index == 0 {
                status_cell(Some(row))
            } else {
                Cell::new("")
            });
            cells.push(comparison_cell(
                metric.kind,
                reference.and_then(|base| metric_value(base.outcome.metrics(), metric.kind)),
                Some(metric.value),
                mismatch,
            ));
        } else {
            cells.push(rating_cell(row.test, &row.parameters, metric));
        }
        table.add_row(cells);
    }
}

fn print_rtt_p95_load(report: &Report, baseline: Option<&Report>, color: ColorMode) {
    let mut rows: Vec<_> = report
        .measurements
        .iter()
        .filter_map(|row| row.rtt_p95_load.as_ref().map(|summary| (row, summary)))
        .collect();
    if rows.is_empty() {
        return;
    }
    rows.sort_by_key(|(row, _)| (row.target.as_deref(), row.test));
    let mut headers = vec!["Узел", "Фаза", "Простой", "Нагрузка", "Δ p95"];
    if baseline.is_some() {
        headers.push("Δ к эталону");
    }
    let mut table = table(color, &headers);
    for (row, summary) in rows {
        let target = row.target.as_deref();
        let mut cells = vec![
            Cell::new(target.map_or_else(String::new, clean)),
            Cell::new(if row.test == Case::PingUpload {
                "Отправка TCP"
            } else {
                "Приём TCP"
            }),
            Cell::new(rtt_ms(summary.idle_ms)),
            Cell::new(rtt_ms(summary.loaded_ms)),
            rtt_delta_cell(summary),
        ];
        if let Some(baseline) = baseline {
            cells.push(rtt_delta_comparison(
                baseline.measurement(row.test, target),
                Some(row),
            ));
        }
        table.add_row(cells);
    }
    println!("  RTT p95, мс · Δ = нагрузка − простой\n{table}\n");
}

fn print_rtt_p95_comparison(left: &Report, right: &Report, color: ColorMode) {
    let tests: BTreeSet<_> = left
        .measurements
        .iter()
        .chain(&right.measurements)
        .filter(|row| row.rtt_p95_load.is_some())
        .map(|row| (row.target.as_deref(), row.test))
        .collect();
    if tests.is_empty() {
        return;
    }
    let mut table = table(color, &["Узел и фаза", "VPS A", "VPS B", "Δ B − Δ A"]);
    for (target, test) in tests {
        let a = left.measurement(test, target);
        let b = right.measurement(test, target);
        table.add_row([
            Cell::new(test_label(test, target)),
            rtt_pair_cell(a),
            rtt_pair_cell(b),
            rtt_delta_comparison(a, b),
        ]);
    }
    println!("  RTT p95, мс · Δ = нагрузка − простой\n{table}\n");
}

fn rtt_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "—".into(), |ms| format!("{ms:.3}"))
}

fn rtt_delta_style(value: f64) -> (String, Color) {
    let text = format!("{value:+.3}");
    match text.as_str() {
        "+0.000" | "-0.000" => ("0.000".into(), Color::Cyan),
        _ if value > 0.0 => (text, Color::Red),
        _ => (text, Color::Green),
    }
}

fn rtt_delta_cell(summary: &LoadRttP95) -> Cell {
    match summary.delta_ms {
        Some(value) => {
            let (text, color) = rtt_delta_style(value);
            Cell::new(text).fg(color)
        }
        None => Cell::new(summary.reason.as_deref().unwrap_or("—")).fg(Color::Yellow),
    }
}

fn rtt_pair_cell(row: Option<&Measurement>) -> Cell {
    let Some(summary) = row.and_then(|row| row.rtt_p95_load.as_ref()) else {
        return Cell::new("—");
    };
    let pair = format!(
        "{} → {}",
        rtt_ms(summary.idle_ms),
        rtt_ms(summary.loaded_ms)
    );
    match summary.delta_ms {
        Some(value) => {
            let (text, color) = rtt_delta_style(value);
            Cell::new(format!("{pair}\nΔ {text}")).fg(color)
        }
        None => Cell::new(format!(
            "{pair}\n{}",
            summary.reason.as_deref().unwrap_or("—")
        ))
        .fg(Color::Yellow),
    }
}

fn rtt_delta_comparison(a: Option<&Measurement>, b: Option<&Measurement>) -> Cell {
    let Some((a, b)) = a.zip(b) else {
        return Cell::new("—");
    };
    let delta = |row: &Measurement| row.rtt_p95_load.as_ref().and_then(|r| r.delta_ms);
    let Some((before, after)) = delta(a).zip(delta(b)) else {
        return Cell::new("—");
    };
    if !comparable_parameters(a.test, &a.parameters, &b.parameters) {
        return Cell::new("Разные параметры").fg(Color::Yellow);
    }
    let difference = after - before;
    if !difference.is_finite() {
        return Cell::new("—");
    }
    let (text, color) = rtt_delta_style(difference);
    Cell::new(text).fg(color)
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

impl ColorMode {
    pub fn enabled(self, is_terminal: bool) -> bool {
        match self {
            Self::Auto => {
                is_terminal
                    && std::env::var_os("NO_COLOR").is_none()
                    && std::env::var("TERM").as_deref() != Ok("dumb")
            }
            Self::Always => true,
            Self::Never => false,
        }
    }
}

fn table(color: ColorMode, headers: &[&str]) -> Table {
    let mut table = Table::new();
    let width = table.width().filter(|width| *width > 0).unwrap_or(96);
    table
        .load_preset(presets::UTF8_FULL_CONDENSED)
        .apply_modifier(modifiers::UTF8_ROUND_CORNERS)
        .set_width(width)
        .set_content_arrangement(ContentArrangement::Dynamic);
    if color.enabled(stdout().is_terminal()) {
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
    print_tool_changes(left, right);
    print_scale_legend();
    let tests: BTreeSet<_> = left
        .measurements
        .iter()
        .chain(&right.measurements)
        .map(|row| (row.test, row.target.as_deref()))
        .collect();
    for section in Section::ORDER {
        let mut section_tests = tests
            .iter()
            .filter(|(test, _)| test.section() == section)
            .peekable();
        if section_tests.peek().is_none() {
            continue;
        }
        println!(
            "\n{}",
            section.heading(color.enabled(stdout().is_terminal()))
        );
        if section == Section::Disk {
            for (name, report) in [("A", left), ("B", right)] {
                if let Some(context) = report.disk_context() {
                    println!("  {name} · {context}");
                }
            }
        }
        if section == Section::Latency {
            print_rtt_p95_comparison(left, right, color);
        }
        let mut table = table(
            color,
            &["Замер и параметры", "Показатель", "A", "B", "B к A"],
        );
        for &(test, target) in section_tests {
            add_comparison(
                &mut table,
                test,
                target,
                left.measurement(test, target),
                right.measurement(test, target),
            );
        }
        println!("{table}");
    }
}

fn print_scale_legend() {
    println!("Шкала · ← хуже · │ уровень эталона · лучше →");
}

fn print_tool_changes(left: &Report, right: &Report) {
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
}

fn add_comparison(
    table: &mut Table,
    test: Case,
    target: Option<&str>,
    a: Option<&Measurement>,
    b: Option<&Measurement>,
) {
    let label = test_label(test, target);
    let workload_a = a.map(Measurement::workload);
    let workload_b = b.map(Measurement::workload);
    let label = match (workload_a, workload_b) {
        (Some(a), Some(b)) if a != b => format!("{label}\nA · {a}\nB · {b}"),
        (Some(workload), _) | (_, Some(workload)) => format!("{label}\n{workload}"),
        _ => label,
    };
    let metrics_a = a.map_or(&[][..], |row| row.outcome.metrics());
    let metrics_b = b.map_or(&[][..], |row| row.outcome.metrics());
    if metrics_a.is_empty() && metrics_b.is_empty() {
        table.add_row([
            Cell::new(label),
            Cell::new(""),
            status_cell(a),
            status_cell(b),
            Cell::new("—"),
        ]);
        return;
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
    for (index, kind) in kinds.into_iter().enumerate() {
        let value_a = metric_value(metrics_a, kind);
        let value_b = metric_value(metrics_b, kind);
        let display = |value: Option<f64>, row: Option<&Measurement>| match value {
            Some(value) => metric_cell(kind, value),
            None if row.is_some_and(|row| !row.outcome.metrics().is_empty()) => Cell::new("—"),
            None => status_cell(row),
        };
        table.add_row([
            Cell::new(if index == 0 { &label } else { "" }),
            Cell::new(kind.label()),
            display(value_a, a),
            display(value_b, b),
            comparison_cell(kind, value_a, value_b, mismatch),
        ]);
    }
}

pub fn metric_value(metrics: &[Metric], kind: MetricKind) -> Option<f64> {
    metrics
        .iter()
        .find(|metric| metric.kind == kind)
        .map(|metric| metric.value)
}

fn comparison_cell(
    kind: MetricKind,
    before: Option<f64>,
    after: Option<f64>,
    mismatch: bool,
) -> Cell {
    match before.zip(after) {
        Some(_) if mismatch => Cell::new("Разные параметры").fg(Color::Yellow),
        Some((before, after)) => assessment(kind, before, after),
        None => Cell::new("—"),
    }
}

pub fn assessment(kind: MetricKind, before: f64, after: f64) -> Cell {
    if matches!(
        kind,
        MetricKind::PacketsSent | MetricKind::PacketsReceived | MetricKind::LastMinuteChangePercent
    ) {
        return Cell::new("—");
    }
    let diff = after - before;
    if !diff.is_finite() {
        return Cell::new("—");
    }
    let percentage_points = matches!(
        kind,
        MetricKind::CpuStealPercent | MetricKind::PacketLossPercent
    );
    let relative = (before > 0.0)
        .then(|| diff / before * 100.0)
        .filter(|n| n.is_finite());
    let (magnitude, span, margin, text) = if percentage_points {
        let sign = if diff > 0.0 { "+" } else { "" };
        (
            diff.abs(),
            Some(5.0),
            0.1,
            format!("{sign}{} п.п.", format_value(diff)),
        )
    } else if let Some(relative) = relative {
        (relative.abs(), Some(50.0), 5.0, format!("{relative:+.2}%"))
    } else {
        let sign = if diff > 0.0 { "+" } else { "" };
        (
            diff.abs(),
            None,
            0.0,
            format!("{sign}{}", format_value(diff)),
        )
    };
    let better = (diff > 0.0) == kind.higher_is_better();
    let close = magnitude <= margin;
    let (status, color) = if close {
        ("Близко", Color::Cyan)
    } else if better {
        ("Лучше", Color::Green)
    } else {
        ("Хуже", Color::Red)
    };
    let Some(span) = span else {
        let direction = if close {
            "│"
        } else if better {
            "→"
        } else {
            "←"
        };
        return Cell::new(format!("{direction} {status} {text}")).fg(color);
    };
    const SLOTS: usize = 5;
    let fill = if close {
        0
    } else {
        (magnitude / span * SLOTS as f64)
            .round()
            .clamp(1.0, SLOTS as f64) as usize
    };
    let (left, right) = if better { (0, fill) } else { (fill, 0) };
    let bar = format!(
        "{}{}│{}{}",
        "─".repeat(SLOTS - left),
        "█".repeat(left),
        "█".repeat(right),
        "─".repeat(SLOTS - right),
    );
    Cell::new(format!("{bar}\n{status} {text}")).fg(color)
}

fn metric_cell(kind: MetricKind, value: f64) -> Cell {
    if matches!(kind, MetricKind::PacketsSent | MetricKind::PacketsReceived) {
        return Cell::new(format!("{value:.0}"));
    }
    if kind == MetricKind::LastMinuteChangePercent {
        return Cell::new(format!("{value:+.2}"));
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
