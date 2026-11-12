use crate::bench::{Metric, MetricKind, capture, timed_command};
use crate::progress::{RunProgress, Stage};
use crate::report::{Case, Measurement, Outcome};
use serde_json::json;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

const INTERVAL_MS: u32 = 200;

#[derive(Clone, Copy)]
pub struct Load<'a> {
    pub host: &'a str,
    pub port: u16,
    pub streams: u32,
}

pub struct Probe {
    pub address: Option<String>,
    pub metrics: Vec<Metric>,
}

pub fn run(
    targets: &[String],
    count: u32,
    load: Option<Load<'_>>,
    unavailable: &BTreeMap<&str, String>,
    progress: &mut RunProgress,
) -> Vec<Measurement> {
    let mut measurements = Vec::new();
    let probe_seconds = (count * INTERVAL_MS).div_ceil(1000);
    let probe_deadline = probe_seconds + 3;
    let load_seconds = probe_deadline + 5;

    for test in [Case::PingIdle, Case::PingUpload, Case::PingDownload] {
        let server = load.filter(|_| test != Case::PingIdle);
        let mut profile = json!({"count": count, "interval_ms": INTERVAL_MS});
        if let Some(server) = server {
            profile["load"] = json!({
                "host": server.host, "port": server.port,
                "streams": server.streams, "seconds": load_seconds
            });
        }
        let first = measurements.len();
        let stage = progress.start(
            test.label(),
            &test.workload(&profile),
            Some(if server.is_some() {
                load_seconds
            } else {
                probe_seconds
            }),
        );
        let failure = unavailable
            .get("ping")
            .or_else(|| server.and(unavailable.get("iperf3")));
        let results = if let Some(reason) = failure {
            Err(reason.clone())
        } else if test == Case::PingIdle || server.is_some() {
            let probe = || {
                stage.update(format!("Ожидание ответов · узлов {}", targets.len()));
                ping_all(targets, count, probe_deadline, &stage)
            };
            match server {
                Some(server) => under_load(
                    server,
                    load_seconds,
                    test == Case::PingDownload,
                    &stage,
                    probe,
                ),
                None => Ok(probe()),
            }
        } else {
            Ok(Vec::new())
        };

        let mut results = match results {
            Ok(rows) => Ok(rows.into_iter()),
            Err(reason) => Err(reason),
        };
        for target in targets {
            let mut parameters = profile.clone();
            let outcome = match &mut results {
                Ok(rows) => match rows.next() {
                    Some(Ok(probe)) => {
                        parameters["resolved_address"] = json!(probe.address);
                        Outcome::Completed {
                            metrics: probe.metrics,
                        }
                    }
                    Some(Err(reason)) => Outcome::Failed { reason },
                    None => Outcome::Skipped {
                        reason: "Укажите --iperf-host для нагрузки".into(),
                    },
                },
                Err(reason) => Outcome::Failed {
                    reason: reason.clone(),
                },
            };
            measurements.push(Measurement::new(
                test,
                Some(target.clone()),
                parameters,
                outcome,
            ));
        }
        stage.finish(
            measurements[first..]
                .iter()
                .map(|measurement| &measurement.outcome),
        );
    }
    measurements
}

fn ping_all(
    targets: &[String],
    count: u32,
    deadline: u32,
    stage: &Stage,
) -> Vec<Result<Probe, String>> {
    thread::scope(|scope| {
        let workers: Vec<_> = targets
            .iter()
            .map(|target| {
                thread::Builder::new().spawn_scoped(scope, || ping(target, count, deadline, stage))
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .map_err(|err| format!("ping: не удалось запустить поток: {err}"))?
                    .join()
                    .map_err(|_| "ping: прерван поток замера".to_owned())?
            })
            .collect()
    })
}

fn ping(target: &str, count: u32, deadline: u32, stage: &Stage) -> Result<Probe, String> {
    let interval = format!("{:.3}", f64::from(INTERVAL_MS) / 1000.0);
    let mut command = timed_command("ping", deadline + 3);
    command.args([
        "-n",
        "-i",
        &interval,
        "-c",
        &count.to_string(),
        "-W",
        "1",
        "-w",
        &deadline.to_string(),
        "--",
        target,
    ]);
    let mut replies = 0;
    let mut observe = |line: &str| {
        if let Some(rtt) = reply_rtt(line) {
            replies += 1;
            stage.update(format!(
                "{target} · RTT {rtt:.3} мс · ответов {replies}/{count}"
            ));
        }
    };
    let output = capture(&mut command, stage.enabled().then_some(&mut observe))
        .map_err(|err| format!("ping: {err}"))?;
    // iputils also uses code 1 when some or all echo replies are missing.
    if !matches!(output.status.code(), Some(0 | 1)) {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ping ({}): {}", output.status, detail.trim()));
    }
    parse_ping(&String::from_utf8_lossy(&output.stdout))
}

