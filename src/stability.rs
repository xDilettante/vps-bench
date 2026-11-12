use crate::bench::{Metric, MetricKind, run_cpu, sysbench_interval};
use crate::progress::Stage;
use crate::report::Outcome;
use serde::{Deserialize, Serialize};

pub const MINUTE_SECONDS: u32 = 60;
const GRAPH_COLUMNS: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CpuSample {
    pub second: u32,
    pub events_per_second: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CpuSeries {
    pub samples: Vec<CpuSample>,
    pub first_minute_samples: usize,
    pub last_minute_samples: usize,
}

impl CpuSeries {
    pub fn coverage(&self) -> String {
        format!(
            "Отсчётов {} · первая минута {}/60 · последняя {}/60",
            self.samples.len(),
            self.first_minute_samples,
            self.last_minute_samples
        )
    }
}

pub fn run(seconds: u32, threads: usize, stage: &Stage<'_>) -> Result<Outcome, String> {
    let mut samples: Vec<CpuSample> = Vec::with_capacity(seconds as usize);
    let mut observe = |line: &str| {
        if let Some(sample) = parse_sample(line)
            && sample.second <= seconds
            && samples
                .last()
                .is_none_or(|previous| previous.second < sample.second)
        {
            samples.push(sample);
            if stage.enabled() {
                let CpuSample {
                    second,
                    events_per_second,
                } = sample;
                stage.sample(
                    events_per_second,
                    format!(
                        "{events_per_second:.1} событий/с · {second}/{seconds} с\n  {}",
                        sparkline(&samples, seconds)
                    ),
                );
            }
        }
    };
    let cpu = run_cpu(seconds, threads, Some(&mut observe))?;
    let (mut metrics, series) = summarize(samples, seconds)?;
    metrics.extend(
        cpu.into_iter()
            .filter(|metric| metric.kind == MetricKind::CpuStealPercent),
    );
    Ok(Outcome::Completed {
        metrics,
        cpu_series: Some(series),
    })
}

pub fn parse_sample(line: &str) -> Option<CpuSample> {
    let (time, _) = line.strip_prefix('[')?.split_once(']')?;
    let second = time.trim().strip_suffix('s')?.parse().ok()?;
    let events_per_second = sysbench_interval(line, false)?;
    (second > 0).then_some(CpuSample {
        second,
        events_per_second,
    })
}

pub fn summarize(
    samples: Vec<CpuSample>,
    seconds: u32,
) -> Result<(Vec<Metric>, CpuSeries), String> {
    use MetricKind::*;

    let first = sorted_rates(samples.iter().filter(|s| s.second <= MINUTE_SECONDS));
    let last = sorted_rates(
        samples
            .iter()
            .filter(|s| s.second > seconds.saturating_sub(MINUTE_SECONDS)),
    );
    let first_median = median(&first).ok_or("sysbench · нет отсчётов первой минуты")?;
    let last_median = median(&last).ok_or("sysbench · нет отсчётов последней минуты")?;
    let all = sorted_rates(samples.iter());
    let p50 = median(&all).ok_or("sysbench · нет секундных отсчётов")?;
    let p10 = all[all.len().div_ceil(10) - 1];
    let mut metrics = vec![
        Metric::new(EventsPerSecondP50, p50),
        Metric::new(EventsPerSecondP10, p10),
        Metric::new(FirstMinuteMedianEventsPerSecond, first_median),
        Metric::new(LastMinuteMedianEventsPerSecond, last_median),
    ];
    if first_median > 0.0 {
        let change = (last_median - first_median) / first_median * 100.0;
        if change.is_finite() {
            metrics.push(Metric::new(LastMinuteChangePercent, change));
        }
    }
    Ok((
        metrics,
        CpuSeries {
            samples,
            first_minute_samples: first.len(),
            last_minute_samples: last.len(),
        },
    ))
}

fn sorted_rates<'a>(samples: impl Iterator<Item = &'a CpuSample>) -> Vec<f64> {
    let mut rates: Vec<_> = samples.map(|sample| sample.events_per_second).collect();
    rates.sort_unstable_by(f64::total_cmp);
    rates
}

fn median(sorted: &[f64]) -> Option<f64> {
    let count = sorted.len();
    let upper = *sorted.get(count / 2)?;
    Some(sorted[(count - 1) / 2].midpoint(upper))
}

pub fn sparkline(samples: &[CpuSample], seconds: u32) -> String {
    let Some(last) = samples.last() else {
        return String::new();
    };
    let bucket_seconds = seconds.div_ceil(GRAPH_COLUMNS);
    let bucket_index = |second: u32| ((second - 1) / bucket_seconds) as usize;
    let mut buckets = vec![(0.0, 0u32); bucket_index(last.second) + 1];
    for sample in samples {
        let (mean, count) = &mut buckets[bucket_index(sample.second)];
        *count += 1;
        *mean += (sample.events_per_second - *mean) / f64::from(*count);
    }
    let (min, max) = buckets.iter().filter(|(_, count)| *count > 0).fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(min, max), (rate, _)| (min.min(*rate), max.max(*rate)),
    );
    let levels = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let bars: String = buckets
        .iter()
        .map(|&(rate, count)| {
            if count == 0 {
                '·'
            } else if min == max {
                '▄'
            } else {
                let position = ((rate - min) / (max - min) * 7.0).round() as usize;
                levels[position]
            }
        })
        .collect();
    format!("График · {bars} · по {bucket_seconds} с · {min:.0}–{max:.0}")
}
