//! Fixed-size checksummed pages. Throwaway. Not kura's store.
//!
//! A torn write cannot look like a valid short record: readers only accept
//! complete 4KiB pages whose CRC covers the whole page.

use crc32fast::Hasher as Crc;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

const PAGE: usize = 4096;
const MAGIC: &[u8; 8] = b"KURAPAGE";
const CRC_LEN: usize = 4;
const USED_LEN: usize = 2;
const PAGE_HDR: usize = CRC_LEN + USED_LEN;

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

struct Writer {
    file: File,
    page: Vec<u8>,
    used: usize,
}

impl Writer {
    fn create(path: &Path) -> Result<Self, String> {
        let mut file = File::create(path).map_err(|e| e.to_string())?;
        let mut superblock = vec![0u8; PAGE];
        superblock[CRC_LEN..CRC_LEN + 8].copy_from_slice(MAGIC);
        superblock[CRC_LEN + 8..CRC_LEN + 10].copy_from_slice(&(PAGE as u16).to_le_bytes());
        seal_page(&mut superblock);
        file.write_all(&superblock).map_err(|e| e.to_string())?;
        Ok(Self {
            file,
            page: empty_data_page(),
            used: 0,
        })
    }

    fn append_existing(path: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            file,
            page: empty_data_page(),
            used: 0,
        })
    }

    fn append(&mut self, record: &Record) -> Result<(), String> {
        let body = encode_body(record)?;
        let framed = framed_record(&body)?;
        if PAGE_HDR + framed.len() > PAGE {
            return Err("record larger than a page".into());
        }
        if PAGE_HDR + self.used + framed.len() > PAGE {
            self.flush_page()?;
        }
        let start = PAGE_HDR + self.used;
        self.page[start..start + framed.len()].copy_from_slice(&framed);
        self.used += framed.len();
        Ok(())
    }

    fn flush_page(&mut self) -> Result<(), String> {
        if self.used == 0 {
            return Ok(());
        }
        self.page[CRC_LEN..PAGE_HDR].copy_from_slice(&(self.used as u16).to_le_bytes());
        seal_page(&mut self.page);
        self.file.write_all(&self.page).map_err(|e| e.to_string())?;
        self.page = empty_data_page();
        self.used = 0;
        Ok(())
    }

    fn finish(mut self) -> Result<(), String> {
        self.flush_page()?;
        self.file.flush().map_err(|e| e.to_string())
    }
}

fn empty_data_page() -> Vec<u8> {
    vec![0u8; PAGE]
}

fn seal_page(page: &mut [u8]) {
    let mut hasher = Crc::new();
    hasher.update(&page[CRC_LEN..]);
    page[..CRC_LEN].copy_from_slice(&hasher.finalize().to_le_bytes());
}

