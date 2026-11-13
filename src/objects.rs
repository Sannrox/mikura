//! File backend of the swap-committed object protocol (ADR 0047).
//!
//! Objects are immutable files. A compare-and-swap is an exclusive create of
//! `manifest.{expected+1}`. Acknowledgement is that create; a displaced
//! writer sees `already exists` and is fenced.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crc32fast::Hasher as Crc;

use crate::codec::{read_str, take, write_str};
use crate::crypto::{decode_catalog, encode_catalog, WrappedDek};

/// Snapshot object magic (`MKMANI02`).
pub const MANIFEST_FORMAT: &[u8; 8] = b"MKMANI02";
/// Pre-snapshot manifests (`MKMANI01`) this crate still opens.
const MANIFEST_FORMAT_V1: &[u8; 8] = b"MKMANI01";
const CRC_LEN: usize = 4;
const FLAG_ENCRYPTED: u8 = 1;
/// Snapshot names a checkpoint, projection, and prefix object (ADR 0047 §10).
const FLAG_SNAPSHOT: u8 = 2;
static NEXT_TMP: AtomicU64 = AtomicU64::new(1);
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// Storage-call counters for one file backend (ADR 0047 §10).
///
/// Sequential waves are derived: every `put` of a commit is one wave,
/// the conditional manifest write is a second. Lost-response reads are
/// not the happy path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StorageCalls {
    pub puts: u64,
    pub gets: u64,
    pub cas: u64,
    pub current: u64,
    pub collect: u64,
    pub probe: u64,
    pub object_len: u64,
}

impl StorageCalls {
    /// Happy-path sequential waves: puts are one wave (parallel on a remote
    /// backend), then one CAS. Extra puts still count as that first wave.
    pub fn commit_waves(self) -> u64 {
        u64::from(self.puts > 0) + u64::from(self.cas > 0)
    }

    pub fn saturating_sub(self, other: Self) -> Self {
        Self {
            puts: self.puts.saturating_sub(other.puts),
            gets: self.gets.saturating_sub(other.gets),
            cas: self.cas.saturating_sub(other.cas),
            current: self.current.saturating_sub(other.current),
            collect: self.collect.saturating_sub(other.collect),
            probe: self.probe.saturating_sub(other.probe),
            object_len: self.object_len.saturating_sub(other.object_len),
        }
    }

    /// Open discovery plus named-object gets, excluding writer probe/collect.
    pub fn open_named_gets(self) -> u64 {
        self.current.saturating_add(self.gets)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub key: String,
    pub first_page: u32,
    pub page_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub version: u64,
    pub committed_pages: u32,
    pub encrypted: bool,
    pub segments: Vec<Segment>,
    pub catalog: BTreeMap<String, WrappedDek>,
    pub checkpoint_key: String,
    pub projection_key: String,
    pub prefix_key: String,
    pub checkpoint_pages: u32,
    /// Frozen segments named by `prefix_key`. Not encoded in the snapshot.
    pub prefix_segments: Vec<Segment>,
}

impl Manifest {
    pub(crate) fn empty(encrypted: bool) -> Self {
        Self {
            version: 0,
            committed_pages: 0,
            encrypted,
            segments: Vec::new(),
            catalog: BTreeMap::new(),
            checkpoint_key: String::new(),
            projection_key: String::new(),
            prefix_key: String::new(),
            checkpoint_pages: 0,
            prefix_segments: Vec::new(),
        }
    }

    pub(crate) fn live_keys(&self) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        if self.version > 0 {
            keys.insert(format!("manifest.{}", self.version));
        }
        insert_named(&mut keys, &self.checkpoint_key);
        insert_named(&mut keys, &self.projection_key);
        insert_named(&mut keys, &self.prefix_key);
        for segment in self.prefix_segments.iter().chain(&self.segments) {
            keys.insert(segment.key.clone());
        }
        keys
    }

