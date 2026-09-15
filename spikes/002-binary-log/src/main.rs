//! Length-prefixed records + CRC32. Throwaway. Not kura's store.

use crc32fast::Hasher as Crc;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

const MAGIC: &[u8; 8] = b"KURALOG\n";

#[derive(Clone, Debug, PartialEq, Eq)]
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

fn main() -> ExitCode {
    let objects = parse_objects(env::args().skip(1)).unwrap_or(10_000);
    let scale = match Scale::from_objects(objects) {
        Ok(scale) => scale,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let dir = env::temp_dir().join(format!("kura-binary-log-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("mkdir: {error}");
        return ExitCode::from(1);
    }
    let log_path = dir.join("objects.log");
    let snapshot = Instant::now();
    if let Err(error) = funnel_snapshot(&log_path, &scale) {
        eprintln!("snapshot: {error}");
        return ExitCode::from(1);
    }
    let snapshot_ms = snapshot.elapsed().as_millis();
    let file_bytes = fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
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
            eprintln!("rebuild: {error}");
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
    let hidden = live.values().filter(|o| o.hidden).count();
    let visible = live.values().filter(|o| !o.hidden).count();
    println!("spike=002-binary-log");
    println!("objects={}", scale.total());
    println!("snapshot_ms={snapshot_ms}");
    println!("incremental_1k_ms={incremental_ms}");
    println!("log_bytes={file_bytes}");
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
    let mut file = BufWriter::new(file);
    file.write_all(MAGIC).map_err(|e| e.to_string())?;
    for id in 0..scale.customers {
        append_record(
            &mut file,
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
        append_record(
            &mut file,
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
        append_record(
            &mut file,
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
    file.flush().map_err(|e| e.to_string())
}

fn funnel_incremental(path: &Path, scale: &Scale, n: i64, gen: u64) -> Result<(), String> {
    let file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut file = BufWriter::new(file);
    for i in 0..n {
        let id = i % scale.customers;
        append_record(
            &mut file,
            &Record {
                gen,
                kind: "Customer".into(),
                key: format!("c{id}"),
                hidden: id % 100 == 0,
                props: HashMap::from([("region".into(), region(id + 1))]),
            },
        )?;
    }
    file.flush().map_err(|e| e.to_string())
}

fn append_record(file: &mut impl Write, record: &Record) -> Result<(), String> {
    let body = encode_body(record)?;
    let len = u32::try_from(body.len()).map_err(|_| "record too large".to_string())?;
    let crc = {
        let mut hasher = Crc::new();
        hasher.update(&body);
        hasher.finalize()
    };
    file.write_all(&len.to_le_bytes()).map_err(|e| e.to_string())?;
    file.write_all(&body).map_err(|e| e.to_string())?;
    file.write_all(&crc.to_le_bytes()).map_err(|e| e.to_string())
}

fn encode_body(record: &Record) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    body.extend_from_slice(&record.gen.to_le_bytes());
    body.push(u8::from(record.hidden));
    write_str(&mut body, &record.kind)?;
    write_str(&mut body, &record.key)?;
    let nprops = u16::try_from(record.props.len()).map_err(|_| "too many props".to_string())?;
    body.extend_from_slice(&nprops.to_le_bytes());
    let mut keys: Vec<_> = record.props.keys().cloned().collect();
    keys.sort();
    for key in keys {
        write_str(&mut body, &key)?;
        write_str(&mut body, record.props.get(&key).expect("prop"))?;
    }
    Ok(body)
}

fn write_str(body: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let len = u16::try_from(value.len()).map_err(|_| "string too long".to_string())?;
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(value.as_bytes());
    Ok(())
}

fn rebuild(path: &Path) -> Result<HashMap<(String, String), Record>, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != MAGIC {
        return Err("bad magic".into());
    }
    let mut live = HashMap::new();
    loop {
        match read_record(&mut file)? {
            ReadOutcome::Record(record) => {
                live.insert((record.kind.clone(), record.key.clone()), record);
            }
            ReadOutcome::TruncatedTail => break,
            ReadOutcome::Eof => break,
        }
    }
    Ok(live)
}

enum ReadOutcome {
    Record(Record),
    TruncatedTail,
    Eof,
}

fn read_record(file: &mut File) -> Result<ReadOutcome, String> {
    let mut len_buf = [0u8; 4];
    match file.read(&mut len_buf).map_err(|e| e.to_string())? {
        0 => return Ok(ReadOutcome::Eof),
        n if n < 4 => return Ok(ReadOutcome::TruncatedTail),
        _ => {}
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    if let Err(error) = file.read_exact(&mut body) {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(ReadOutcome::TruncatedTail);
        }
        return Err(error.to_string());
    }
    let mut crc_buf = [0u8; 4];
    if let Err(error) = file.read_exact(&mut crc_buf) {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(ReadOutcome::TruncatedTail);
        }
        return Err(error.to_string());
    }
    let expected = u32::from_le_bytes(crc_buf);
    let mut hasher = Crc::new();
    hasher.update(&body);
    if hasher.finalize() != expected {
        return Err("checksum mismatch".into());
    }
    Ok(ReadOutcome::Record(decode_body(&body)?))
}

fn decode_body(body: &[u8]) -> Result<Record, String> {
    let mut cur = body;
    let gen = u64::from_le_bytes(take::<8>(&mut cur)?.try_into().unwrap());
    let hidden = take::<1>(&mut cur)?[0] != 0;
    let kind = read_str(&mut cur)?;
    let key = read_str(&mut cur)?;
    let nprops = u16::from_le_bytes(take::<2>(&mut cur)?.try_into().unwrap()) as usize;
    let mut props = HashMap::new();
    for _ in 0..nprops {
        let k = read_str(&mut cur)?;
        let v = read_str(&mut cur)?;
        props.insert(k, v);
    }
    if !cur.is_empty() {
        return Err("trailing body bytes".into());
    }
    Ok(Record {
        gen,
        kind,
        key,
        hidden,
        props,
    })
}

fn take<'a, const N: usize>(cur: &mut &'a [u8]) -> Result<&'a [u8], String> {
    if cur.len() < N {
        return Err("short body".into());
    }
    let (head, rest) = cur.split_at(N);
    *cur = rest;
    Ok(head)
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
    fn checksum_mismatch_fails_closed_and_truncated_tail_is_ignored() {
        let dir = std::env::temp_dir().join("kura-binary-log-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.log");
        let scale = Scale::from_objects(1_000).unwrap();
        funnel_snapshot(&path, &scale).unwrap();
        let live = rebuild(&path).unwrap();
        assert_eq!(live.len() as i64, scale.total());
        assert!(two_hop_count(&live) > 0);

        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(0xff);
        std::fs::write(&path, &bytes).unwrap();
        let tailed = rebuild(&path).unwrap();
        assert_eq!(identity_digest(&live), identity_digest(&tailed));

        bytes[20] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();
        let err = rebuild(&path).unwrap_err();
        assert!(err.contains("checksum"));
        let _ = fs::remove_dir_all(&dir);
    }
}
