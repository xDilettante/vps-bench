#[cfg(any(target_os = "linux", test))]
use crate::bench::MetricKind;
use crate::bench::{Metric, cpu_args, sysbench_interval};
use crate::progress::Stage;
#[cfg(target_os = "linux")]
use rustix::{
    io::Errno,
    thread::clock_nanosleep_absolute,
    time::{ClockId, Timespec, clock_gettime},
};
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

pub const INTERVAL_US: u32 = 1_000;
#[cfg(target_os = "linux")]
const TICKS_PER_SECOND: u32 = 1_000_000 / INTERVAL_US;
const LOAD_STARTUP_SECONDS: u32 = 10;

#[cfg(target_os = "linux")]
pub fn run(seconds: u32, stage: &Stage<'_>) -> Result<Vec<Metric>, String> {
    let planned = seconds * TICKS_PER_SECOND;
    stage.reset_elapsed();
    stage.update("Замер пробуждения потока");
    let samples = if stage.enabled() {
        thread::scope(|scope| {
            let (sender, receiver) = sync_channel(1);
            let display = thread::Builder::new()
                .spawn_scoped(scope, move || {
                    for (delay, count) in receiver {
                        let value = micros(delay);
                        stage.sample(value, format!("последний {value:.1} мкс · замеров {count}"));
                    }
                })
                .map_err(|err| format!("Не удалось запустить вывод замеров · {err}"))?;
            let samples = collect(planned, Some(&sender));
            drop(sender);
            display
                .join()
                .map_err(|_| "Прерван вывод замеров пробуждения")?;
            samples
        })?
    } else {
        collect(planned, None)?
    };
    summarize(samples, planned)
}

#[cfg(not(target_os = "linux"))]
pub fn run(_seconds: u32, _stage: &Stage<'_>) -> Result<Vec<Metric>, String> {
    Err("Для замера пробуждения требуется Linux".into())
}

#[cfg(target_os = "linux")]
fn collect(
    planned: u32,
    progress: Option<&SyncSender<(Duration, usize)>>,
) -> Result<Vec<Duration>, String> {
    const CLOCK: ClockId = ClockId::Monotonic;
    let period = Duration::from_micros(u64::from(INTERVAL_US));
    let mut samples = Vec::with_capacity(planned as usize);
    let start = clock_gettime(CLOCK);
    let mut tick = 1;
    let mut last_update = Duration::ZERO;
    while tick <= planned {
        let offset = Timespec::try_from(period * tick).expect("bounded wakeup test duration");
        let deadline = start + offset;
        loop {
            match clock_nanosleep_absolute(CLOCK, &deadline) {
                Ok(()) => break,
                Err(Errno::INTR) => continue,
                Err(err) => return Err(format!("Таймер пробуждения · {err}")),
            }
        }
        let woke = clock_gettime(CLOCK);
        let delay = Duration::try_from(woke - deadline).unwrap_or_default();
        samples.push(delay);
        let elapsed = Duration::try_from(woke - start).unwrap_or_default();
        tick = next_tick(tick, elapsed, planned);
        if let Some(progress) = progress
            && elapsed.saturating_sub(last_update) >= Duration::from_secs(1)
        {
            let _ = progress.try_send((delay, samples.len()));
            last_update = elapsed;
        }
    }
    Ok(samples)
}

#[cfg(any(target_os = "linux", test))]
pub fn next_tick(tick: u32, elapsed: Duration, planned: u32) -> u32 {
    let elapsed_ticks = elapsed.as_micros() / u128::from(INTERVAL_US);
    tick.max(elapsed_ticks.min(u128::from(planned)) as u32) + 1
}

#[cfg(any(target_os = "linux", test))]
fn micros(delay: Duration) -> f64 {
    delay.as_nanos() as f64 / 1_000.0
}

#[cfg(any(target_os = "linux", test))]
pub fn summarize(mut samples: Vec<Duration>, planned: u32) -> Result<Vec<Metric>, String> {
    let count = samples.len();
    if count == 0 {
        return Err("Нет замеров пробуждения".into());
    }
    samples.sort_unstable();
    let p99 = samples[(count * 99).div_ceil(100) - 1];
    let maximum = samples[count - 1];
    let missed = (planned as usize).saturating_sub(count);
    Ok(vec![
        Metric::new(MetricKind::WakeupP99Us, micros(p99)),
        Metric::new(MetricKind::WakeupMaxUs, micros(maximum)),
        Metric::new(MetricKind::WakeupSamples, count as f64),
        Metric::new(MetricKind::WakeupMissedIntervals, missed as f64),
    ])
}

pub fn under_cpu_load(
    seconds: u32,
    threads: usize,
    stage: &Stage<'_>,
) -> Result<Vec<Metric>, String> {
    stage.update("Ожидание нагрузки sysbench");
    let log = NamedTempFile::new().map_err(|err| format!("sysbench · {err}"))?;
    let stdout = log.reopen().map_err(|err| format!("sysbench · {err}"))?;
    let stderr = stdout
        .try_clone()
        .map_err(|err| format!("sysbench · {err}"))?;
    // Keep the loader alive through the probe and stop this owned process afterwards.
    let mut child = Command::new("sysbench")
        .args(cpu_args(seconds + LOAD_STARTUP_SECONDS + 1, threads, true))
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .map_err(|err| format!("Не удалось запустить нагрузку sysbench · {err}"))?;
    let result = (|| {
        let deadline = Instant::now() + Duration::from_secs(u64::from(LOAD_STARTUP_SECONDS));
        loop {
            if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
                return Err(format!("sysbench завершился до начала нагрузки ({status})"));
            }
            let output = std::fs::read_to_string(log.path()).map_err(|err| err.to_string())?;
            if output
                .lines()
                .any(|line| sysbench_interval(line, false).is_some_and(|rate| rate > 0.0))
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "sysbench не подтвердил нагрузку за {LOAD_STARTUP_SECONDS} с"
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
        let metrics = run(seconds, stage)?;
        if child.try_wait().map_err(|err| err.to_string())?.is_some() {
            return Err("sysbench завершился до окончания замера пробуждения".into());
        }
        Ok(metrics)
    })();
    let _ = child.kill();
    let reaped = child
        .wait()
        .map_err(|err| format!("Не удалось дождаться sysbench · {err}"));
    let metrics = result?;
    reaped?;
    Ok(metrics)
}