    pub(crate) fn page_span(&self, index: u32) -> Result<(&Segment, u32), String> {
        if index == 0 || index > self.committed_pages {
            return Err(format!("page {index} is not committed"));
        }
        let pool = if self.checkpoint_pages > 0 && index <= self.checkpoint_pages {
            self.prefix_segments.as_slice()
        } else {
            self.segments.as_slice()
        };
        for segment in pool {
            let last = segment.first_page + segment.page_count - 1;
            if index >= segment.first_page && index <= last {
                return Ok((segment, index - segment.first_page));
            }
        }
        Err(format!(
            "committed page {index} is missing from the manifest"
        ))
    }
}

fn insert_named(keys: &mut BTreeSet<String>, key: &str) {
    if !key.is_empty() {
        keys.insert(key.to_string());
    }
}

/// Unique object key so a leftover put never fences the live writer.
pub(crate) fn unique_object_key(kind: &str, stamp: u32) -> String {
    let seq = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{kind}.{stamp}.{nanos:x}-{seq:x}")
}

pub(crate) fn encode_manifest(manifest: &Manifest) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    body.extend_from_slice(MANIFEST_FORMAT);
    body.extend_from_slice(&manifest.version.to_le_bytes());
    body.extend_from_slice(&manifest.committed_pages.to_le_bytes());
    let mut flags = u8::from(manifest.encrypted) * FLAG_ENCRYPTED;
    flags |= FLAG_SNAPSHOT;
    body.push(flags);
    write_segments(&mut body, &manifest.segments)?;
    write_str(&mut body, &manifest.checkpoint_key)?;
    write_str(&mut body, &manifest.projection_key)?;
    write_str(&mut body, &manifest.prefix_key)?;
    body.extend_from_slice(&manifest.checkpoint_pages.to_le_bytes());
    if manifest.encrypted {
        body.extend_from_slice(&encode_catalog(&manifest.catalog)?);
    }
    let mut out = Vec::with_capacity(CRC_LEN + body.len());
    out.extend_from_slice(&crc32(&body).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

fn write_segments(body: &mut Vec<u8>, segments: &[Segment]) -> Result<(), String> {
    let n = u32::try_from(segments.len()).map_err(|_| "too many segments".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    for segment in segments {
        write_str(body, &segment.key)?;
        body.extend_from_slice(&segment.first_page.to_le_bytes());
        body.extend_from_slice(&segment.page_count.to_le_bytes());
    }
    Ok(())
}

fn read_segments(cur: &mut &[u8]) -> Result<Vec<Segment>, String> {
    let n = u32::from_le_bytes(take::<4>(cur)?.try_into().unwrap()) as usize;
    let mut segments = Vec::with_capacity(n);
    for _ in 0..n {
        let key = read_str(cur)?;
        if !valid_key(&key) {
            return Err("bad segment key".into());
        }
        let first_page = u32::from_le_bytes(take::<4>(cur)?.try_into().unwrap());
        let page_count = u32::from_le_bytes(take::<4>(cur)?.try_into().unwrap());
        if page_count == 0 {
            return Err("segment page count is zero".into());
        }
        segments.push(Segment {
            key,
            first_page,
            page_count,
        });
    }
    Ok(segments)
}

fn read_named_key(cur: &mut &[u8]) -> Result<String, String> {
    let key = read_str(cur)?;
    if key.is_empty() {
        return Ok(key);
    }
    if !valid_key(&key) {
        return Err("bad object key".into());
    }
    Ok(key)
}

pub(crate) fn decode_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    if bytes.len() < CRC_LEN + MANIFEST_FORMAT.len() {
        return Err("short manifest".into());
    }
    let expected = u32::from_le_bytes(bytes[..CRC_LEN].try_into().unwrap());
    let body = &bytes[CRC_LEN..];
    if crc32(body) != expected {
        return Err("manifest checksum mismatch".into());
    }
    let mut cur = body;
    let magic = take::<8>(&mut cur)?;
    if magic != MANIFEST_FORMAT && magic != MANIFEST_FORMAT_V1 {
        return Err("bad manifest magic".into());
    }
    let version = u64::from_le_bytes(take::<8>(&mut cur)?.try_into().unwrap());
    let committed_pages = u32::from_le_bytes(take::<4>(&mut cur)?.try_into().unwrap());
    let flags = take::<1>(&mut cur)?[0];
    let encrypted = flags & FLAG_ENCRYPTED != 0;
    let snapshot = magic == MANIFEST_FORMAT || flags & FLAG_SNAPSHOT != 0;
    let segments = read_segments(&mut cur)?;
    let (checkpoint_key, projection_key, prefix_key, checkpoint_pages) = if snapshot {
        (
            read_named_key(&mut cur)?,
            read_named_key(&mut cur)?,
            read_named_key(&mut cur)?,
            u32::from_le_bytes(take::<4>(&mut cur)?.try_into().unwrap()),
        )
    } else {
        (String::new(), String::new(), String::new(), 0)
    };
    let catalog = if encrypted {
        decode_catalog(&mut cur)?
    } else {
        BTreeMap::new()
    };
    Ok(Manifest {
        version,
        committed_pages,
        encrypted,
        segments,
        catalog,
        checkpoint_key,
        projection_key,
        prefix_key,
        checkpoint_pages,
        prefix_segments: Vec::new(),
    })
}

pub(crate) struct FileStore {
    root: PathBuf,
    durable: bool,
    memory: Option<Mutex<BTreeMap<String, Vec<u8>>>>,
    puts: AtomicU64,
    gets: AtomicU64,
    cas: AtomicU64,
    current: AtomicU64,
    collect: AtomicU64,
    probe: AtomicU64,
    lens: AtomicU64,
}

impl FileStore {
    pub(crate) fn create(root: &Path, durable: bool) -> Result<Self, String> {
        if root.exists() {
            std::fs::remove_dir_all(root).map_err(|e| e.to_string())?;
        }
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        if durable {
            crate::durable::sync_parent(root)?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            durable,
            memory: None,
            puts: AtomicU64::new(0),
            gets: AtomicU64::new(0),
            cas: AtomicU64::new(0),
            current: AtomicU64::new(0),
            collect: AtomicU64::new(0),
            probe: AtomicU64::new(0),
            lens: AtomicU64::new(0),
        })
    }

    pub(crate) fn open(root: &Path, durable: bool) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("missing object namespace {}", root.display()));
        }
        Ok(Self {
            root: root.to_path_buf(),
            durable,
            memory: None,
            puts: AtomicU64::new(0),
            gets: AtomicU64::new(0),
            cas: AtomicU64::new(0),
            current: AtomicU64::new(0),
            collect: AtomicU64::new(0),
            probe: AtomicU64::new(0),
            lens: AtomicU64::new(0),
        })
    }

    /// In-memory object map for call-budget tests that would exhaust inodes.
    #[cfg(test)]
    pub(crate) fn memory() -> Self {
        Self {
            root: PathBuf::new(),
            durable: false,
            memory: Some(Mutex::new(BTreeMap::new())),
            puts: AtomicU64::new(0),
            gets: AtomicU64::new(0),
            cas: AtomicU64::new(0),
            current: AtomicU64::new(0),
            collect: AtomicU64::new(0),
            probe: AtomicU64::new(0),
            lens: AtomicU64::new(0),
        }
    }

    pub(crate) fn storage_calls(&self) -> StorageCalls {
        StorageCalls {
            puts: self.puts.load(Ordering::Relaxed),
            gets: self.gets.load(Ordering::Relaxed),
            cas: self.cas.load(Ordering::Relaxed),
            current: self.current.load(Ordering::Relaxed),
            collect: self.collect.load(Ordering::Relaxed),
            probe: self.probe.load(Ordering::Relaxed),
            object_len: self.lens.load(Ordering::Relaxed),
        }
    }

    /// Two exclusive creates of one probe object: exactly one must succeed.
    pub(crate) fn probe(&self) -> Result<u64, String> {
        self.probe.fetch_add(1, Ordering::Relaxed);
        let key = "probe";
        self.put(key, b"a")?;
        match self.put(key, b"b") {
            Err(error) if error.contains("already exists") => {}
            Ok(_) => {
                let _ = self.remove(key);
                return Err("conditional write probe failed: both competing puts succeeded".into());
            }
            Err(error) => {
                let _ = self.remove(key);
                return Err(error);
            }
        }
        self.remove(key)?;
        Ok(if self.durable { 4 } else { 0 })
    }

    pub(crate) fn put(&self, key: &str, bytes: &[u8]) -> Result<u64, String> {
        self.puts.fetch_add(1, Ordering::Relaxed);
        self.put_exclusive(key, bytes)
    }

    fn put_exclusive(&self, key: &str, bytes: &[u8]) -> Result<u64, String> {
        if let Some(memory) = &self.memory {
            if !valid_key(key) {
                return Err(format!("bad object key {key}"));
            }
            let mut map = memory.lock().map_err(|e| e.to_string())?;
            if map.contains_key(key) {
                return Err(format!("put {key}: already exists"));
            }
            map.insert(key.to_string(), bytes.to_vec());
            return Ok(0);
        }
        let dest = self.object_path(key)?;
        let tmp = self.tmp_path(key);
        let mut file = crate::fault::create_new(&tmp)?;
        let write = (|| -> Result<u64, String> {
            crate::fault::write_all(&mut file, bytes)?;
            let mut fsyncs = 0;
            if self.durable {
                crate::fault::sync_data(&file)?;
                fsyncs += 1;
            }
            drop(file);
            match crate::fault::run(crate::fault::Op::Rename, || {
                std::fs::hard_link(&tmp, &dest).map_err(|e| e.to_string())
            }) {
                Ok(()) => {
                    if self.durable {
                        crate::durable::sync_parent(&dest)?;
                        fsyncs += 1;
                    }
                    Ok(fsyncs)
                }
                Err(error) if already_exists(&error) => Err(format!("put {key}: already exists")),
                Err(error) => Err(format!("put {key}: {error}")),
            }
        })();
        let _ = std::fs::remove_file(&tmp);
        write
    }

    pub(crate) fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        self.gets.fetch_add(1, Ordering::Relaxed);
        if let Some(memory) = &self.memory {
            let map = memory.lock().map_err(|e| e.to_string())?;
            return map
                .get(key)
                .cloned()
                .ok_or_else(|| format!("get {key}: not found"));
        }
        let path = self.object_path(key)?;
        let mut file = crate::fault::open_read(&path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| format!("get {key}: {e}"))?;
        Ok(bytes)
    }

    pub(crate) fn object_len(&self, key: &str) -> Result<u64, String> {
        self.lens.fetch_add(1, Ordering::Relaxed);
        if let Some(memory) = &self.memory {
            let map = memory.lock().map_err(|e| e.to_string())?;
            return map
                .get(key)
                .map(|bytes| bytes.len() as u64)
                .ok_or_else(|| "committed pages missing".to_string());
        }
        let path = self.object_path(key)?;
        std::fs::metadata(&path)
            .map(|meta| meta.len())
            .map_err(|_| "committed pages missing".to_string())
    }

    pub(crate) fn cas(&self, expected: u64, bytes: &[u8]) -> Result<(u64, u64), String> {
        self.cas.fetch_add(1, Ordering::Relaxed);
        let next = expected + 1;
        let key = format!("manifest.{next}");
        match self.put_exclusive(&key, bytes) {
            Ok(fsyncs) => Ok((next, fsyncs)),
            Err(error) if error.contains("already exists") => {
                Err(format!("{}: manifest {next} exists", crate::LOG_FENCED))
            }
            Err(error) => self.resolve_cas(expected, next, bytes, error),
        }
    }

    /// A lost swap response is resolved by reading the successor manifest.
    fn resolve_cas(
        &self,
        expected: u64,
        next: u64,
        bytes: &[u8],
        error: String,
    ) -> Result<(u64, u64), String> {
        match self.get(&format!("manifest.{next}")) {
            Ok(landed) if landed == bytes => Ok((next, 0)),
            Ok(_) => Err(format!("{}: manifest {next} exists", crate::LOG_FENCED)),
            Err(_) => match self.current()? {
                Some((version, _)) if version > expected => {
                    Err(format!("{}: manifest {next} exists", crate::LOG_FENCED))
                }
                _ => Err(error),
            },
        }
    }

    pub(crate) fn current(&self) -> Result<Option<(u64, Vec<u8>)>, String> {
        self.current.fetch_add(1, Ordering::Relaxed);
        if let Some(memory) = &self.memory {
            let map = memory.lock().map_err(|e| e.to_string())?;
            let mut highest = None;
            for name in map.keys() {
                if let Some(rest) = name.strip_prefix("manifest.") {
                    if let Ok(version) = rest.parse::<u64>() {
                        if highest.is_none_or(|v| version > v) {
                            highest = Some(version);
                        }
                    }
                }
            }
            return match highest {
                Some(version) => {
                    let key = format!("manifest.{version}");
                    let bytes = map
                        .get(&key)
                        .cloned()
                        .ok_or_else(|| format!("get {key}: not found"))?;
                    Ok(Some((version, bytes)))
                }
                None => Ok(None),
            };
        }
        let mut highest = None;
        let entries = std::fs::read_dir(&self.root).map_err(|e| e.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(rest) = name.strip_prefix("manifest.") {
                if let Ok(version) = rest.parse::<u64>() {
                    if highest.is_none_or(|v| version > v) {
                        highest = Some(version);
                    }
                }
            }
        }
        match highest {
            Some(version) => Ok(Some((version, self.get(&format!("manifest.{version}"))?))),
            None => Ok(None),
        }
    }

    pub(crate) fn has_durable_objects(&self) -> Result<bool, String> {
        if let Some(memory) = &self.memory {
            let map = memory.lock().map_err(|e| e.to_string())?;
            return Ok(map.keys().any(|name| !name.starts_with('.')));
        }
        let entries = std::fs::read_dir(&self.root).map_err(|e| e.to_string())?;
        for entry in entries {
            let name = entry.map_err(|e| e.to_string())?.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with('.') {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn collect(&self, live: &BTreeSet<String>) -> Result<(), String> {
        self.collect.fetch_add(1, Ordering::Relaxed);
        if let Some(memory) = &self.memory {
            let mut map = memory.lock().map_err(|e| e.to_string())?;
            map.retain(|name, _| live.contains(name));
            return Ok(());
        }
        let entries = std::fs::read_dir(&self.root).map_err(|e| e.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if live.contains(name) {
                continue;
            }
            let _ = std::fs::remove_file(entry.path());
        }
        if self.durable {
            crate::durable::sync_parent(&self.root.join("."))?;
        }
        Ok(())
    }

    fn remove(&self, key: &str) -> Result<(), String> {
        if let Some(memory) = &self.memory {
            let mut map = memory.lock().map_err(|e| e.to_string())?;
            map.remove(key);
            return Ok(());
        }
        let path = self.object_path(key)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn object_path(&self, key: &str) -> Result<PathBuf, String> {
        if !valid_key(key) {
            return Err(format!("bad object key {key}"));
        }
        Ok(self.root.join(key))
    }

    fn tmp_path(&self, key: &str) -> PathBuf {
        self.root.join(format!(
            ".tmp-{}-{}-{key}",
            std::process::id(),
            NEXT_TMP.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.contains('/')
        && !key.contains('\\')
        && !key.contains('\0')
        && key != "."
        && key != ".."
        && !key.starts_with('.')
}

fn already_exists(error: &str) -> bool {
    error.contains("exists") || error.contains("EEXIST")
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = Crc::new();
    hasher.update(bytes);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mikura-obj-{label}-{}-{}",
            std::process::id(),
            NEXT_TMP.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn exclusive_put_is_the_fence() {
        let dir = scratch("put");
        let store = FileStore::create(&dir, false).unwrap();
        store.put("seg.1", b"one").unwrap();
        let err = store.put("seg.1", b"two").unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(store.get("seg.1").unwrap(), b"one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cas_refuses_a_displaced_writer() {
        let dir = scratch("cas");
        let store = FileStore::create(&dir, false).unwrap();
        let body = encode_manifest(&Manifest::empty(false)).unwrap();
        let (v1, _) = store.cas(0, &body).unwrap();
        assert_eq!(v1, 1);
        let err = store.cas(0, &body).unwrap_err();
        assert!(err.contains(crate::LOG_FENCED), "{err}");
        let (v2, _) = store.cas(1, &body).unwrap();
        assert_eq!(v2, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cas_is_its_own_wave() {
        let store = FileStore::memory();
        let body = encode_manifest(&Manifest::empty(false)).unwrap();
        let before = store.storage_calls();
        store.cas(0, &body).unwrap();
        let delta = store.storage_calls().saturating_sub(before);
        assert_eq!(delta.puts, 0);
        assert_eq!(delta.cas, 1);
        assert_eq!(delta.commit_waves(), 1);
    }

    #[test]
    fn probe_requires_exclusive_create() {
        let dir = scratch("probe");
        let store = FileStore::create(&dir, false).unwrap();
        store.probe().unwrap();
        assert!(store.current().unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn collect_drops_unreferenced_objects() {
        let dir = scratch("gc");
        let store = FileStore::create(&dir, false).unwrap();
        store.put("seg.1", b"keep").unwrap();
        store.put("seg.2", b"drop").unwrap();
        let mut live = BTreeSet::new();
        live.insert("seg.1".into());
        store.collect(&live).unwrap();
        assert_eq!(store.get("seg.1").unwrap(), b"keep");
        assert!(store.get("seg.2").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_round_trip_keeps_segments_and_flag() {
        let mut manifest = Manifest::empty(true);
        manifest.version = 3;
        manifest.committed_pages = 4;
        manifest.segments.push(Segment {
            key: "seg.1".into(),
            first_page: 1,
            page_count: 4,
        });
        manifest.checkpoint_key = "ckpt.2".into();
        manifest.prefix_key = "prefix.2".into();
        manifest.checkpoint_pages = 2;
        let bytes = encode_manifest(&manifest).unwrap();
        assert_eq!(&bytes[CRC_LEN..CRC_LEN + 8], MANIFEST_FORMAT);
        let got = decode_manifest(&bytes).unwrap();
        assert_eq!(got.segments, manifest.segments);
        assert_eq!(got.checkpoint_key, "ckpt.2");
        assert_eq!(got.prefix_key, "prefix.2");
        assert_eq!(got.checkpoint_pages, 2);
        assert!(got.encrypted);
    }

    #[test]
    fn decode_accepts_pre_snapshot_manifests() {
        let mut body = Vec::new();
        body.extend_from_slice(MANIFEST_FORMAT_V1);
        body.extend_from_slice(&1u64.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        body.push(0);
        body.extend_from_slice(&1u32.to_le_bytes());
        write_str(&mut body, "seg.1").unwrap();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&crc32(&body).to_le_bytes());
        bytes.extend_from_slice(&body);
        let got = decode_manifest(&bytes).unwrap();
        assert_eq!(got.segments.len(), 1);
        assert!(got.checkpoint_key.is_empty());
        assert_eq!(got.checkpoint_pages, 0);
    }
}