fn framed_record(body: &[u8]) -> Result<Vec<u8>, String> {
    let len = u16::try_from(body.len()).map_err(|_| "record too large".to_string())?;
    let mut out = Vec::with_capacity(2 + body.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(body);
    Ok(out)
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
    let dir = env::temp_dir().join(format!("kura-paged-log-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("mkdir: {error}");
        return ExitCode::from(1);
    }
    let log_path = dir.join("objects.pages");
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
    println!("spike=003-paged-log");
    println!("page_bytes={PAGE}");
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
    let mut writer = Writer::create(path)?;
    for id in 0..scale.customers {
        writer.append(&Record {
            gen: 1,
            kind: "Customer".into(),
            key: format!("c{id}"),
            hidden: id % 100 == 0,
            props: HashMap::from([("region".into(), region(id))]),
        })?;
    }
    for id in 0..scale.orders {
        writer.append(&Record {
            gen: 1,
            kind: "Order".into(),
            key: format!("o{id}"),
            hidden: id % 100 == 0,
            props: HashMap::from([
                ("customer_id".into(), format!("c{}", id % scale.customers)),
                ("amount".into(), format!("{}", (id % 100) + 1)),
            ]),
        })?;
    }
    for id in 0..scale.shipments {
        writer.append(&Record {
            gen: 1,
            kind: "Shipment".into(),
            key: format!("s{id}"),
            hidden: id % 100 == 0,
            props: HashMap::from([
                ("order_id".into(), format!("o{}", id % scale.orders)),
                ("amount".into(), format!("{}", (id % 50) + 1)),
            ]),
        })?;
    }
    writer.finish()
}

fn funnel_incremental(path: &Path, scale: &Scale, n: i64, gen: u64) -> Result<(), String> {
    let mut writer = Writer::append_existing(path)?;
    writer.file.seek_end()?;
    for i in 0..n {
        let id = i % scale.customers;
        writer.append(&Record {
            gen,
            kind: "Customer".into(),
            key: format!("c{id}"),
            hidden: id % 100 == 0,
            props: HashMap::from([("region".into(), region(id + 1))]),
        })?;
    }
    writer.finish()
}

trait SeekEnd {
    fn seek_end(&mut self) -> Result<(), String>;
}

impl SeekEnd for File {
    fn seek_end(&mut self) -> Result<(), String> {
        use std::io::{Seek, SeekFrom};
        self.seek(SeekFrom::End(0)).map(|_| ()).map_err(|e| e.to_string())
    }
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
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < PAGE {
        return Err("missing superblock".into());
    }
    let complete = bytes.len() / PAGE;
    let superblock = &bytes[..PAGE];
    verify_page(superblock).map_err(|e| format!("superblock: {e}"))?;
    if &superblock[CRC_LEN..CRC_LEN + 8] != MAGIC {
        return Err("bad magic".into());
    }
    let mut live = HashMap::new();
    for index in 1..complete {
        let page = &bytes[index * PAGE..(index + 1) * PAGE];
        match verify_page(page) {
            Ok(()) => decode_data_page(page, &mut live)?,
            Err(error) if index + 1 == complete => {
                // Last complete page failed CRC: treat as torn write.
                let _ = error;
                break;
            }
            Err(error) => return Err(format!("page {index}: {error}")),
        }
    }
    Ok(live)
}

fn verify_page(page: &[u8]) -> Result<(), String> {
    if page.len() != PAGE {
        return Err("short page".into());
    }
    let expected = u32::from_le_bytes(page[..CRC_LEN].try_into().unwrap());
    let mut hasher = Crc::new();
    hasher.update(&page[CRC_LEN..]);
    if hasher.finalize() != expected {
        return Err("checksum mismatch".into());
    }
    Ok(())
}

fn decode_data_page(
    page: &[u8],
    live: &mut HashMap<(String, String), Record>,
) -> Result<(), String> {
    let used = u16::from_le_bytes(page[CRC_LEN..PAGE_HDR].try_into().unwrap()) as usize;
    if PAGE_HDR + used > PAGE {
        return Err("used past page".into());
    }
    let mut cur = &page[PAGE_HDR..PAGE_HDR + used];
    while !cur.is_empty() {
        if cur.len() < 2 {
            return Err("short record len".into());
        }
        let len = u16::from_le_bytes(cur[..2].try_into().unwrap()) as usize;
        cur = &cur[2..];
        if cur.len() < len {
            return Err("short record body".into());
        }
        let record = decode_body(&cur[..len])?;
        cur = &cur[len..];
        live.insert((record.kind.clone(), record.key.clone()), record);
    }
    Ok(())
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
    fn torn_last_page_is_dropped_middle_page_fails_closed() {
        let dir = std::env::temp_dir().join("kura-paged-log-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.pages");
        let scale = Scale::from_objects(5_000).unwrap();
        funnel_snapshot(&path, &scale).unwrap();
        let full = rebuild(&path).unwrap();
        assert_eq!(full.len() as i64, scale.total());

        let mut bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() % PAGE, 0);
        assert!(bytes.len() > PAGE * 3);
        let original_len = bytes.len();
        bytes.truncate(original_len - 200);
        std::fs::write(&path, &bytes).unwrap();
        let torn = rebuild(&path).unwrap();
        assert!(torn.len() < full.len());
        assert!(!torn.is_empty());

        bytes.resize(original_len, 0);
        // Restore a valid snapshot then flip a byte in page 1 (first data page).
        funnel_snapshot(&path, &scale).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[PAGE + 20] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();
        let err = rebuild(&path).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }
}
