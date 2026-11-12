mod bench;
mod host;
mod latency;
mod progress;
mod report;

#[cfg(test)]
mod tests;

use clap::{Args, Parser, Subcommand, ValueEnum};
use host::Host;
use progress::{RunProgress, Stage};
use report::{Case, ColorMode, Measurement, Outcome, Report, clean};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::{Builder, NamedTempFile};

#[derive(Parser)]
#[command(version, about = "Замеры CPU, памяти, диска и сети на Linux VPS")]
struct Cli {
    /// Цвет таблицы в терминале.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    color: ColorMode,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Запустить тесты и сохранить JSON.
    Run(RunArgs),
    /// Сравнить два JSON-отчёта.
    Compare {
        baseline: PathBuf,
        candidate: PathBuf,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Section {
    Cpu,
    Memory,
    Disk,
    Network,
    Latency,
}

#[derive(Args)]
struct RunArgs {
    /// Наборы тестов через запятую.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "cpu,memory,disk,network,latency"
    )]
    only: Vec<Section>,
    /// Имя VPS в отчёте. По умолчанию используется имя хоста.
    #[arg(long)]
    label: Option<String>,
    /// Путь к новому JSON-файлу.
    #[arg(long, short)]
    output: Option<PathBuf>,
    /// Скрыть анимацию и промежуточные значения.
    #[arg(long)]
    no_progress: bool,
    /// Длительность теста в секундах. Запись fio ограничена объёмом файла.
    #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u32).range(1..=120))]
    seconds: u32,
    /// Потоки многопоточного CPU-теста. По умолчанию используются доступные vCPU.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=1024))]
    threads: Option<u32>,
    /// Размер рабочего буфера RAM в МиБ, степень двойки.
    #[arg(long, default_value_t = 64, value_parser = memory_mib)]
    memory_mib: u32,
    /// Каталог на тестируемом томе.
    #[arg(long, default_value = ".")]
    disk_dir: PathBuf,
    /// Размер временного файла fio в МиБ.
    #[arg(long, default_value_t = 512, value_parser = clap::value_parser!(u32).range(16..=16_384))]
    disk_mib: u32,
    /// Добавить тест записи 4 КиБ с fdatasync после каждого блока.
    #[arg(long)]
    disk_sync: bool,
    /// Адрес сервера iperf3. При отсутствии сетевой тест пропускается.
    #[arg(long)]
    iperf_host: Option<String>,
    /// TCP-порт сервера iperf3.
    #[arg(long, default_value_t = 5201, value_parser = clap::value_parser!(u16).range(1..))]
    iperf_port: u16,
    /// Потоки многопоточного сетевого теста.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(2..=32))]
    streams: u32,
    /// IP или имена узлов для ICMP. Можно повторить параметр или указать список через запятую.
    #[arg(long = "ping-target", value_delimiter = ',', value_parser = host_arg)]
    ping_targets: Vec<String>,
    /// Число ICMP-пакетов на узел в каждой фазе замера RTT.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=300))]
    ping_count: u32,
}

fn host_arg(value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.starts_with('-')
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        Err("укажите IP или имя хоста без пробелов".into())
    } else {
        Ok(value.to_owned())
    }
}

fn memory_mib(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .ok()
        .filter(|n| n.is_power_of_two() && (4..=1024).contains(n))
        .ok_or_else(|| "размер памяти должен быть степенью двойки от 4 до 1024 МиБ".into())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.action {
        Action::Run(args) => run(args, cli.color),
        Action::Compare {
            baseline,
            candidate,
        } => (|| {
            report::compare(
                &Report::load(&baseline)?,
                &Report::load(&candidate)?,
                cli.color,
            );
            Ok(true)
        })(),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("Ошибка · {}", clean(&err));
            ExitCode::FAILURE
        }
    }
}

