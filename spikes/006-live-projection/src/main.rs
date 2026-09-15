//! Live in-memory projection updated by the funnel. Throwaway. Not mikura's store.
//!
//! Evaluate reads the live map. Dual-read rebuilds from the JSONL log (vehicle)
//! and compares identity + two-hop. JSONL is not the product format.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Record {
    gen: u64,
    kind: String,
    key: String,
    hidden: bool,
    props: HashMap<String, String>,
}

struct Scale {
    customers: i64,
    orders: i64,
    shipments: i64,
}

impl Scale {
    fn from_objects(objects: i64) -> Result<Self, String> {
        if objects < 10 {
            return Err("need at least 10 objects".into());
        }
        Ok(Self {
            customers: objects / 100,
            orders: objects / 10,
            shipments: objects - objects / 100 - objects / 10,
        })
    }

    fn total(&self) -> i64 {
        self.customers + self.orders + self.shipments
    }
}

struct Live {
    objects: HashMap<(String, String), Record>,
}

impl Live {
    fn new() -> Self {
        Self {
            objects: HashMap::new(),
        }
    }

    fn apply(&mut self, record: Record) {
        self.objects
            .insert((record.kind.clone(), record.key.clone()), record);
    }
}

fn main() -> ExitCode {
    let objects = parse_objects(env::args().skip(1)).unwrap_or(10_000);
    let scale = match Scale::from_objects(objects) {
        Ok(scale) => scale,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let dir = env::temp_dir().join(format!("mikura-live-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("mkdir: {error}");
        return ExitCode::from(1);
    }
    let log_path = dir.join("objects.jsonl");
    let mut live = Live::new();
    let snapshot = Instant::now();
    if let Err(error) = funnel_snapshot(&log_path, &scale, &mut live) {
        eprintln!("snapshot: {error}");
        return ExitCode::from(1);
    }
    let snapshot_ms = snapshot.elapsed().as_millis();
    let live_hop_start = Instant::now();
    let live_hop = two_hop_count(&live.objects);
    let live_two_hop_ms = live_hop_start.elapsed().as_millis();
    let rebuild_start = Instant::now();
    let rebuilt = match rebuild(&log_path) {
        Ok(map) => map,
        Err(error) => {
            eprintln!("rebuild: {error}");
            return ExitCode::from(1);
        }
    };
    let rebuild_ms = rebuild_start.elapsed().as_millis();
    let rebuilt_hop_start = Instant::now();
    let rebuilt_hop = two_hop_count(&rebuilt);
    let rebuilt_two_hop_ms = rebuilt_hop_start.elapsed().as_millis();
    let incr = Instant::now();
    if let Err(error) = funnel_incremental(&log_path, &scale, 1_000, 2, &mut live) {
        eprintln!("incremental: {error}");
        return ExitCode::from(1);
    }
    let incremental_live_ms = incr.elapsed().as_millis();
    let rebuilt_after = match rebuild(&log_path) {
        Ok(map) => map,
        Err(error) => {
            eprintln!("rebuild after incremental: {error}");
            return ExitCode::from(1);
        }
    };
    let dual_read = identity_digest(&live.objects) == identity_digest(&rebuilt_after)
        && live_hop == rebuilt_hop;
    let hidden = live.objects.values().filter(|o| o.hidden).count();
    println!("spike=006-live-projection");
    println!("objects={}", scale.total());
    println!("snapshot_ms={snapshot_ms}");
    println!("live_two_hop_ms={live_two_hop_ms}");
    println!("rebuild_ms={rebuild_ms}");
    println!("rebuilt_two_hop_ms={rebuilt_two_hop_ms}");
    println!("incremental_live_1k_ms={incremental_live_ms}");
    println!("two_hop_visible_customers={live_hop}");
    println!("hidden={hidden}");
    println!("dual_read_hold={dual_read}");
    let _ = fs::remove_dir_all(&dir);
    if dual_read && live_hop > 0 && hidden > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn funnel_snapshot(path: &Path, scale: &Scale, live: &mut Live) -> Result<(), String> {
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(file);
    for id in 0..scale.customers {
        append_and_apply(
            &mut out,
            live,
            Record {
                gen: 1,
                kind: "Customer".into(),
                key: format!("c{id}"),
                hidden: id % 100 == 0,
                props: HashMap::from([("region".into(), region(id))]),
            },
        )?;
    }
    for id in 0..scale.orders {
        append_and_apply(
            &mut out,
            live,
            Record {
                gen: 1,
                kind: "Order".into(),
                key: format!("o{id}"),
                hidden: id % 100 == 0,
                props: HashMap::from([
                    ("customer_id".into(), format!("c{}", id % scale.customers)),
                    ("amount".into(), format!("{}", (id % 100) + 1)),
                ]),
            },
        )?;
    }
    for id in 0..scale.shipments {
        append_and_apply(
            &mut out,
            live,
            Record {
                gen: 1,
                kind: "Shipment".into(),
                key: format!("s{id}"),
                hidden: id % 100 == 0,
                props: HashMap::from([
                    ("order_id".into(), format!("o{}", id % scale.orders)),
                    ("amount".into(), format!("{}", (id % 50) + 1)),
                ]),
            },
        )?;
    }
    out.flush().map_err(|e| e.to_string())
}

fn funnel_incremental(
    path: &Path,
    scale: &Scale,
    n: i64,
    gen: u64,
    live: &mut Live,
) -> Result<(), String> {
    let file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(file);
    for i in 0..n {
        let id = i % scale.customers;
        append_and_apply(
            &mut out,
            live,
            Record {
                gen,
                kind: "Customer".into(),
                key: format!("c{id}"),
                hidden: id % 100 == 0,
                props: HashMap::from([("region".into(), region(id + 1))]),
            },
        )?;
    }
    out.flush().map_err(|e| e.to_string())
}

fn append_and_apply(
    out: &mut BufWriter<File>,
    live: &mut Live,
    record: Record,
) -> Result<(), String> {
    serde_json::to_writer(&mut *out, &record).map_err(|e| e.to_string())?;
    out.write_all(b"\n").map_err(|e| e.to_string())?;
    live.apply(record);
    Ok(())
}

fn rebuild(path: &Path) -> Result<HashMap<(String, String), Record>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let mut live = HashMap::new();
    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.is_empty() {
            continue;
        }
        let record: Record = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        live.insert((record.kind.clone(), record.key.clone()), record);
    }
    Ok(live)
}

fn two_hop_count(live: &HashMap<(String, String), Record>) -> usize {
    let mut orders_by_customer: HashMap<&str, Vec<&str>> = HashMap::new();
    for object in live.values() {
        if object.kind != "Order" || object.hidden {
            continue;
        }
        if let Some(customer_id) = object.props.get("customer_id") {
            orders_by_customer
                .entry(customer_id.as_str())
                .or_default()
                .push(object.key.as_str());
        }
    }
    let mut orders_with_shipment = HashSet::new();
    for object in live.values() {
        if object.kind != "Shipment" || object.hidden {
            continue;
        }
        if let Some(order_id) = object.props.get("order_id") {
            orders_with_shipment.insert(order_id.as_str());
        }
    }
    live.values()
        .filter(|object| object.kind == "Customer" && !object.hidden)
        .filter(|customer| {
            orders_by_customer
                .get(customer.key.as_str())
                .is_some_and(|orders| {
                    orders
                        .iter()
                        .any(|order_id| orders_with_shipment.contains(order_id))
                })
        })
        .count()
}

fn identity_digest(live: &HashMap<(String, String), Record>) -> String {
    let mut rows: Vec<_> = live
        .values()
        .map(|object| {
            format!(
                "{}:{}:{}:{}",
                object.kind, object.key, object.gen, object.hidden
            )
        })
        .collect();
    rows.sort();
    let mut hasher = Sha256::new();
    for row in rows {
        hasher.update(row.as_bytes());
        hasher.update(b"\n");
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn region(id: i64) -> String {
    ["eu", "us", "ap"][(id.rem_euclid(3)) as usize].into()
}

fn parse_objects(mut args: impl Iterator<Item = String>) -> Option<i64> {
    while let Some(arg) = args.next() {
        if arg == "--objects" {
            return args.next()?.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_matches_rebuild_and_incremental_skips_log_scan() {
        let dir = std::env::temp_dir().join("mikura-live-projection-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.jsonl");
        let scale = Scale::from_objects(1_000).unwrap();
        let mut live = Live::new();
        funnel_snapshot(&path, &scale, &mut live).unwrap();
        let rebuilt = rebuild(&path).unwrap();
        assert_eq!(identity_digest(&live.objects), identity_digest(&rebuilt));
        assert_eq!(
            two_hop_count(&live.objects),
            two_hop_count(&rebuilt)
        );
        funnel_incremental(&path, &scale, 10, 2, &mut live).unwrap();
        assert_eq!(live.objects[&("Customer".into(), "c1".into())].gen, 2);
        let rebuilt = rebuild(&path).unwrap();
        assert_eq!(identity_digest(&live.objects), identity_digest(&rebuilt));
        let _ = fs::remove_dir_all(&dir);
    }
}
