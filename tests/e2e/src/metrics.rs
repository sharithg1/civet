//! Measured numbers: printed as `METRIC {json}` lines on stderr (run with
//! `--nocapture` to see them) and appended to `$CIVET_E2E_METRICS` (JSON
//! lines) when set. `scripts/e2e-report.sh` collects them.

use std::io::Write;
use std::time::Duration;

/// Percentile summary of a sample (milliseconds).
#[derive(Debug, Clone, Copy)]
pub struct Summary {
    /// Sample size.
    pub n: usize,
    /// Median.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// Maximum.
    pub max: f64,
    /// Minimum.
    pub min: f64,
    /// Mean.
    pub mean: f64,
}

/// Nearest-rank percentile of a sorted sample.
fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

impl Summary {
    /// Summarize durations.
    pub fn of(samples: &[Duration]) -> Self {
        let mut v: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        Self {
            n: v.len(),
            p50: pct(&v, 50.0),
            p95: pct(&v, 95.0),
            max: v.last().copied().unwrap_or(f64::NAN),
            min: v.first().copied().unwrap_or(f64::NAN),
            mean: v.iter().sum::<f64>() / v.len().max(1) as f64,
        }
    }
}

/// Record one metric.
pub fn record(name: &str, value: f64, unit: &str, extra: serde_json::Value) {
    let line = serde_json::json!({
        "metric": name,
        "value": (value * 1000.0).round() / 1000.0,
        "unit": unit,
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "extra": extra,
    })
    .to_string();
    eprintln!("METRIC {line}");
    if let Ok(path) = std::env::var("CIVET_E2E_METRICS")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Record a latency summary as `<name>.p50`, `.p95`, `.max`.
pub fn record_summary(name: &str, s: &Summary, target_ms: Option<f64>) {
    let extra =
        serde_json::json!({ "n": s.n, "min": s.min, "mean": s.mean, "target_ms": target_ms });
    record(&format!("{name}.p50"), s.p50, "ms", extra.clone());
    record(&format!("{name}.p95"), s.p95, "ms", extra.clone());
    record(&format!("{name}.max"), s.max, "ms", extra);
}

/// Milliseconds as f64.
pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Read an env var as a number, with a default.
pub fn env_num<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let v: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        let s = Summary::of(&v);
        assert_eq!(s.p50, 50.0);
        assert_eq!(s.p95, 95.0);
        assert_eq!(s.max, 100.0);
        assert_eq!(s.n, 100);
    }
}
