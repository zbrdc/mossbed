//! Load driver for the runner: N threads, each running guest code in a loop.
//!
//! Measures the runner rather than the MCP line protocol, so the numbers are a
//! ceiling for what the runner can do, not for a client of it.
//!
//!     cargo run --release --example load -- <threads> <runs> ["code"]
//!
//! Measured on a 24-core machine, 3000 runs per level, `1+1;` per run:
//!
//! | threads | runs/s | p50 | p99 |
//! |---|---|---|---|
//! | 1 | 185 | 5 ms | 10 ms |
//! | 8 | 1095 | 5 ms | 10 ms |
//! | 32 | 2546 | 11 ms | 19 ms |
//! | 64 | 2572 | 23 ms | 49 ms |
//! | 256 | 2515 | 76 ms | 259 ms |
//!
//! Throughput saturates near 32 concurrent runs. Past that the machine runs 21
//! of its 24 cores busy and extra workers only add queueing: p50 goes from 11 ms
//! to 76 ms with no gain. A guest that computes rather than merely starting up is
//! slower to serve: the same sweep with a 1e6-iteration loop peaks at 1028 runs/s.

use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use mossbed::{RunOptions, Runner, resolve_bin};

fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<&str> = stat
        .rsplit(')')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    if fields.len() < 13 {
        return 0.0;
    }
    let utime: f64 = fields[11].parse().unwrap_or(0.0);
    let stime: f64 = fields[12].parse().unwrap_or(0.0);
    (utime + stime) / 100.0
}

fn percentile(sorted: &[u128], q: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() as f64 * q) as usize).min(sorted.len() - 1)]
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let threads: usize = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(32);
    let total: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(2000);
    let code = args.get(3).cloned().unwrap_or_else(|| "1+1;".to_string());

    let mut runner = Runner::new(resolve_bin(None)).expect("runner config");
    runner.sandbox_check().expect("sandbox check");
    let runner = Arc::new(runner);
    let next = Arc::new(AtomicUsize::new(0));

    let cpu0 = cpu_seconds();
    let start = Instant::now();
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let runner = Arc::clone(&runner);
            let next = Arc::clone(&next);
            let code = code.clone();
            thread::spawn(move || {
                let opts = RunOptions::default();
                let mut own = Vec::new();
                loop {
                    if next.fetch_add(1, Ordering::Relaxed) >= total {
                        break;
                    }
                    match runner.run_js(&code, &opts) {
                        Ok(result) if result.status == mossbed::RunStatus::Ok => {
                            own.push(result.duration_ms)
                        }
                        Ok(_) => own.push(0),
                        Err(_) => {}
                    }
                }
                own
            })
        })
        .collect();

    let mut durations: Vec<u128> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap_or_default())
        .collect();
    let wall = start.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu0;
    durations.sort_unstable();

    println!(
        "threads={:<4} runs={:<5} wall={:>5.2}s throughput={:>7.0} runs/s  p50={:>3}ms p95={:>3}ms p99={:>3}ms max={:>3}ms  driver_cpu={:>5.1}s ({:>4.1} cores)",
        threads,
        durations.len(),
        wall,
        durations.len() as f64 / wall,
        percentile(&durations, 0.50),
        percentile(&durations, 0.95),
        percentile(&durations, 0.99),
        durations.last().copied().unwrap_or(0),
        cpu,
        cpu / wall
    );
}
