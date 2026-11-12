use crate::bench::{Metric, MetricKind};
use crate::quality::rating;
use crate::report::{Case, ColorMode, Outcome, Section, clean};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};
use serde_json::Value;
use std::fmt::Write;
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

    pub fn start<'a>(
        &mut self,
        test: Case,
        parameters: &'a Value,
        planned_seconds: Option<u32>,
    ) -> Stage<'a> {
        self.index += 1;
        let label = format!("{}/{}  {}", self.index, self.total, test.label());
        let bar = self.enabled.then(|| {
            let mut template = if self.color {
                "{spinner:.cyan} {elapsed_precise:.cyan}  {prefix}\n  {workload}"
            } else {
                "{spinner} {elapsed_precise}  {prefix}\n  {workload}"
            }
            .to_owned();
            if planned_seconds.is_some() {
                template.push_str(if self.color {
                    "\n  {meter:.cyan}"
                } else {
                    "\n  {meter}"
                });
            }
            template.push_str("\n  {wide_msg}");
            let workload = test.workload(parameters);
            let style = ProgressStyle::with_template(&template)
                .expect("valid progress template")
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏✓")
                .with_key(
                    "workload",
                    move |_: &ProgressState, out: &mut dyn std::fmt::Write| {
                        let _ = out.write_str(&workload);
                    },
                )
                .with_key(
                    "meter",
                    move |state: &ProgressState, out: &mut dyn std::fmt::Write| {
                        if let Some(seconds) = planned_seconds {
                            let slots = 12;
                            let fraction = state.elapsed().as_secs_f64() / f64::from(seconds);
                            let filled = (fraction * slots as f64).min(slots as f64) as usize;
                            let _ = write!(
                                out,
                                "{}{}  {seconds} с",
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
            test,
            parameters,
            color: self.color,
            label,
            started: Instant::now(),
        }
    }
}

pub struct Stage<'a> {
    bar: Option<ProgressBar>,
    test: Case,
    parameters: &'a Value,
    color: bool,
    label: String,
    started: Instant,
}

impl Stage<'_> {
    pub fn enabled(&self) -> bool {
        self.bar.is_some()
    }

    pub fn update(&self, message: impl Into<std::borrow::Cow<'static, str>>) {
        if let Some(bar) = &self.bar {
            bar.set_message(message);
        }
    }

    pub fn sample(&self, value: f64, message: String) {
        if let Some(bar) = &self.bar {
            let metric = Metric::new(self.primary_metric(), value);
            bar.set_message(self.rated(&metric, message));
        }
    }

    pub fn finish<'a>(self, outcomes: impl IntoIterator<Item = (Option<&'a str>, &'a Outcome)>) {
        let Some(bar) = &self.bar else {
            return;
        };
        bar.finish_and_clear();
        let mut complete = 0;
        let mut failed = 0;
        let mut skipped = 0;
        let mut summaries = String::new();
        for (target, outcome) in outcomes {
            match outcome {
                Outcome::Completed { metrics } => {
                    complete += 1;
                    self.append_summary(&mut summaries, target, metrics);
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
            String::new()
        };
        eprintln!(
            "{badge}  {}  ({elapsed:.1} с){detail}{summaries}",
            self.label
        );
    }

    fn primary_metric(&self) -> MetricKind {
        match self.test {
            Case::CpuSingle | Case::CpuMulti => MetricKind::EventsPerSecond,
            Case::CryptoSha256
            | Case::CryptoAes256Gcm
            | Case::RamRead
            | Case::RamWrite
            | Case::DiskSeqWrite
            | Case::DiskSeqRead => MetricKind::MibPerSecond,
            Case::DiskRandRead | Case::DiskRandWrite | Case::DiskSync => MetricKind::Iops,
            Case::NetUp | Case::NetDown | Case::NetUpMulti | Case::NetDownMulti => {
                MetricKind::MegabitsPerSecond
            }
            Case::PingIdle | Case::PingUpload | Case::PingDownload => MetricKind::RttP95Ms,
        }
    }

    fn rated(&self, metric: &Metric, message: String) -> String {
        match rating(self.test, self.parameters, metric) {
            Some(rating) => format!("{} · {message}", rating.terminal(self.color)),
            None => message,
        }
    }

    fn append_summary(&self, out: &mut String, target: Option<&str>, metrics: &[Metric]) {
        let indent = if let Some(target) = target {
            let _ = write!(out, "\n  {}", clean(target));
            "    "
        } else {
            "  "
        };
        for kind in [
            self.primary_metric(),
            MetricKind::PacketLossPercent,
            MetricKind::SyncP95Ms,
        ] {
            let Some(metric) = metrics.iter().find(|metric| metric.kind == kind) else {
                continue;
            };
            let value = metric.value;
            let message = match kind {
                MetricKind::EventsPerSecond => format!("{value:.2} событий/с"),
                MetricKind::MibPerSecond => format!("{value:.2} МиБ/с"),
                MetricKind::Iops => format!("{value:.2} IOPS"),
                MetricKind::MegabitsPerSecond => format!("{value:.2} Мбит/с"),
                MetricKind::RttP95Ms => format!("RTT p95 {value:.3} мс"),
                MetricKind::PacketLossPercent => format!("потери {value:.2}%"),
                MetricKind::SyncP95Ms => format!("fdatasync p95 {value:.3} мс"),
                _ => continue,
            };
            let _ = write!(out, "\n{indent}Итог · {}", self.rated(metric, message));
        }
    }
}
