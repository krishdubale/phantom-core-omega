// ============================================================================
// PhantomCore — Benchmarking and Metrics Collection
// Tracks latency, throughput, CPU usage for the daemon
// ============================================================================

use serde::Serialize;
use std::time::{Duration, Instant};

/// Collected benchmark statistics
#[derive(Debug, Serialize)]
pub struct BenchmarkStats {
    pub avg_latency_us: f64,
    pub p50_latency_us: f64,
    pub p95_latency_us: f64,
    pub p99_latency_us: f64,
    pub min_latency_us: f64,
    pub max_latency_us: f64,
    pub avg_throughput_rps: f64,
    pub total_requests: u64,
    pub total_duration_secs: f64,
    pub jit_cache_hits: u64,
    pub jit_cache_misses: u64,
    pub speculative_hits: u64,
    pub speculative_misses: u64,
}

/// Collects performance metrics during daemon operation
#[derive(Debug)]
pub struct BenchmarkCollector {
    /// All recorded request latencies in microseconds
    latencies_us: Vec<f64>,
    /// Throughput samples: (timestamp, requests_in_window)
    throughput_samples: Vec<(Instant, u64)>,
    /// When the collector was created
    start_time: Instant,
    /// Request count in current throughput window
    window_count: u64,
    /// Last throughput window reset
    last_window_reset: Instant,
    /// Throughput measurement window size
    window_duration: Duration,
    /// JIT cache hits
    pub jit_hits: u64,
    /// JIT cache misses
    pub jit_misses: u64,
    /// Speculative cache hits
    pub spec_hits: u64,
    /// Speculative cache misses
    pub spec_misses: u64,
}

impl BenchmarkCollector {
    pub fn new() -> Self {
        let now = Instant::now();
        BenchmarkCollector {
            latencies_us: Vec::with_capacity(100_000),
            throughput_samples: Vec::with_capacity(10_000),
            start_time: now,
            window_count: 0,
            last_window_reset: now,
            window_duration: Duration::from_secs(1),
            jit_hits: 0,
            jit_misses: 0,
            spec_hits: 0,
            spec_misses: 0,
        }
    }

    /// Record a completed request with its start and end times
    pub fn record_request(&mut self, start: Instant, end: Instant) {
        let duration = end.duration_since(start);
        self.latencies_us.push(duration.as_micros() as f64);

        self.window_count += 1;

        // Check if we need to roll over the throughput window
        if self.last_window_reset.elapsed() >= self.window_duration {
            self.throughput_samples
                .push((self.last_window_reset, self.window_count));
            self.window_count = 0;
            self.last_window_reset = Instant::now();
        }
    }

    /// Record a JIT cache hit or miss
    pub fn record_jit_lookup(&mut self, hit: bool) {
        if hit {
            self.jit_hits += 1;
        } else {
            self.jit_misses += 1;
        }
    }

    /// Record a speculative cache hit or miss
    pub fn record_speculative_lookup(&mut self, hit: bool) {
        if hit {
            self.spec_hits += 1;
        } else {
            self.spec_misses += 1;
        }
    }

    /// Compute percentile from sorted latency data
    fn percentile(sorted: &[f64], p: f64) -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let idx = ((sorted.len() as f64) * p / 100.0).ceil() as usize;
        let idx = idx.min(sorted.len() - 1);
        sorted[idx]
    }

    /// Compute and return all benchmark statistics
    pub fn get_stats(&self) -> BenchmarkStats {
        let mut sorted = self.latencies_us.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let total = sorted.len() as u64;
        let sum: f64 = sorted.iter().sum();
        let avg = if total > 0 { sum / total as f64 } else { 0.0 };
        let total_duration = self.start_time.elapsed().as_secs_f64();
        let throughput = if total_duration > 0.0 {
            total as f64 / total_duration
        } else {
            0.0
        };

        BenchmarkStats {
            avg_latency_us: avg,
            p50_latency_us: Self::percentile(&sorted, 50.0),
            p95_latency_us: Self::percentile(&sorted, 95.0),
            p99_latency_us: Self::percentile(&sorted, 99.0),
            min_latency_us: sorted.first().copied().unwrap_or(0.0),
            max_latency_us: sorted.last().copied().unwrap_or(0.0),
            avg_throughput_rps: throughput,
            total_requests: total,
            total_duration_secs: total_duration,
            jit_cache_hits: self.jit_hits,
            jit_cache_misses: self.jit_misses,
            speculative_hits: self.spec_hits,
            speculative_misses: self.spec_misses,
        }
    }

    /// Serialize all collected data to JSON for external analysis tools
    pub fn to_json(&self) -> String {
        let stats = self.get_stats();
        serde_json::to_string_pretty(&stats).unwrap_or_else(|_| "{}".to_string())
    }

    /// Write benchmark data to a JSON log file
    pub fn save_to_file(&self, path: &str) -> std::io::Result<()> {
        let json = self.to_json();
        std::fs::write(path, json)
    }
}

impl Default for BenchmarkCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_stats() {
        let mut collector = BenchmarkCollector::new();
        // Simulate 100 requests with known latencies
        for _ in 0..100 {
            let start = Instant::now();
            std::thread::sleep(Duration::from_micros(10));
            let end = Instant::now();
            collector.record_request(start, end);
        }

        let stats = collector.get_stats();
        assert_eq!(stats.total_requests, 100);
        assert!(stats.avg_latency_us > 0.0);
        assert!(stats.p99_latency_us >= stats.p50_latency_us);
    }

    #[test]
    fn test_empty_stats() {
        let collector = BenchmarkCollector::new();
        let stats = collector.get_stats();
        assert_eq!(stats.total_requests, 0);
        assert_eq!(stats.avg_latency_us, 0.0);
    }

    #[test]
    fn test_json_output() {
        let collector = BenchmarkCollector::new();
        let json = collector.to_json();
        assert!(json.contains("total_requests"));
        assert!(json.contains("avg_latency_us"));
    }
}
