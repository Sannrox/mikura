//! Throwaway: does a 4 KiB page file plus a fixed-size page cache keep RSS
//! near the cache budget while answering point reads, while a HashMap of the
//! same records grows with N?
//!
//! Not the mikura store of record. Magic `KURAPAGE` is spike-only.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

const PAGE: usize = 4096;
const REC: usize = 64;
const HDR: usize = 16;
const RECS_PER_PAGE: usize = (PAGE - HDR) / REC;
const LOOKUPS: usize = 100_000;

fn main() {
    let mut n: u64 = 10_000;
    let mut cache_pages: usize = 32;
    let mut mode = String::from("paged");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--n" => n = args.next().and_then(|s| s.parse().ok()).unwrap_or(n),
            "--cache-pages" => {
                cache_pages = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(cache_pages)
            }
            "--mode" => {
                if let Some(value) = args.next() {
                    mode = value;
                }
            }
            other => {
                eprintln!("unknown arg {other}");
                std::process::exit(2);
            }
        }
    }

    match mode.as_str() {
        "hashmap" => run_hashmap(n),
        "blob_decode" => run_blob_decode(n),
        "paged" => run_paged(n, cache_pages),
        other => {
            eprintln!("mode must be hashmap, blob_decode, or paged, got {other}");
            std::process::exit(2);
        }
    }
}

fn run_hashmap(n: u64) {
    let mut map = HashMap::with_capacity(n as usize);
    for id in 0..n {
        map.insert(id, record(id));
    }
    let (hits, ns) = lookup_hashmap(&map, n);
    report("hashmap", n, 0, map.len() as u64 * REC as u64, hits, ns);
}

fn run_blob_decode(n: u64) {
    let path = temp_path("blob");
    write_pages(&path, n);
    let bytes = fs::read(&path).expect("read blob");
    let file_bytes = bytes.len() as u64;
    let mut map = HashMap::with_capacity(n as usize);
    for page in bytes.chunks(PAGE) {
        if page.len() != PAGE {
            break;
        }
        let count = u16::from_le_bytes(page[0..2].try_into().unwrap()) as usize;
        for i in 0..count {
            let rec = &page[HDR + i * REC..HDR + (i + 1) * REC];
            let id = u64::from_le_bytes(rec[0..8].try_into().unwrap());
            let mut payload = [0u8; REC];
            payload.copy_from_slice(rec);
            map.insert(id, payload);
        }
    }
    let (hits, ns) = lookup_hashmap(&map, n);
    let _ = fs::remove_file(&path);
    report(
        "blob_decode",
        n,
        file_bytes,
        map.len() as u64 * REC as u64,
        hits,
        ns,
    );
}

fn run_paged(n: u64, cache_pages: usize) {
    let path = temp_path("paged");
    write_pages(&path, n);
    let file_bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut file = File::open(&path).expect("open pages");
    let mut cache: HashMap<u32, Box<[u8; PAGE]>> = HashMap::new();
    let mut lru: VecDeque<u32> = VecDeque::new();
    let mut hits = 0u64;
    let start = Instant::now();
    for k in 0..LOOKUPS {
        let id = xorshift(k as u64) % n;
        let page_id = (id as usize / RECS_PER_PAGE) as u32;
        if cache.get(&page_id).is_none() {
            if cache.len() >= cache_pages {
                if let Some(old) = lru.pop_front() {
                    cache.remove(&old);
                }
            }
            let mut page = Box::new([0u8; PAGE]);
            let off = page_id as u64 * PAGE as u64;
            file.seek(SeekFrom::Start(off)).expect("seek");
            file.read_exact(&mut page[..]).expect("read page");
            cache.insert(page_id, page);
            lru.push_back(page_id);
        }
        let page = cache.get(&page_id).expect("cached");
        let slot = id as usize % RECS_PER_PAGE;
        let rec = &page[HDR + slot * REC..HDR + (slot + 1) * REC];
        let got = u64::from_le_bytes(rec[0..8].try_into().unwrap());
        if got == id {
            hits += 1;
        }
    }
    let ns = start.elapsed().as_nanos() / LOOKUPS as u128;
    let cache_bytes = cache.len() as u64 * PAGE as u64;
    let _ = fs::remove_file(&path);
    report("paged", n, file_bytes, cache_bytes, hits, ns);
}

fn lookup_hashmap(map: &HashMap<u64, [u8; REC]>, n: u64) -> (u64, u128) {
    let mut hits = 0u64;
    let start = Instant::now();
    for k in 0..LOOKUPS {
        let id = xorshift(k as u64) % n;
        if let Some(rec) = map.get(&id) {
            let got = u64::from_le_bytes(rec[0..8].try_into().unwrap());
            if got == id {
                hits += 1;
            }
        }
    }
    (hits, start.elapsed().as_nanos() / LOOKUPS as u128)
}

fn write_pages(path: &Path, n: u64) {
    let pages = (n as usize + RECS_PER_PAGE - 1) / RECS_PER_PAGE;
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .expect("create pages");
    let mut page = [0u8; PAGE];
    let mut written = 0u64;
    for _ in 0..pages {
        page.fill(0);
        let remaining = (n - written) as usize;
        let count = remaining.min(RECS_PER_PAGE);
        page[0..2].copy_from_slice(&(count as u16).to_le_bytes());
        for i in 0..count {
            let rec = record(written + i as u64);
            let start = HDR + i * REC;
            page[start..start + REC].copy_from_slice(&rec);
        }
        file.write_all(&page).expect("write page");
        written += count as u64;
    }
    file.sync_all().expect("sync pages");
}

fn record(id: u64) -> [u8; REC] {
    let mut rec = [0u8; REC];
    rec[0..8].copy_from_slice(&id.to_le_bytes());
    rec[8..16].copy_from_slice(&(id.wrapping_mul(0x9e37_79b9_7f4a_7c15)).to_le_bytes());
    rec
}

fn temp_path(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "mikura-014-{}-{}-{tag}.bin",
        std::process::id(),
        nonce
    ))
}

fn xorshift(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

fn rss_bytes() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kib = |prefix: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .and_then(|rest| {
                rest.trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .unwrap_or(0)
            * 1024
    };
    (kib("VmRSS:"), kib("VmHWM:"))
}

fn report(mode: &str, n: u64, file_bytes: u64, live_bytes: u64, hits: u64, ns_per_lookup: u128) {
    let (rss, hwm) = rss_bytes();
    println!(
        "mode={mode} n={n} file_bytes={file_bytes} live_bytes={live_bytes} rss_bytes={rss} peak_rss_bytes={hwm} lookups={LOOKUPS} hits={hits} ns_per_lookup={ns_per_lookup}"
    );
}
