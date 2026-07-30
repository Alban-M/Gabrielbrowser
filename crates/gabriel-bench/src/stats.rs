//! Timing collection and reporting.
//!
//! Latency distributions are skewed, so this reports percentiles rather than a
//! mean with a standard deviation. The p99 of a request path is what a
//! developer actually notices; the mean hides it.

use std::time::Duration;

pub struct Samples {
    label: String,
    micros: Vec<u128>,
}

impl Samples {
    pub fn new(label: impl Into<String>) -> Self {
        Samples {
            label: label.into(),
            micros: Vec::new(),
        }
    }

    pub fn push(&mut self, elapsed: Duration) {
        self.micros.push(elapsed.as_micros());
    }

    fn percentile(&self, sorted: &[u128], p: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        // Nearest-rank, clamped — good enough at these sample counts and it
        // never interpolates a value that was never measured.
        let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        sorted[rank.saturating_sub(1).min(sorted.len() - 1)] as f64 / 1000.0
    }

    pub fn summary(&self) -> Summary {
        let mut sorted = self.micros.clone();
        sorted.sort_unstable();
        let total: u128 = sorted.iter().sum();
        Summary {
            label: self.label.clone(),
            count: sorted.len(),
            mean_ms: if sorted.is_empty() {
                0.0
            } else {
                total as f64 / sorted.len() as f64 / 1000.0
            },
            p50_ms: self.percentile(&sorted, 50.0),
            p95_ms: self.percentile(&sorted, 95.0),
            p99_ms: self.percentile(&sorted, 99.0),
            max_ms: sorted.last().copied().unwrap_or(0) as f64 / 1000.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub label: String,
    pub count: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

pub fn print_table(title: &str, rows: &[Summary]) {
    println!("\n{title}");
    println!(
        "{:<38} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "", "n", "mean", "p50", "p95", "p99", "max"
    );
    for row in rows {
        println!(
            "{:<38} {:>7} {:>9} {:>9} {:>9} {:>9} {:>9}",
            row.label,
            row.count,
            ms(row.mean_ms),
            ms(row.p50_ms),
            ms(row.p95_ms),
            ms(row.p99_ms),
            ms(row.max_ms),
        );
    }
}

/// Sub-millisecond numbers are the interesting ones here, so keep resolution
/// instead of rounding everything to "0 ms".
pub fn ms(value: f64) -> String {
    if value >= 100.0 {
        format!("{value:.0}ms")
    } else if value >= 1.0 {
        format!("{value:.1}ms")
    } else if value >= 0.001 {
        format!("{:.0}µs", value * 1000.0)
    } else {
        format!("{:.0}ns", value * 1_000_000.0)
    }
}

pub fn print_row(label: &str, value: String) {
    println!("{label:<38} {value:>9}");
}

pub fn rate(count: usize, elapsed: Duration) -> String {
    let per_second = count as f64 / elapsed.as_secs_f64();
    if per_second >= 1_000_000.0 {
        format!("{:.1}M/s", per_second / 1_000_000.0)
    } else if per_second >= 1_000.0 {
        format!("{:.1}k/s", per_second / 1_000.0)
    } else {
        format!("{per_second:.0}/s")
    }
}
