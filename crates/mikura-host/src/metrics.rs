//! Process-local Prometheus text for host operations (ADR 0041).
//!
//! Labels are wire `op` names and store namespaces. Payloads, keys, and
//! property values never appear.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const MAX_NAMESPACES: usize = 16;
const OTHER_NS: &str = "_other";
const BUCKETS_MS: &[u64] = &[1, 5, 25, 100, 500, 1_000, 5_000];

#[derive(Default)]
struct Histogram {
    buckets: [AtomicU64; 7],
    inf: AtomicU64,
    count: AtomicU64,
    sum_us: AtomicU64,
}

impl Histogram {
    fn observe(&self, elapsed: Duration) {
        let us = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_us.fetch_add(us, Ordering::Relaxed);
        let mut placed = false;
        for (i, bound) in BUCKETS_MS.iter().enumerate() {
            if elapsed <= Duration::from_millis(*bound) {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
                placed = true;
                break;
            }
        }
        if !placed {
            self.inf.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn render(&self, op: &str, out: &mut String) {
        let op = escape_label(op);
        let mut cumulative = 0u64;
        for (i, bound) in BUCKETS_MS.iter().enumerate() {
            cumulative += self.buckets[i].load(Ordering::Relaxed);
            let seconds = if *bound % 1000 == 0 {
                format!("{:.0}", *bound as f64 / 1000.0)
            } else {
                format!("{}", *bound as f64 / 1000.0)
            };
            out.push_str(&format!(
                "mikura_rpc_latency_seconds_bucket{{op=\"{op}\",le=\"{seconds}\"}} {cumulative}\n"
            ));
        }
        cumulative += self.inf.load(Ordering::Relaxed);
        out.push_str(&format!(
            "mikura_rpc_latency_seconds_bucket{{op=\"{op}\",le=\"+Inf\"}} {cumulative}\n"
        ));
        out.push_str(&format!(
            "mikura_rpc_latency_seconds_sum{{op=\"{op}\"}} {}\n",
            self.sum_us.load(Ordering::Relaxed) as f64 / 1_000_000.0
        ));
        out.push_str(&format!(
            "mikura_rpc_latency_seconds_count{{op=\"{op}\"}} {}\n",
            self.count.load(Ordering::Relaxed)
        ));
    }
}

pub struct ProcessSnapshot {
    pub accepted: u64,
    pub rejected: u64,
    pub queue_depth: u64,
    pub log_bytes: u64,
    pub log_growth_bytes: u64,
    pub committed_pages: u64,
    pub sidecar_lag_pages: u64,
    pub open_replay_seconds: f64,
}

pub struct Metrics {
    by_op: Mutex<BTreeMap<String, Histogram>>,
    by_ns: Mutex<BTreeMap<String, AtomicU64>>,
    overloaded: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            by_op: Mutex::new(BTreeMap::new()),
            by_ns: Mutex::new(BTreeMap::new()),
            overloaded: AtomicU64::new(0),
        }
    }

    pub fn observe_rpc(&self, op: &str, namespace: Option<&str>, elapsed: Duration) {
        if let Ok(mut map) = self.by_op.lock() {
            map.entry(op.to_string()).or_default().observe(elapsed);
        }
        if let Some(ns) = namespace {
            self.observe_namespace(ns);
        }
    }

    pub fn observe_overload(&self) {
        self.overloaded.fetch_add(1, Ordering::Relaxed);
    }

