use crate::bench::{Metric, MetricKind};
use crate::report::{ColorMode, Outcome, Section, clean};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};
use std::io::{IsTerminal, stderr};
use std::time::{Duration, Instant};

pub struct RunProgress {
    enabled: bool,
    color: bool,
    total: usize,
    index: usize,
}

impl RunProgress {
    pub fn new(host: &str, total: usize, color: ColorMode, disabled: bool) -> Self {
        let terminal = stderr().is_terminal();
        let enabled = !disabled && terminal && std::env::var("TERM").as_deref() != Ok("dumb");
        if enabled {
            eprintln!("VPS Bench · {} · этапов {total}", clean(host));
        }
        Self {
            enabled,
            color: color.enabled(terminal),
            total,
            index: 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn section(&self, section: Section, context: Option<&str>) {
        if self.enabled {
            eprintln!("\n{}", section.heading(self.color));
            if let Some(context) = context {
                eprintln!("  {context}");
            }
        }
    }

    pub fn start(&mut self, name: &str, workload: &str, planned_seconds: Option<u32>) -> Stage {
        self.index += 1;
        let label = format!("{}/{}  {name}", self.index, self.total);
        let bar = self.enabled.then(|| {
            let template = if self.color {
                "{spinner:.cyan} {elapsed_precise:.cyan}  {prefix}\n  {workload}\n  {meter:.cyan}{wide_msg}"
            } else {
                "{spinner} {elapsed_precise}  {prefix}\n  {workload}\n  {meter}{wide_msg}"
            };
            let workload = workload.to_owned();
            let style = ProgressStyle::with_template(template)
                .expect("valid progress template")
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏✓")
                .with_key("workload", move |_: &ProgressState, out: &mut dyn std::fmt::Write| {
                    let _ = out.write_str(&workload);
                })
                .with_key(
                    "meter",
                    move |state: &ProgressState, out: &mut dyn std::fmt::Write| {
                        if let Some(seconds) = planned_seconds {
                            let slots = 12;
                            let fraction = state.elapsed().as_secs_f64() / f64::from(seconds);
                            let filled = (fraction * slots as f64).min(slots as f64) as usize;
                            let _ = write!(
                                out,
                                "{}{}  {seconds} с · ",
                                "━".repeat(filled),
                                "─".repeat(slots - filled)
                            );
                        }
                    },
                );
            let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr_with_hz(10));
            bar.set_style(style);
            bar.set_prefix(label.clone());
            bar.set_message("Запуск");
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        });
        Stage {
            bar,
            color: self.color,
            label,
            started: Instant::now(),
        }
    }
}

pub struct Stage {
    bar: Option<ProgressBar>,
    color: bool,
    label: String,
    started: Instant,
}

impl Stage {
    pub fn enabled(&self) -> bool {
        self.bar.is_some()
    }

    pub fn update(&self, message: impl Into<std::borrow::Cow<'static, str>>) {
        if let Some(bar) = &self.bar {
            bar.set_message(message);
        }
    }

    pub fn finish<'a>(self, outcomes: impl IntoIterator<Item = &'a Outcome>) {
        let Some(bar) = self.bar else {
            return;
        };
        bar.finish_and_clear();
        let mut complete = 0;
        let mut failed = 0;
        let mut skipped = 0;
        let mut metrics = &[][..];
        for outcome in outcomes {
            match outcome {
                Outcome::Completed { metrics: sample } => {
                    complete += 1;
                    metrics = sample;
                }
                Outcome::Failed { .. } => failed += 1,
                Outcome::Skipped { .. } => skipped += 1,
            }
        }
        let status = match (complete, failed, skipped) {
            (_, 0, 0) => ("✓", "Готово", "32"),
            (0, 0, _) => ("–", "Пропущено", "33"),
            (0, _, _) => ("✗", "Ошибка", "31"),
            _ => ("!", "Частично", "33"),
        };
        let (symbol, message, tint) = status;
        let badge = if self.color {
            format!("\x1b[{tint}m{symbol} {message}\x1b[0m")
        } else {
            format!("{symbol} {message}")
        };
        let elapsed = self.started.elapsed().as_secs_f64();
        let detail = if complete + failed + skipped > 1 {
            format!(" · узлов готово {complete}/{}", complete + failed + skipped)
        } else {
            summary(metrics).map_or_else(String::new, |value| format!(" · {value}"))
        };
        eprintln!("{badge}  {}  ({elapsed:.1} с){detail}", self.label);
    }
}

fn summary(metrics: &[Metric]) -> Option<String> {
    let lookup = |kind| {
        metrics
            .iter()
            .find(|metric| metric.kind == kind)
            .map(|m| m.value)
    };
    if let Some(loss) = lookup(MetricKind::PacketLossPercent) {
        return Some(lookup(MetricKind::RttP95Ms).map_or_else(
            || format!("потери {loss:.1}%"),
            |p95| format!("p95 {p95:.3} мс · потери {loss:.1}%"),
        ));
    }
    let metric = metrics.first()?;
    let unit = match metric.kind {
        MetricKind::EventsPerSecond => "событий/с",
        MetricKind::MibPerSecond => "МиБ/с",
        MetricKind::MegabitsPerSecond => "Мбит/с",
        _ => return None,
    };
    Some(format!("{:.1} {unit}", metric.value))
}
