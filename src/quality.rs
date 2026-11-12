use crate::bench::{Metric, MetricKind};
use crate::report::Case;
use comfy_table::{Cell, Color};
use serde_json::Value;

const WIDTH: usize = 10;
const NORMAL_START: usize = 4;

pub struct Rating {
    filled: usize,
    label: &'static str,
    color: Color,
    ansi: u8,
    normal: f64,
    good: f64,
    sign: char,
}

impl Rating {
    fn text(&self) -> String {
        format!(
            "{}{} {}",
            "█".repeat(self.filled),
            "░".repeat(WIDTH - self.filled),
            self.label
        )
    }

    pub fn terminal(&self, color: bool) -> String {
        let text = self.text();
        if color {
            format!("\x1b[{}m{text}\x1b[0m", self.ansi)
        } else {
            text
        }
    }
}

pub fn rating_cell(test: Case, parameters: &Value, metric: &Metric) -> Cell {
    let Some(rating) = rating(test, parameters, metric) else {
        return Cell::new("—");
    };
    let Rating {
        normal, good, sign, ..
    } = rating;
    Cell::new(format!("{}\n{sign}{normal} / {sign}{good}", rating.text())).fg(rating.color)
}

pub fn rating(test: Case, parameters: &Value, metric: &Metric) -> Option<Rating> {
    if !metric.value.is_finite() || metric.value < 0.0 {
        return None;
    }
    let (normal, good) = thresholds(test, parameters, metric.kind)?;

    // Orient each scale so that moving to the right is always an improvement.
    let (value, normal_limit, good_limit, bottom, sign) = if metric.kind.higher_is_better() {
        (metric.value, normal, good, 0.0, '≥')
    } else {
        let bottom = normal + (normal - good);
        (-metric.value, -normal, -good, -bottom, '≤')
    };
    let (filled, label, color, ansi) = if value >= good_limit {
        (WIDTH, "Хорошо", Color::Green, 32)
    } else if value >= normal_limit {
        let fraction = (value - normal_limit) / (good_limit - normal_limit);
        let filled = NORMAL_START + band_fill(fraction, WIDTH - NORMAL_START);
        (filled, "Норма", Color::Yellow, 33)
    } else {
        let fraction = (value - bottom) / (normal_limit - bottom);
        (band_fill(fraction, NORMAL_START), "Плохо", Color::Red, 31)
    };
    Some(Rating {
        filled,
        label,
        color,
        ansi,
        normal,
        good,
        sign,
    })
}

fn band_fill(fraction: f64, cells: usize) -> usize {
    ((fraction.max(0.0) * cells as f64) as usize).min(cells - 1)
}

fn thresholds(test: Case, parameters: &Value, kind: MetricKind) -> Option<(f64, f64)> {
    use Case::*;
    use MetricKind::*;

    Some(match (test, kind) {
        (CpuSingle | CpuMulti, EventsPerSecond) => {
            let workers = parameters["threads"].as_u64().filter(|&n| n > 0)? as f64;
            (500.0 * workers, 1200.0 * workers)
        }
        (_, CpuStealPercent) => (5.0, 1.0),
        (CryptoSha256, MibPerSecond) => (256.0, 1024.0),
        (CryptoAes256Gcm, MibPerSecond) => (512.0, 2048.0),
        (RamRead | RamWrite, MibPerSecond)
            if parameters["block_mib"].as_u64() == Some(64)
                && parameters["threads"].as_u64() == Some(1) =>
        {
            match test {
                RamRead => (5.0 * 1024.0, 12.0 * 1024.0),
                _ => (4.0 * 1024.0, 10.0 * 1024.0),
            }
        }
        (DiskSeqRead | DiskSeqWrite, MibPerSecond) => (125.0, 500.0),
        (DiskRandRead | DiskRandWrite, Iops) => (1000.0, 5000.0),
        (DiskRandRead | DiskRandWrite, LatencyP50Ms) => (2.0, 0.5),
        (DiskRandRead | DiskRandWrite, LatencyP95Ms) => (5.0, 1.0),
        (DiskRandRead | DiskRandWrite, LatencyP99Ms) => (10.0, 2.0),
        (DiskSync, Iops) => (100.0, 1000.0),
        (DiskSync, SyncP95Ms) => (10.0, 2.0),
        (DiskSync, SyncP99Ms) => (20.0, 5.0),
        (_, MegabitsPerSecond) => (90.0, 900.0),
        (_, RttP50Ms | RttP95Ms | RttP99Ms | TcpMeanRttMs) => (100.0, 30.0),
        (_, PacketLossPercent) => (1.0, 0.0),
        _ => return None,
    })
}
