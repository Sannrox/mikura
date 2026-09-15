//! Persist join maps (order→customer, shipment→order+amount) beside the log.
//! Throwaway. Not kura's store. Restart answers two-hop count and sum(amount)
//! without replaying the Live object map.

use crc32fast::Hasher as Crc;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

const JOIN_MAGIC: &[u8; 8] = b"KURAJN\n\n";

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

/// Restart projection: enough to count two-hop customers and sum shipment amount.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct JoinMaps {
    visible_customers: HashSet<String>,
    order_customer: HashMap<String, String>,
    shipment_order_amount: HashMap<String, (String, i64)>,
}

impl JoinMaps {
    fn hop_count(&self) -> usize {
        let mut reachable = HashSet::new();
        for (order_id, amount) in self.shipment_order_amount.values() {
            let _ = amount;
            if let Some(customer_id) = self.order_customer.get(order_id) {
                if self.visible_customers.contains(customer_id) {
                    reachable.insert(customer_id.clone());
                }
            }
        }
        reachable.len()
    }

    fn sum_amount(&self) -> i64 {
        let mut total = 0i64;
        for (order_id, amount) in self.shipment_order_amount.values() {
            if let Some(customer_id) = self.order_customer.get(order_id) {
                if self.visible_customers.contains(customer_id) {
                    total += amount;
                }
            }
        }
        total
    }
}

struct Live {
    objects: HashMap<(String, String), Record>,
    joins: JoinMaps,
}

impl Live {
    fn new() -> Self {
        Self {
            objects: HashMap::new(),
            joins: JoinMaps::default(),
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
                self.joins.visible_customers.remove(&record.key);
            }
            "Order" => {
                self.joins.order_customer.remove(&record.key);
            }
            "Shipment" => {
                self.joins.shipment_order_amount.remove(&record.key);
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
                self.joins.visible_customers.insert(record.key.clone());
            }
            "Order" => {
                if let Some(customer_id) = record.props.get("customer_id") {
                    self.joins
                        .order_customer
                        .insert(record.key.clone(), customer_id.clone());
                }
            }
            "Shipment" => {
                if let (Some(order_id), Some(amount)) = (
                    record.props.get("order_id"),
                    record.props.get("amount").and_then(|raw| raw.parse().ok()),
                ) {
                    self.joins
                        .shipment_order_amount
                        .insert(record.key.clone(), (order_id.clone(), amount));
                }
            }
            _ => {}
        }
    }
}

fn persist_joins(path: &Path, joins: &JoinMaps) -> Result<u64, String> {
    let tmp = path.with_extension("join.tmp");
    let mut body = Vec::new();
    body.extend_from_slice(JOIN_MAGIC);
    write_set(&mut body, &joins.visible_customers)?;
    write_map(&mut body, &joins.order_customer)?;
    write_shipments(&mut body, &joins.shipment_order_amount)?;
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

fn load_joins(path: &Path) -> Result<JoinMaps, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < 8 + 4 {
        return Err("join sidecar too short".into());
    }
    let (body, crc_bytes) = bytes.split_at(bytes.len() - 4);
    let expected = u32::from_le_bytes(crc_bytes.try_into().unwrap());
    let mut hasher = Crc::new();
    hasher.update(body);
    if hasher.finalize() != expected {
        return Err("join checksum mismatch".into());
    }
    if body.len() < 8 || &body[..8] != JOIN_MAGIC {
        return Err("bad join magic".into());
    }
    let mut cur = &body[8..];
    let visible_customers = read_set(&mut cur)?;
    let order_customer = read_map(&mut cur)?;
    let shipment_order_amount = read_shipments(&mut cur)?;
    if !cur.is_empty() {
        return Err("trailing join bytes".into());
    }
    Ok(JoinMaps {
        visible_customers,
        order_customer,
        shipment_order_amount,
    })
}

