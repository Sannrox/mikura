//! Persist join maps (order→customer, shipment amounts) beside the object log.
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

const JOIN_MAGIC: &[u8; 8] = b"KURAJOIN";

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct JoinMaps {
    visible_customers: HashSet<String>,
    order_customer: HashMap<String, String>,
    order_amount: HashMap<String, i64>,
}

impl JoinMaps {
    fn hop_count(&self) -> usize {
        let mut seen = HashSet::new();
        for (order_id, customer_id) in &self.order_customer {
            if self.order_amount.contains_key(order_id)
                && self.visible_customers.contains(customer_id)
            {
                seen.insert(customer_id.as_str());
            }
        }
        seen.len()
    }

    fn sum_amount(&self) -> i64 {
        self.order_amount
            .iter()
            .filter_map(|(order_id, amount)| {
                let customer = self.order_customer.get(order_id)?;
                self.visible_customers
                    .contains(customer)
                    .then_some(*amount)
            })
            .sum()
    }
}

struct Live {
    objects: HashMap<(String, String), Record>,
    maps: JoinMaps,
    shipment_amount: HashMap<String, i64>,
    shipment_order: HashMap<String, String>,
}

impl Live {
    fn new() -> Self {
        Self {
            objects: HashMap::new(),
            maps: JoinMaps::default(),
            shipment_amount: HashMap::new(),
            shipment_order: HashMap::new(),
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
                self.maps.visible_customers.remove(&record.key);
            }
            "Order" => {
                self.maps.order_customer.remove(&record.key);
            }
            "Shipment" => {
                if let Some(order_id) = self.shipment_order.remove(&record.key) {
                    let amt = self.shipment_amount.remove(&record.key).unwrap_or(0);
                    if let Some(total) = self.maps.order_amount.get_mut(&order_id) {
                        *total -= amt;
                        if *total == 0 {
                            self.maps.order_amount.remove(&order_id);
                        }
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
                self.maps.visible_customers.insert(record.key.clone());
            }
            "Order" => {
                if let Some(customer_id) = record.props.get("customer_id") {
                    self.maps
                        .order_customer
                        .insert(record.key.clone(), customer_id.clone());
                }
            }
            "Shipment" => {
                let Some(order_id) = record.props.get("order_id") else {
                    return;
                };
                let amount: i64 = record
                    .props
                    .get("amount")
                    .and_then(|raw| raw.parse().ok())
                    .unwrap_or(0);
                self.shipment_order
                    .insert(record.key.clone(), order_id.clone());
                self.shipment_amount.insert(record.key.clone(), amount);
                *self.maps.order_amount.entry(order_id.clone()).or_insert(0) += amount;
            }
            _ => {}
        }
    }
}

fn persist_joins(path: &Path, maps: &JoinMaps) -> Result<u64, String> {
    let tmp = path.with_extension("join.tmp");
    let mut body = Vec::new();
    body.extend_from_slice(JOIN_MAGIC);
    write_set(&mut body, &maps.visible_customers)?;
    write_pairs(&mut body, &maps.order_customer)?;
    write_amounts(&mut body, &maps.order_amount)?;
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
    let order_customer = read_pairs(&mut cur)?;
    let order_amount = read_amounts(&mut cur)?;
    if !cur.is_empty() {
        return Err("trailing join bytes".into());
    }
    Ok(JoinMaps {
        visible_customers,
        order_customer,
        order_amount,
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

fn write_pairs(body: &mut Vec<u8>, map: &HashMap<String, String>) -> Result<(), String> {
    let n = u32::try_from(map.len()).map_err(|_| "too many pairs".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    let mut keys: Vec<_> = map.keys().cloned().collect();
    keys.sort();
    for key in keys {
        write_str(body, &key)?;
        write_str(body, map.get(&key).expect("pair"))?;
    }
    Ok(())
}

fn write_amounts(body: &mut Vec<u8>, map: &HashMap<String, i64>) -> Result<(), String> {
    let n = u32::try_from(map.len()).map_err(|_| "too many amounts".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    let mut keys: Vec<_> = map.keys().cloned().collect();
    keys.sort();
    for key in keys {
        write_str(body, &key)?;
        body.extend_from_slice(&map.get(&key).expect("amt").to_le_bytes());
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
    let n = u32::from_le_bytes(take::<4>(cur)?.try_into().unwrap()) as usize;
    let mut set = HashSet::with_capacity(n);
    for _ in 0..n {
        set.insert(read_str(cur)?);
    }
    Ok(set)
}

fn read_pairs(cur: &mut &[u8]) -> Result<HashMap<String, String>, String> {
    let n = u32::from_le_bytes(take::<4>(cur)?.try_into().unwrap()) as usize;
    let mut map = HashMap::with_capacity(n);
    for _ in 0..n {
        let k = read_str(cur)?;
        let v = read_str(cur)?;
        map.insert(k, v);
    }
    Ok(map)
}

fn read_amounts(cur: &mut &[u8]) -> Result<HashMap<String, i64>, String> {
    let n = u32::from_le_bytes(take::<4>(cur)?.try_into().unwrap()) as usize;
    let mut map = HashMap::with_capacity(n);
    for _ in 0..n {
        let k = read_str(cur)?;
        let amt = i64::from_le_bytes(take::<8>(cur)?.try_into().unwrap());
        map.insert(k, amt);
    }
    Ok(map)
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
    let live_hop = live.maps.hop_count();
    let live_sum = live.maps.sum_amount();
    let persist = Instant::now();
    let join_bytes = match persist_joins(&join_path, &live.maps) {
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
    let loaded_hop = loaded.hop_count();
    let loaded_sum = loaded.sum_amount();
    let eval = Instant::now();
    let _ = (loaded.hop_count(), loaded.sum_amount());
    let eval_ms = eval.elapsed().as_millis();
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
    for record in rebuilt.into_values() {
        replayed.apply(record);
    }
    let replay_ms = replay.elapsed().as_millis();
    let dual_read = loaded_hop == live_hop
        && loaded_sum == live_sum
        && loaded_hop == replayed.maps.hop_count()
        && loaded_sum == replayed.maps.sum_amount();
    println!("spike=010-persist-joins");
    println!("objects={}", scale.total());
    println!("snapshot_ms={snapshot_ms}");
    println!("persist_joins_ms={persist_ms}");
    println!("join_bytes={join_bytes}");
    println!("load_joins_ms={load_ms}");
    println!("eval_count_sum_ms={eval_ms}");
    println!("rebuild_log_ms={rebuild_ms}");
    println!("replay_live_ms={replay_ms}");
    println!("two_hop_visible_customers={loaded_hop}");
    println!("two_hop_sum_amount={loaded_sum}");
    println!("dual_read_hold={dual_read}");
    let _ = fs::remove_dir_all(&dir);
    if dual_read && loaded_hop > 0 && loaded_sum > 0 {
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
    fn persist_load_matches_count_and_sum_and_checksum_fails_closed() {
        let dir = std::env::temp_dir().join("kura-persist-joins-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("objects.jsonl");
        let joins = dir.join("joins.bin");
        let scale = Scale::from_objects(1_000).unwrap();
        let mut live = Live::new();
        funnel_snapshot(&log, &scale, &mut live).unwrap();
        persist_joins(&joins, &live.maps).unwrap();
        let loaded = load_joins(&joins).unwrap();
        assert_eq!(loaded.hop_count(), live.maps.hop_count());
        assert_eq!(loaded.sum_amount(), live.maps.sum_amount());
        assert_eq!(loaded.hop_count(), 9);
        assert!(loaded.sum_amount() > 0);
        drop(live);
        assert_eq!(loaded.hop_count(), 9);
        let mut bytes = std::fs::read(&joins).unwrap();
        let n = bytes.len();
        bytes[n - 5] ^= 0x01;
        std::fs::write(&joins, bytes).unwrap();
        let err = load_joins(&joins).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