fn run(mut args: RunArgs, color: ColorMode) -> Result<bool, String> {
    if !cfg!(target_os = "linux") {
        return Err(
            "Для замеров требуется Linux. Команда compare читает отчёты на любой платформе.".into(),
        );
    }
    if args
        .iperf_host
        .as_ref()
        .is_some_and(|host| host_arg(host).is_err())
    {
        return Err("--iperf-host принимает IP или имя хоста без пробелов".into());
    }
    if args.only.iter().all(|section| *section == Section::Latency) && args.ping_targets.is_empty()
    {
        return Err("Для замера RTT укажите --ping-target".into());
    }
    let mut seen = BTreeSet::new();
    args.ping_targets
        .retain(|target| seen.insert(target.clone()));
    let latency = !args.ping_targets.is_empty()
        && (args.only.contains(&Section::Latency) || args.only.contains(&Section::Network));

    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64;
    let path = args
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("vps-bench-{started_at}.json")));
    match fs::symlink_metadata(&path) {
        Ok(_) => return Err(format!("Файл уже существует · {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("{}: {err}", path.display())),
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let output = NamedTempFile::new_in(parent).map_err(|e| format!("{}: {e}", path.display()))?;

    let mut report = Report::new(Host::read(args.label.clone()), started_at);
    let threads = args
        .threads
        .map_or(report.host.available_vcpus, |n| n as usize);
    let profiles: Vec<_> = Case::ALL
        .into_iter()
        .filter_map(|test| {
            let (section, parameters) = profile(test, &args, threads);
            (args.only.contains(&section) && (test != Case::DiskSync || args.disk_sync))
                .then_some((test, section, parameters))
        })
        .collect();
    let mut progress = RunProgress::new(
        &report.host.label,
        profiles.len() + if latency { 3 } else { 0 },
        color,
        args.no_progress,
    );
    let mut unavailable = BTreeMap::new();
    for (tool, required) in [
        (
            "sysbench",
            args.only.contains(&Section::Cpu) || args.only.contains(&Section::Memory),
        ),
        ("fio", args.only.contains(&Section::Disk)),
        (
            "iperf3",
            (args.only.contains(&Section::Network) || latency) && args.iperf_host.is_some(),
        ),
        ("ping", latency),
    ] {
        if required {
            let version_flag = if tool == "ping" { "-V" } else { "--version" };
            match bench::command(tool, [version_flag], 3, None) {
                Ok(output) => {
                    report.tools.insert(
                        tool.into(),
                        output.lines().next().unwrap_or_default().trim().into(),
                    );
                }
                Err(err) => {
                    unavailable.insert(tool, err);
                }
            }
        }
    }

    let stream_iperf = progress.enabled()
        && args.only.contains(&Section::Network)
        && report.tools.contains_key("iperf3")
        && bench::command("iperf3", ["--help"], 3, None)
            .is_ok_and(|help| help.split_whitespace().any(|arg| arg == "--json-stream"));

    if latency {
        let load = args.iperf_host.as_deref().map(|host| latency::Load {
            host,
            port: args.iperf_port,
            streams: args.streams,
        });
        report.measurements.extend(latency::run(
            &args.ping_targets,
            args.ping_count,
            load,
            &unavailable,
            &mut progress,
        ));
    }

    let disk = if args.only.contains(&Section::Disk) && !unavailable.contains_key("fio") {
        let mut prepare = || -> Result<_, std::io::Error> {
            let dir = fs::canonicalize(&args.disk_dir)?;
            report.disk_directory = Some(dir.display().to_string());
            if let Ok(stat) = bench::command(
                "stat",
                [
                    OsStr::new("--file-system"),
                    OsStr::new("--format=%T %a %S"),
                    OsStr::new("--"),
                    dir.as_os_str(),
                ],
                3,
                None,
            ) {
                let mut fields = stat.split_whitespace();
                report.disk_filesystem = fields.next().map(str::to_owned);
                let available_bytes = fields
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .zip(fields.next().and_then(|n| n.parse::<u64>().ok()))
                    .map(|(blocks, block_size)| blocks.saturating_mul(block_size));
                report.disk_available_mib = available_bytes.map(|n| n / (1024 * 1024));
                if let Some(available) = report.disk_available_mib
                    && available < u64::from(args.disk_mib)
                {
                    return Err(std::io::Error::other(format!(
                        "доступно {available} МиБ, для файла нужно {} МиБ",
                        args.disk_mib
                    )));
                }
            }
            Builder::new().prefix(".vps-bench-").tempdir_in(dir)
        };
        prepare().map_err(|err| format!("Каталог {}: {err}", args.disk_dir.display()))
    } else {
        Err("Диск не выбран или fio недоступен".into())
    };

    let mut disk_written = false;
    for (test, section, parameters) in profiles {
        let planned = (test != Case::DiskSeqWrite).then_some(args.seconds);
        let stage = progress.start(test.label(), planned);
        let outcome = if let Some(reason) = unavailable.get(test.tool()) {
            Outcome::Failed {
                reason: reason.clone(),
            }
        } else if section == Section::Network && args.iperf_host.is_none() {
            Outcome::Skipped {
                reason: "Укажите --iperf-host".into(),
            }
        } else if section == Section::Memory
            && host::memory_headroom_mib().is_some_and(|mib| mib < u64::from(args.memory_mib) + 32)
        {
            Outcome::Skipped {
                reason: format!(
                    "Для буфера {} МиБ и запаса 32 МиБ мало свободной RAM",
                    args.memory_mib
                ),
            }
        } else if section == Section::Disk && test != Case::DiskSeqWrite && !disk_written {
            Outcome::Skipped {
                reason: "Тестовый файл не подготовлен".into(),
            }
        } else {
            measure(
                test,
                &args,
                threads,
                disk.as_ref().map(|dir| dir.path()).map_err(String::as_str),
                stream_iperf,
                &stage,
            )
            .into()
        };
        if test == Case::DiskSeqWrite {
            disk_written = matches!(outcome, Outcome::Completed { .. });
        }
        stage.finish([&outcome]);
        report.measurements.push(Measurement {
            test,
            target: None,
            parameters,
            outcome,
        });
    }
    if let Ok(dir) = disk
        && let Err(err) = dir.close()
    {
        eprintln!(
            "Не удалось удалить тестовый каталог · {}",
            clean(&err.to_string())
        );
    }

    report
        .save(output, &path)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    report.print(color);
    println!("JSON · {}", clean(&path.display().to_string()));
    Ok(report.success())
}

fn profile(test: Case, args: &RunArgs, threads: usize) -> (Section, Value) {
    let seconds = args.seconds;
    let size = args.disk_mib;
    match test {
        Case::CpuSingle => (Section::Cpu, json!({"seconds": seconds, "threads": 1})),
        Case::CpuMulti => (
            Section::Cpu,
            json!({"seconds": seconds, "threads": threads}),
        ),
        Case::RamRead | Case::RamWrite => (
            Section::Memory,
            json!({"seconds": seconds, "block_mib": args.memory_mib, "threads": 1}),
        ),
        Case::DiskSeqWrite => (
            Section::Disk,
            json!({"size_mib": size, "block_kib": 1024, "iodepth": 1, "direct": true}),
        ),
        Case::DiskSeqRead => (
            Section::Disk,
            json!({"seconds": seconds, "size_mib": size, "block_kib": 1024, "iodepth": 1, "direct": true}),
        ),
        Case::DiskRandRead | Case::DiskRandWrite | Case::DiskSync => (
            Section::Disk,
            json!({"seconds": seconds, "size_mib": size, "block_kib": 4, "iodepth": 1, "direct": test != Case::DiskSync}),
        ),
        _ => {
            let streams = network_streams(test, args.streams);
            (
                Section::Network,
                json!({"seconds": seconds, "host": args.iperf_host, "port": args.iperf_port, "streams": streams}),
            )
        }
    }
}

fn network_streams(test: Case, multi: u32) -> u32 {
    if matches!(test, Case::NetUpMulti | Case::NetDownMulti) {
        multi
    } else {
        1
    }
}

fn measure(
    test: Case,
    args: &RunArgs,
    threads: usize,
    disk: Result<&Path, &str>,
    stream_iperf: bool,
    stage: &Stage,
) -> Result<Vec<bench::Metric>, String> {
    let seconds = args.seconds;
    match test {
        Case::CpuSingle => bench::cpu(seconds, 1, stage),
        Case::CpuMulti => bench::cpu(seconds, threads, stage),
        Case::RamRead => bench::memory(seconds, args.memory_mib, "read", stage),
        Case::RamWrite => bench::memory(seconds, args.memory_mib, "write", stage),
        Case::DiskSeqWrite => bench::disk(disk?, seconds, args.disk_mib, "write", stage),
        Case::DiskSeqRead => bench::disk(disk?, seconds, args.disk_mib, "read", stage),
        Case::DiskRandRead => bench::disk(disk?, seconds, args.disk_mib, "randread", stage),
        Case::DiskRandWrite => bench::disk(disk?, seconds, args.disk_mib, "randwrite", stage),
        Case::DiskSync => bench::disk(disk?, seconds, args.disk_mib, "sync", stage),
        _ => bench::network(
            args.iperf_host.as_deref().ok_or("Укажите --iperf-host")?,
            args.iperf_port,
            seconds,
            network_streams(test, args.streams),
            matches!(test, Case::NetDown | Case::NetDownMulti),
            stream_iperf,
            stage,
        ),
    }
}