fn write_set(body: &mut Vec<u8>, set: &HashSet<String>) -> Result<(), String> {
    let n = u32::try_from(set.len()).map_err(|_| "too many keys".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    let mut keys: Vec<_> = set.iter().cloned().collect();
    keys.sort();
    for key in keys {
        write_str(body, &key)?;
    }
    Ok(())
}

fn write_map(body: &mut Vec<u8>, map: &HashMap<String, String>) -> Result<(), String> {
    let n = u32::try_from(map.len()).map_err(|_| "too many orders".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    let mut keys: Vec<_> = map.keys().cloned().collect();
    keys.sort();
    for key in keys {
        write_str(body, &key)?;
        write_str(body, map.get(&key).expect("order"))?;
    }
    Ok(())
}

fn write_shipments(
    body: &mut Vec<u8>,
    map: &HashMap<String, (String, i64)>,
) -> Result<(), String> {
    let n = u32::try_from(map.len()).map_err(|_| "too many shipments".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    let mut keys: Vec<_> = map.keys().cloned().collect();
    keys.sort();
    for key in keys {
        let (order_id, amount) = map.get(&key).expect("shipment");
        write_str(body, &key)?;
        write_str(body, order_id)?;
        body.extend_from_slice(&amount.to_le_bytes());
    }
    Ok(())
}

fn write_str(body: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let len = u16::try_from(value.len()).map_err(|_| "string too long".to_string())?;
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(value.as_bytes());
    Ok(())
}

fn read_set(cur: &mut &[u8]) -> Result<HashSet<String>, String> {
    let n = read_u32(cur)? as usize;
    let mut set = HashSet::with_capacity(n);
    for _ in 0..n {
        set.insert(read_str(cur)?);
    }
    Ok(set)
}

fn read_map(cur: &mut &[u8]) -> Result<HashMap<String, String>, String> {
    let n = read_u32(cur)? as usize;
    let mut map = HashMap::with_capacity(n);
    for _ in 0..n {
        let k = read_str(cur)?;
        let v = read_str(cur)?;
        map.insert(k, v);
    }
    Ok(map)
}

fn read_shipments(cur: &mut &[u8]) -> Result<HashMap<String, (String, i64)>, String> {
    let n = read_u32(cur)? as usize;
    let mut map = HashMap::with_capacity(n);
    for _ in 0..n {
        let k = read_str(cur)?;
        let order = read_str(cur)?;
        let amount = i64::from_le_bytes(take::<8>(cur)?.try_into().unwrap());
        map.insert(k, (order, amount));
    }
    Ok(map)
}

fn read_u32(cur: &mut &[u8]) -> Result<u32, String> {
    Ok(u32::from_le_bytes(take::<4>(cur)?.try_into().unwrap()))
}

fn read_str(cur: &mut &[u8]) -> Result<String, String> {
    let len = u16::from_le_bytes(take::<2>(cur)?.try_into().unwrap()) as usize;
    if cur.len() < len {
        return Err("short string".into());
    }
    let (head, rest) = cur.split_at(len);
    *cur = rest;
    String::from_utf8(head.to_vec()).map_err(|_| "not utf8".into())
}

fn take<'a, const N: usize>(cur: &mut &'a [u8]) -> Result<&'a [u8], String> {
    if cur.len() < N {
        return Err("short body".into());
    }
    let (head, rest) = cur.split_at(N);
    *cur = rest;
    Ok(head)
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
    let dir = env::temp_dir().join(format!("kura-persist-joins-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("mkdir: {error}");
        return ExitCode::from(1);
    }
    let log_path = dir.join("objects.jsonl");
    let join_path = dir.join("joins.bin");
    let mut live = Live::new();
    let snapshot = Instant::now();
    if let Err(error) = funnel_snapshot(&log_path, &scale, &mut live) {
        eprintln!("snapshot: {error}");
        return ExitCode::from(1);
    }
    let snapshot_ms = snapshot.elapsed().as_millis();
    let live_count = live.joins.hop_count();
    let live_sum = live.joins.sum_amount();
    let persist = Instant::now();
    let join_bytes = match persist_joins(&join_path, &live.joins) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("persist: {error}");
            return ExitCode::from(1);
        }
    };
    let persist_ms = persist.elapsed().as_millis();
    drop(live);
    let load = Instant::now();
    let loaded = match load_joins(&join_path) {
        Ok(maps) => maps,
        Err(error) => {
            eprintln!("load: {error}");
            return ExitCode::from(1);
        }
    };
    let load_ms = load.elapsed().as_millis();
    let loaded_count = loaded.hop_count();
    let loaded_sum = loaded.sum_amount();
    let replay_start = Instant::now();
    let mut replayed = Live::new();
    if let Err(error) = replay_log(&log_path, &mut replayed) {
        eprintln!("replay: {error}");
        return ExitCode::from(1);
    }
    let replay_ms = replay_start.elapsed().as_millis();
    let dual_read = loaded_count == live_count
        && loaded_sum == live_sum
        && loaded_count == replayed.joins.hop_count()
        && loaded_sum == replayed.joins.sum_amount();
    println!("spike=010-persist-joins");
    println!("objects={}", scale.total());
    println!("snapshot_ms={snapshot_ms}");
    println!("persist_joins_ms={persist_ms}");
    println!("join_bytes={join_bytes}");
    println!("load_joins_ms={load_ms}");
    println!("replay_live_ms={replay_ms}");
    println!("two_hop_visible_customers={loaded_count}");
    println!("sum_shipment_amount={loaded_sum}");
    println!("dual_read_hold={dual_read}");
    let _ = fs::remove_dir_all(&dir);
    if dual_read && loaded_count > 0 && loaded_sum > 0 {
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

fn replay_log(path: &Path, live: &mut Live) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.is_empty() {
            continue;
        }
        let record: Record = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        live.apply(record);
    }
    Ok(())
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
    fn persist_load_matches_live_count_and_sum_checksum_fails_closed_hidden_out() {
        let dir = std::env::temp_dir().join("kura-persist-joins-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("objects.jsonl");
        let joins = dir.join("joins.bin");
        let scale = Scale::from_objects(1_000).unwrap();
        let mut live = Live::new();
        funnel_snapshot(&log, &scale, &mut live).unwrap();
        assert!(!live.joins.visible_customers.contains("c0"));
        assert!(!live.joins.order_customer.contains_key("o0"));
        assert!(!live.joins.shipment_order_amount.contains_key("s0"));
        let live_count = live.joins.hop_count();
        let live_sum = live.joins.sum_amount();
        assert_eq!(live_count, 9);
        assert!(live_sum > 0);
        persist_joins(&joins, &live.joins).unwrap();
        drop(live);
        let loaded = load_joins(&joins).unwrap();
        assert_eq!(loaded.hop_count(), live_count);
        assert_eq!(loaded.sum_amount(), live_sum);
        assert!(!loaded.visible_customers.contains("c0"));
        let mut bytes = std::fs::read(&joins).unwrap();
        let n = bytes.len();
        bytes[n - 5] ^= 0x01;
        std::fs::write(&joins, bytes).unwrap();
        let err = load_joins(&joins).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
