//! Throwaway funnel + JSONL object log. Not mikura's store of record.

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct Object {
    kind: String,
    key: String,
    hidden: bool,
    props: HashMap<String, String>,
    gen: u64,
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

fn main() -> ExitCode {
    let objects = parse_objects(env::args().skip(1)).unwrap_or(10_000);
    let scale = match Scale::from_objects(objects) {
        Ok(scale) => scale,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let dir = env::temp_dir().join(format!("mikura-funnel-log-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("mkdir: {error}");
        return ExitCode::from(1);
    }
    let log_path = dir.join("objects.jsonl");
    let snapshot = Instant::now();
    if let Err(error) = funnel_snapshot(&log_path, &scale) {
        eprintln!("snapshot: {error}");
        return ExitCode::from(1);
    }
    let snapshot_ms = snapshot.elapsed().as_millis();
    let mut live = match rebuild(&log_path) {
        Ok(map) => map,
        Err(error) => {
            eprintln!("rebuild: {error}");
            return ExitCode::from(1);
        }
    };
    let hop = two_hop_count(&live);
    let incr = Instant::now();
    if let Err(error) = funnel_incremental(&log_path, &scale, 1_000, 2) {
        eprintln!("incremental: {error}");
        return ExitCode::from(1);
    }
    let incremental_ms = incr.elapsed().as_millis();
    live = match rebuild(&log_path) {
        Ok(map) => map,
        Err(error) => {
            eprintln!("rebuild after incremental: {error}");
            return ExitCode::from(1);
        }
    };
    let rebuilt = match rebuild(&log_path) {
        Ok(map) => map,
        Err(error) => {
            eprintln!("second rebuild: {error}");
            return ExitCode::from(1);
        }
    };
    let identity_hold = identity_digest(&live) == identity_digest(&rebuilt);
    let hidden = live.values().filter(|object| object.hidden).count();
    let visible = live.values().filter(|object| !object.hidden).count();
    println!("spike=001-funnel-log");
    println!("objects={}", scale.total());
    println!("customers={}", scale.customers);
    println!("orders={}", scale.orders);
    println!("shipments={}", scale.shipments);
    println!("snapshot_ms={snapshot_ms}");
    println!("incremental_1k_ms={incremental_ms}");
    println!("visible={visible}");
    println!("hidden={hidden}");
    println!("two_hop_visible_customers={hop}");
    println!("rebuild_identity_hold={identity_hold}");
    let _ = fs::remove_dir_all(&dir);
    if identity_hold && hop > 0 && hidden > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn funnel_snapshot(path: &Path, scale: &Scale) -> Result<(), String> {
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(file);
    for id in 0..scale.customers {
        write_record(
            &mut out,
            &Record {
                gen: 1,
                kind: "Customer".into(),
                key: format!("c{id}"),
                hidden: id % 100 == 0,
                props: HashMap::from([("region".into(), region(id))]),
            },
        )?;
    }
    for id in 0..scale.orders {
        write_record(
            &mut out,
            &Record {
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
        write_record(
            &mut out,
            &Record {
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

fn funnel_incremental(path: &Path, scale: &Scale, n: i64, gen: u64) -> Result<(), String> {
    let file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(file);
    for i in 0..n {
        let id = i % scale.customers;
        write_record(
            &mut out,
            &Record {
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

fn write_record(out: &mut BufWriter<File>, record: &Record) -> Result<(), String> {
    serde_json::to_writer(&mut *out, record).map_err(|e| e.to_string())?;
    out.write_all(b"\n").map_err(|e| e.to_string())
}

fn rebuild(path: &Path) -> Result<HashMap<(String, String), Object>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let mut live = HashMap::new();
    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.is_empty() {
            continue;
        }
        let record: Record = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        live.insert(
            (record.kind.clone(), record.key.clone()),
            Object {
                kind: record.kind,
                key: record.key,
                hidden: record.hidden,
                props: record.props,
                gen: record.gen,
            },
        );
    }
    Ok(live)
}

fn two_hop_count(live: &HashMap<(String, String), Object>) -> usize {
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

fn identity_digest(live: &HashMap<(String, String), Object>) -> String {
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
    fn rebuild_replays_last_generation_and_keeps_hidden_out_of_hops() {
        let dir = std::env::temp_dir().join("mikura-funnel-log-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.jsonl");
        let scale = Scale::from_objects(1_000).unwrap();
        funnel_snapshot(&path, &scale).unwrap();
        funnel_incremental(&path, &scale, 10, 2).unwrap();
        let live = rebuild(&path).unwrap();
        assert_eq!(live.len() as i64, scale.total());
        assert!(live[&("Customer".into(), "c1".into())].gen == 2);
        assert!(live[&("Customer".into(), "c0".into())].hidden);
        assert!(two_hop_count(&live) > 0);
        let again = rebuild(&path).unwrap();
        assert_eq!(identity_digest(&live), identity_digest(&again));
        let _ = fs::remove_dir_all(&dir);
    }
}
