//! Persist reachable hop keys beside the object log. Throwaway. Not mikura's store.
//!
//! Restart loads the hop sidecar instead of replaying JSONL. The sidecar is a
//! checksummed blob replaced atomically. JSONL remains the object-log vehicle.

use crc32fast::Hasher as Crc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

const HOP_MAGIC: &[u8; 8] = b"KURAHOP\n";

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
    visible_customers: HashSet<String>,
    orders_by_customer: HashMap<String, HashSet<String>>,
    order_customer: HashMap<String, String>,
    shipments_by_order: HashMap<String, HashSet<String>>,
    reachable: HashSet<String>,
}

impl Live {
    fn new() -> Self {
        Self {
            objects: HashMap::new(),
            visible_customers: HashSet::new(),
            orders_by_customer: HashMap::new(),
            order_customer: HashMap::new(),
            shipments_by_order: HashMap::new(),
            reachable: HashSet::new(),
        }
    }

    fn apply(&mut self, record: Record) {
        let id = (record.kind.clone(), record.key.clone());
        if let Some(old) = self.objects.remove(&id) {
            self.unindex(&old);
        }
        self.index(&record);
        self.objects.insert(id, record);
    }

    fn unindex(&mut self, record: &Record) {
        match record.kind.as_str() {
            "Customer" => {
                self.visible_customers.remove(&record.key);
                self.reachable.remove(&record.key);
            }
            "Order" => {
                if let Some(customer_id) = self.order_customer.remove(&record.key) {
                    if let Some(orders) = self.orders_by_customer.get_mut(&customer_id) {
                        orders.remove(&record.key);
                    }
                    self.refresh_reachable(&customer_id);
                }
            }
            "Shipment" => {
                if let Some(order_id) = record.props.get("order_id") {
                    if let Some(shipments) = self.shipments_by_order.get_mut(order_id) {
                        shipments.remove(&record.key);
                    }
                    if let Some(customer_id) = self.order_customer.get(order_id).cloned() {
                        self.refresh_reachable(&customer_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn index(&mut self, record: &Record) {
        if record.hidden {
            return;
        }
        match record.kind.as_str() {
            "Customer" => {
                self.visible_customers.insert(record.key.clone());
                self.refresh_reachable(&record.key);
            }
            "Order" => {
                if let Some(customer_id) = record.props.get("customer_id") {
                    self.order_customer
                        .insert(record.key.clone(), customer_id.clone());
                    self.orders_by_customer
                        .entry(customer_id.clone())
                        .or_default()
                        .insert(record.key.clone());
                    self.refresh_reachable(customer_id);
                }
            }
            "Shipment" => {
                if let Some(order_id) = record.props.get("order_id") {
                    self.shipments_by_order
                        .entry(order_id.clone())
                        .or_default()
                        .insert(record.key.clone());
                    if let Some(customer_id) = self.order_customer.get(order_id).cloned() {
                        self.refresh_reachable(&customer_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn refresh_reachable(&mut self, customer_id: &str) {
        if !self.visible_customers.contains(customer_id) {
            self.reachable.remove(customer_id);
            return;
        }
        let ok = self
            .orders_by_customer
            .get(customer_id)
            .is_some_and(|orders| {
                orders.iter().any(|order_id| {
                    self.shipments_by_order
                        .get(order_id)
                        .is_some_and(|set| !set.is_empty())
                })
            });
        if ok {
            self.reachable.insert(customer_id.to_string());
        } else {
            self.reachable.remove(customer_id);
        }
    }

    fn hop_count(&self) -> usize {
        self.reachable.len()
    }
}

fn persist_hop(path: &Path, reachable: &HashSet<String>) -> Result<u64, String> {
    let tmp = path.with_extension("hop.tmp");
    let mut body = Vec::new();
    body.extend_from_slice(HOP_MAGIC);
    let n = u32::try_from(reachable.len()).map_err(|_| "too many keys".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    let mut keys: Vec<_> = reachable.iter().cloned().collect();
    keys.sort();
    for key in keys {
        let len = u16::try_from(key.len()).map_err(|_| "key too long".to_string())?;
        body.extend_from_slice(&len.to_le_bytes());
        body.extend_from_slice(key.as_bytes());
    }
    let mut hasher = Crc::new();
    hasher.update(&body);
    let crc = hasher.finalize();
    let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
    file.write_all(&body).map_err(|e| e.to_string())?;
    file.write_all(&crc.to_le_bytes()).map_err(|e| e.to_string())?;
    file.sync_data().map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(body.len() as u64 + 4)
}

fn load_hop(path: &Path) -> Result<HashSet<String>, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < 8 + 4 + 4 {
        return Err("hop sidecar too short".into());
    }
    let (body, crc_bytes) = bytes.split_at(bytes.len() - 4);
    let expected = u32::from_le_bytes(crc_bytes.try_into().unwrap());
    let mut hasher = Crc::new();
    hasher.update(body);
    if hasher.finalize() != expected {
        return Err("hop checksum mismatch".into());
    }
    if &body[..8] != HOP_MAGIC {
        return Err("bad hop magic".into());
    }
    let n = u32::from_le_bytes(body[8..12].try_into().unwrap()) as usize;
    let mut cur = &body[12..];
    let mut reachable = HashSet::with_capacity(n);
    for _ in 0..n {
        if cur.len() < 2 {
            return Err("short hop key len".into());
        }
        let len = u16::from_le_bytes(cur[..2].try_into().unwrap()) as usize;
        cur = &cur[2..];
        if cur.len() < len {
            return Err("short hop key".into());
        }
        let key = String::from_utf8(cur[..len].to_vec()).map_err(|_| "hop key not utf8")?;
        cur = &cur[len..];
        reachable.insert(key);
    }
    if !cur.is_empty() {
        return Err("trailing hop bytes".into());
    }
    if reachable.len() != n {
        return Err("duplicate hop keys".into());
    }
    Ok(reachable)
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
    let dir = env::temp_dir().join(format!("mikura-persist-hop-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("mkdir: {error}");
        return ExitCode::from(1);
    }
    let log_path = dir.join("objects.jsonl");
    let hop_path = dir.join("hop.bin");
    let mut live = Live::new();
    let snapshot = Instant::now();
    if let Err(error) = funnel_snapshot(&log_path, &scale, &mut live) {
        eprintln!("snapshot: {error}");
        return ExitCode::from(1);
    }
    let snapshot_ms = snapshot.elapsed().as_millis();
    let persist = Instant::now();
    let hop_bytes = match persist_hop(&hop_path, &live.reachable) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("persist: {error}");
            return ExitCode::from(1);
        }
    };
    let persist_ms = persist.elapsed().as_millis();
    let live_hop = live.hop_count();
    drop(live);
    let load = Instant::now();
    let loaded = match load_hop(&hop_path) {
        Ok(set) => set,
        Err(error) => {
            eprintln!("load: {error}");
            return ExitCode::from(1);
        }
    };
    let load_ms = load.elapsed().as_millis();
    let loaded_hop = loaded.len();
    let rebuild_start = Instant::now();
    let rebuilt = match rebuild(&log_path) {
        Ok(map) => map,
        Err(error) => {
            eprintln!("rebuild: {error}");
            return ExitCode::from(1);
        }
    };
    let rebuild_ms = rebuild_start.elapsed().as_millis();
    let replay = Instant::now();
    let mut replayed = Live::new();
    for record in rebuilt.values().cloned() {
        replayed.apply(record);
    }
    let replay_ms = replay.elapsed().as_millis();
    let dual_read = loaded_hop == live_hop && loaded_hop == replayed.hop_count();
    println!("spike=009-persist-hop");
    println!("objects={}", scale.total());
    println!("snapshot_ms={snapshot_ms}");
    println!("persist_hop_ms={persist_ms}");
    println!("hop_bytes={hop_bytes}");
    println!("load_hop_ms={load_ms}");
    println!("rebuild_log_ms={rebuild_ms}");
    println!("replay_live_ms={replay_ms}");
    println!("two_hop_visible_customers={loaded_hop}");
    println!("dual_read_hold={dual_read}");
    let _ = fs::remove_dir_all(&dir);
    if dual_read && loaded_hop > 0 {
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
    fn persist_load_matches_and_checksum_fails_closed() {
        let dir = std::env::temp_dir().join("mikura-persist-hop-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("objects.jsonl");
        let hop = dir.join("hop.bin");
        let scale = Scale::from_objects(1_000).unwrap();
        let mut live = Live::new();
        funnel_snapshot(&log, &scale, &mut live).unwrap();
        persist_hop(&hop, &live.reachable).unwrap();
        let loaded = load_hop(&hop).unwrap();
        assert_eq!(loaded.len(), live.hop_count());
        assert_eq!(loaded.len(), 9);
        let mut bytes = std::fs::read(&hop).unwrap();
        let n = bytes.len();
        bytes[n - 5] ^= 0x01;
        std::fs::write(&hop, bytes).unwrap();
        let err = load_hop(&hop).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