    fn observe_namespace(&self, ns: &str) {
        let Ok(mut map) = self.by_ns.lock() else {
            return;
        };
        if let Some(count) = map.get(ns) {
            count.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let key = if map.len() < MAX_NAMESPACES {
            ns.to_string()
        } else {
            OTHER_NS.to_string()
        };
        map.entry(key)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn overloaded(&self) -> u64 {
        self.overloaded.load(Ordering::Relaxed)
    }

    pub fn render(&self, snap: ProcessSnapshot) -> String {
        let ProcessSnapshot {
            accepted,
            rejected,
            queue_depth,
            log_bytes,
            log_growth_bytes,
            committed_pages,
            sidecar_lag_pages,
            open_replay_seconds,
        } = snap;
        let mut out = String::new();
        out.push_str("# HELP mikura_rpc_latency_seconds Host RPC latency since process start.\n");
        out.push_str("# TYPE mikura_rpc_latency_seconds histogram\n");
        if let Ok(map) = self.by_op.lock() {
            for (op, hist) in map.iter() {
                hist.render(op, &mut out);
            }
        }
        out.push_str("# HELP mikura_rpc_requests_total Completed RPCs by bounded namespace.\n");
        out.push_str("# TYPE mikura_rpc_requests_total counter\n");
        if let Ok(map) = self.by_ns.lock() {
            for (ns, count) in map.iter() {
                out.push_str(&format!(
                    "mikura_rpc_requests_total{{namespace=\"{}\"}} {}\n",
                    escape_label(ns),
                    count.load(Ordering::Relaxed)
                ));
            }
        }
        out.push_str("# HELP mikura_rpc_accepted_total Successful non-health RPCs since start.\n");
        out.push_str("# TYPE mikura_rpc_accepted_total counter\n");
        out.push_str(&format!("mikura_rpc_accepted_total {accepted}\n"));
        out.push_str("# HELP mikura_rpc_rejected_total Failed non-health RPCs since start.\n");
        out.push_str("# TYPE mikura_rpc_rejected_total counter\n");
        out.push_str(&format!("mikura_rpc_rejected_total {rejected}\n"));
        out.push_str(
            "# HELP mikura_rpc_overloaded_total Request-bound or request-timeout rejects.\n",
        );
        out.push_str("# TYPE mikura_rpc_overloaded_total counter\n");
        out.push_str(&format!(
            "mikura_rpc_overloaded_total {}\n",
            self.overloaded()
        ));
        out.push_str("# HELP mikura_queue_depth In-flight reads plus writer waiters.\n");
        out.push_str("# TYPE mikura_queue_depth gauge\n");
        out.push_str(&format!("mikura_queue_depth {queue_depth}\n"));
        out.push_str("# HELP mikura_log_bytes Object log file size.\n");
        out.push_str("# TYPE mikura_log_bytes gauge\n");
        out.push_str(&format!("mikura_log_bytes {log_bytes}\n"));
        out.push_str("# HELP mikura_log_growth_bytes Bytes grown since process open.\n");
        out.push_str("# TYPE mikura_log_growth_bytes gauge\n");
        out.push_str(&format!("mikura_log_growth_bytes {log_growth_bytes}\n"));
        out.push_str("# HELP mikura_committed_pages Superblock committed page count.\n");
        out.push_str("# TYPE mikura_committed_pages gauge\n");
        out.push_str(&format!("mikura_committed_pages {committed_pages}\n"));
        out.push_str("# HELP mikura_sidecar_lag_pages Committed pages minus join sidecar stamp.\n");
        out.push_str("# TYPE mikura_sidecar_lag_pages gauge\n");
        out.push_str(&format!("mikura_sidecar_lag_pages {sidecar_lag_pages}\n"));
        out.push_str("# HELP mikura_open_replay_seconds Store open/replay duration at start.\n");
        out.push_str("# TYPE mikura_open_replay_seconds gauge\n");
        out.push_str(&format!(
            "mikura_open_replay_seconds {open_replay_seconds:.6}\n"
        ));
        out
    }
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn escape_label_newlines_quotes_and_backslashes() {
        assert_eq!(escape_label("a\nb\"c\\d"), r#"a\nb\"c\\d"#);
    }

    #[test]
    fn latency_keeps_sub_millisecond_sum_and_bucket_bounds() {
        let metrics = Metrics::new();
        metrics.observe_rpc("evaluate", None, Duration::from_micros(400));
        metrics.observe_rpc("evaluate", None, Duration::from_micros(1500));
        let text = metrics.render(ProcessSnapshot {
            accepted: 0,
            rejected: 0,
            queue_depth: 0,
            log_bytes: 0,
            log_growth_bytes: 0,
            committed_pages: 0,
            sidecar_lag_pages: 0,
            open_replay_seconds: 0.0,
        });
        assert!(
            text.contains("mikura_rpc_latency_seconds_sum{op=\"evaluate\"} 0.0019"),
            "{text}"
        );
        assert!(
            text.contains("mikura_rpc_latency_seconds_bucket{op=\"evaluate\",le=\"0.001\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("mikura_rpc_latency_seconds_bucket{op=\"evaluate\",le=\"0.005\"} 2"),
            "{text}"
        );
    }
}
