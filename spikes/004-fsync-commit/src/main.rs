//! Data page fsync, then superblock commit pointer. Throwaway. Not kura's store.
//!
//! Rebuild trusts only pages 1..=committed. An extra CRC-valid page is not
//! authority. A committed page with a bad CRC fails closed.

use crc32fast::Hasher as Crc;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

const PAGE: usize = 4096;
const MAGIC: &[u8; 8] = b"KURASYNC";
const CRC_LEN: usize = 4;
const USED_LEN: usize = 2;
const PAGE_HDR: usize = CRC_LEN + USED_LEN;
const SUPER_COMMIT_OFF: usize = CRC_LEN + 8 + 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SyncPolicy {
    None,
    Page,
}

impl SyncPolicy {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "none" => Ok(Self::None),
            "page" => Ok(Self::Page),
            other => Err(format!("unknown --sync {other}; expected none|page")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Page => "page",
        }
    }
}

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
    committed: u32,
    sync: SyncPolicy,
}

impl Writer {
    fn create(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let file = File::create(path).map_err(|e| e.to_string())?;
        let mut writer = Self {
            file,
            page: empty_data_page(),
            used: 0,
            committed: 0,
            sync,
        };
        writer.write_superblock()?;
        Ok(writer)
    }

    fn open(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        let committed = read_committed(&mut file)?;
        let end = PAGE as u64 * u64::from(1 + committed);
        file.seek(SeekFrom::Start(end)).map_err(|e| e.to_string())?;
        Ok(Self {
            file,
            page: empty_data_page(),
            used: 0,
            committed,
            sync,
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
        if self.sync == SyncPolicy::Page {
            self.file.sync_data().map_err(|e| e.to_string())?;
        }
        self.committed += 1;
        self.write_superblock()?;
        self.page = empty_data_page();
        self.used = 0;
        Ok(())
    }

    fn write_superblock(&mut self) -> Result<(), String> {
        let mut superblock = vec![0u8; PAGE];
        superblock[CRC_LEN..CRC_LEN + 8].copy_from_slice(MAGIC);
        superblock[CRC_LEN + 8..CRC_LEN + 10].copy_from_slice(&(PAGE as u16).to_le_bytes());
        superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4]
            .copy_from_slice(&self.committed.to_le_bytes());
        seal_page(&mut superblock);
        let pos = self.file.stream_position().map_err(|e| e.to_string())?;
        self.file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        self.file.write_all(&superblock).map_err(|e| e.to_string())?;
        if self.sync == SyncPolicy::Page {
            self.file.sync_data().map_err(|e| e.to_string())?;
        }
        self.file
            .seek(SeekFrom::Start(pos.max(PAGE as u64)))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn finish(mut self) -> Result<u32, String> {
        self.flush_page()?;
        self.file.flush().map_err(|e| e.to_string())?;
        Ok(self.committed)
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

fn read_committed(file: &mut File) -> Result<u32, String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut superblock = vec![0u8; PAGE];
    file.read_exact_page(&mut superblock)?;
    verify_page(&superblock).map_err(|e| format!("superblock: {e}"))?;
    if &superblock[CRC_LEN..CRC_LEN + 8] != MAGIC {
        return Err("bad magic".into());
    }
    Ok(u32::from_le_bytes(
        superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4]
            .try_into()
            .unwrap(),
    ))
}

trait ReadExactPage {
    fn read_exact_page(&mut self, buf: &mut [u8]) -> Result<(), String>;
}

impl ReadExactPage for File {
    fn read_exact_page(&mut self, buf: &mut [u8]) -> Result<(), String> {
        use std::io::Read;
        self.read_exact(buf).map_err(|e| e.to_string())
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let objects = flag(&args, "--objects")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    let sync = match flag(&args, "--sync")
        .map(|v| SyncPolicy::parse(&v))
        .unwrap_or(Ok(SyncPolicy::Page))
    {
        Ok(sync) => sync,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let scale = match Scale::from_objects(objects) {
        Ok(scale) => scale,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let dir = env::temp_dir().join(format!("kura-fsync-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    if let Err(error) = fs::create_dir_all(&dir) {
        eprintln!("mkdir: {error}");
        return ExitCode::from(1);
    }
    let log_path = dir.join("objects.pages");
    let snapshot = Instant::now();
    let committed = match funnel_snapshot(&log_path, &scale, sync) {
        Ok(committed) => committed,
        Err(error) => {
            eprintln!("snapshot: {error}");
            return ExitCode::from(1);
        }
    };
    let snapshot_ms = snapshot.elapsed().as_millis();
    let file_bytes = fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
    let live = match rebuild(&log_path) {
        Ok(map) => map,
        Err(error) => {
            eprintln!("rebuild: {error}");
            return ExitCode::from(1);
        }
    };
    let hop = two_hop_count(&live);
    let identity_hold = identity_digest(&live) == identity_digest(&rebuild(&log_path).unwrap());
    let hidden = live.values().filter(|o| o.hidden).count();
    println!("spike=004-fsync-commit");
    println!("sync={}", sync.as_str());
    println!("objects={}", scale.total());
    println!("committed_pages={committed}");
    println!("snapshot_ms={snapshot_ms}");
    println!("log_bytes={file_bytes}");
    println!("visible={}", live.len() - hidden);
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

fn flag(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find_map(|pair| {
        if pair[0] == name {
            Some(pair[1].clone())
        } else {
            None
        }
    })
}

fn funnel_snapshot(path: &Path, scale: &Scale, sync: SyncPolicy) -> Result<u32, String> {
    let mut writer = Writer::create(path, sync)?;
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
    let superblock = &bytes[..PAGE];
    verify_page(superblock).map_err(|e| format!("superblock: {e}"))?;
    if &superblock[CRC_LEN..CRC_LEN + 8] != MAGIC {
        return Err("bad magic".into());
    }
    let committed = u32::from_le_bytes(
        superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let needed = PAGE * (1 + committed);
    if bytes.len() < needed {
        return Err("committed pages missing".into());
    }
    let mut live = HashMap::new();
    for index in 1..=committed {
        let page = &bytes[index * PAGE..(index + 1) * PAGE];
        verify_page(page).map_err(|e| format!("committed page {index}: {e}"))?;
        decode_data_page(page, &mut live)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed_empty_data_page() -> Vec<u8> {
        let mut page = empty_data_page();
        page[CRC_LEN..PAGE_HDR].copy_from_slice(&0u16.to_le_bytes());
        seal_page(&mut page);
        page
    }

    #[test]
    fn uncommitted_page_is_not_authority_and_missing_committed_fails_closed() {
        let dir = std::env::temp_dir().join("kura-fsync-commit-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.pages");
        let scale = Scale::from_objects(2_000).unwrap();
        funnel_snapshot(&path, &scale, SyncPolicy::None).unwrap();
        let committed = rebuild(&path).unwrap();
        assert_eq!(committed.len() as i64, scale.total());

        let mut bytes = std::fs::read(&path).unwrap();
        let before = bytes.len();
        bytes.extend_from_slice(&sealed_empty_data_page());
        std::fs::write(&path, &bytes).unwrap();
        assert!(bytes.len() > before);
        let after_orphan = rebuild(&path).unwrap();
        assert_eq!(identity_digest(&committed), identity_digest(&after_orphan));

        bytes.truncate(PAGE + 100);
        std::fs::write(&path, &bytes).unwrap();
        let err = rebuild(&path).unwrap_err();
        assert!(err.contains("committed pages missing"), "{err}");

        funnel_snapshot(&path, &scale, SyncPolicy::None).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[PAGE + 20] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();
        let err = rebuild(&path).unwrap_err();
        assert!(err.contains("checksum"), "{err}");

        funnel_snapshot(&path, &scale, SyncPolicy::None).unwrap();
        let mut writer = Writer::open(&path, SyncPolicy::None).unwrap();
        writer
            .append(&Record {
                gen: 2,
                kind: "Customer".into(),
                key: "c1".into(),
                hidden: false,
                props: HashMap::from([("region".into(), "uk".into())]),
            })
            .unwrap();
        writer.finish().unwrap();
        let live = rebuild(&path).unwrap();
        assert_eq!(live[&("Customer".into(), "c1".into())].gen, 2);
        let _ = fs::remove_dir_all(&dir);
    }
}