pub fn parse_ping(text: &str) -> Result<Probe, String> {
    let address = text.lines().next().and_then(|line| {
        let (_, rest) = line.strip_prefix("PING ")?.split_once(" (")?;
        rest.split_once(')').map(|(address, _)| address.to_owned())
    });
    let counts = text.lines().find_map(|line| {
        let (sent, rest) = line.split_once(" packets transmitted, ")?;
        let received = rest.split_whitespace().next()?;
        Some((sent.parse::<usize>().ok()?, received.parse::<usize>().ok()?))
    });
    let (sent, received) = counts
        .filter(|&(sent, received)| sent > 0 && received <= sent)
        .ok_or("ping: отсутствует итоговая статистика iputils")?;
    let mut rtts: Vec<f64> = text.lines().filter_map(reply_rtt).collect();
    if rtts.len() != received {
        return Err(format!(
            "ping: распознано {} RTT из {received} ответов",
            rtts.len()
        ));
    }
    let mut metrics = vec![
        Metric::new(MetricKind::PacketsSent, sent as f64),
        Metric::new(MetricKind::PacketsReceived, received as f64),
        Metric::new(
            MetricKind::PacketLossPercent,
            100.0 * (sent - received) as f64 / sent as f64,
        ),
    ];
    if received > 0 {
        rtts.sort_by(f64::total_cmp);
        for (percent, metric) in [
            (50, MetricKind::RttP50Ms),
            (95, MetricKind::RttP95Ms),
            (99, MetricKind::RttP99Ms),
        ] {
            metrics.push(Metric::new(
                metric,
                rtts[(received * percent).div_ceil(100) - 1],
            ));
        }
    }
    Ok(Probe { address, metrics })
}

fn reply_rtt(line: &str) -> Option<f64> {
    if !line.contains("icmp_seq=") || line.contains("DUP!") {
        return None;
    }
    let (_, time) = line
        .split_once(" time=")
        .or_else(|| line.split_once(" time<"))?;
    time.split_once(" ms")?
        .0
        .parse::<f64>()
        .ok()
        .filter(|rtt| rtt.is_finite() && *rtt >= 0.0)
}

fn under_load<T>(
    server: Load<'_>,
    seconds: u32,
    reverse: bool,
    stage: &Stage,
    capture: impl FnOnce() -> T,
) -> Result<T, String> {
    stage.update("Ожидание передачи iperf3");
    let log = NamedTempFile::new().map_err(|err| format!("iperf3: {err}"))?;
    let mut command = timed_command("iperf3", seconds + 12);
    command.args([
        format!("--client={}", server.host),
        format!("--port={}", server.port),
        format!("--parallel={}", server.streams),
        format!("--time={seconds}"),
        "--forceflush".into(),
        "--interval=1".into(),
        "--connect-timeout=3000".into(),
    ]);
    if reverse {
        command.arg("--reverse");
    }
    let output = log.reopen().map_err(|err| format!("iperf3: {err}"))?;
    let error = output.try_clone().map_err(|err| format!("iperf3: {err}"))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(output)
        .stderr(error)
        .spawn()
        .map_err(|err| format!("iperf3: {err}"))?;

    let result = (|| -> Result<T, String> {
        let expires = Instant::now() + Duration::from_secs(8);
        loop {
            if child.try_wait().map_err(|err| err.to_string())?.is_some() {
                return Err("iperf3 завершился до начала замера RTT".into());
            }
            let output = std::fs::read_to_string(log.path()).map_err(|err| err.to_string())?;
            if has_load(&output) {
                break;
            }
            if Instant::now() >= expires {
                return Err("iperf3 не подтвердил передачу данных за 8 с".into());
            }
            thread::sleep(Duration::from_millis(50));
        }
        let result = capture();
        if child.try_wait().map_err(|err| err.to_string())?.is_some() {
            return Err("iperf3 завершился до окончания замера RTT".into());
        }
        stage.update("ICMP завершён · ожидание итога TCP");
        Ok(result)
    })();
    let status = child.wait().map_err(|err| format!("iperf3: {err}"))?;
    if !status.success() {
        let output = std::fs::read_to_string(log.path()).unwrap_or_default();
        let detail = output
            .lines()
            .find(|line| line.starts_with("iperf3:"))
            .unwrap_or_default();
        return Err(format!("Нагрузка iperf3 ({status}): {detail}"));
    }
    result
}

pub fn has_load(output: &str) -> bool {
    output
        .lines()
        .filter(|line| line.starts_with('['))
        .any(|line| {
            let mut columns = line.split_whitespace();
            let interval = columns.find(|word| word.contains('-'));
            interval
                .and_then(|word| word.split_once('-'))
                .and_then(|(start, end)| {
                    Some((start.parse::<f64>().ok()?, end.parse::<f64>().ok()?))
                })
                .is_some_and(|(start, end)| end > start && end - start <= 1.2)
                && columns.next() == Some("sec")
                && columns
                    .next()
                    .and_then(|size| size.parse::<f64>().ok())
                    .is_some_and(|size| size > 0.0)
        })
}
