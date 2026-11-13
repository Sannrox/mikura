//! Paged join projection (`MKJOIN07`) plus a bounded page cache (ADR 0048).
//!
//! Unbudgeted stores still write `MKJOIN06`. A serving budget persists and
//! serves this page file so decoded projection state stays near the cache.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::joins::{crc32, JoinMaps, LiveMeta, ACTION_NONE};

/// Caller-set cap on decoded projection pages (ADR 0048). Zero is invalid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServingBudget {
    bytes: usize,
}

impl ServingBudget {
    /// Fail closed when `bytes` is zero.
    pub fn from_bytes(bytes: usize) -> Result<Self, String> {
        if bytes == 0 {
            return Err("serving budget must be greater than zero".into());
        }
        Ok(Self { bytes })
    }

    pub fn bytes(self) -> usize {
        self.bytes
    }
}

pub(crate) const JOIN_PAGED_MAGIC: &[u8; 8] = crate::JOIN_FORMAT_PAGED;
pub(crate) const PAGE: usize = 4096;
const CRC: usize = 4;
const BODY: usize = PAGE - CRC;
const LOC_ENTRY: usize = 6;
const LOCS_PER_PAGE: usize = BODY / LOC_ENTRY;
const HASH_SLOT: usize = 16;
const HASH_SLOTS: usize = BODY / HASH_SLOT;
const ID_SLOT: usize = 8;
const ID_SLOTS: usize = BODY / ID_SLOT;
const U32_PER_PAGE: usize = BODY / 4;
const PARENT_ENTRY: usize = 12;
const PARENTS_PER_PAGE: usize = BODY / PARENT_ENTRY;
const LIST_DIR_ENTRY: usize = 16;
const LIST_DIRS_PER_PAGE: usize = BODY / LIST_DIR_ENTRY;

type LiveId = (u32, u32, u32);
type OwnedPairs = Vec<(u32, u32)>;
type IdentityRow = (LiveId, LiveMeta, OwnedPairs);
type PropDir = Vec<((u32, u32), DirRef)>;
type RollupDir = Vec<((u32, u32, u32), DirRef)>;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ListRef {
    page: u32,
    pages: u32,
    count: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DirRef {
    page: u32,
    pages: u32,
    count: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct NsDir {
    pub intern_len: u32,
    intern_loc_page: u32,
    intern_hash_page: u32,
    intern_hash_pages: u32,
    pub(crate) ns_id: u32,
    by_kind: Vec<(u32, ListRef)>,
    by_prop: Vec<((u32, u32), DirRef)>,
    amounts: Vec<((u32, u32), DirRef)>,
    pub(crate) rollups: Vec<((u32, u32, u32), DirRef)>,
    pub(crate) declared: Vec<(u32, u32)>,
}

#[derive(Debug)]
struct Superblock {
    log_pages: u32,
    page_count: u32,
    identity_hash_page: u32,
    identity_hash_pages: u32,
    ns_blob_page: u32,
    ns_blob_len: u32,
}

pub(crate) struct PagedFile {
    path: PathBuf,
    file: Mutex<File>,
    cache: Mutex<PageCache>,
    superblock: Superblock,
    namespaces: HashMap<String, NsDir>,
    ns_names: Vec<String>,
    fault: Mutex<Option<String>>,
}

impl std::fmt::Debug for PagedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PagedFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

struct PageCache {
    cap: usize,
    map: HashMap<u32, Box<[u8; PAGE]>>,
    lru: VecDeque<u32>,
}

struct PageWriter {
    pages: Vec<[u8; PAGE]>,
    pack_page: u32,
    pack_used: usize,
}

impl PageWriter {
    fn new() -> Self {
        Self {
            pages: vec![[0u8; PAGE]],
            pack_page: 0,
            pack_used: BODY,
        }
    }

    fn alloc(&mut self) -> u32 {
        let id = self.pages.len() as u32;
        self.pages.push([0u8; PAGE]);
        id
    }

    fn alloc_n(&mut self, n: u32) -> u32 {
        let start = self.pages.len() as u32;
        for _ in 0..n {
            self.pages.push([0u8; PAGE]);
        }
        start
    }

    fn page_mut(&mut self, id: u32) -> &mut [u8; PAGE] {
        &mut self.pages[id as usize]
    }

    fn pack(&mut self, bytes: &[u8]) -> Result<(u32, u16), String> {
        if bytes.len() > BODY {
            return Err("paged record exceeds one page".into());
        }
        if self.pack_page == 0 || self.pack_used + bytes.len() > BODY {
            self.pack_page = self.alloc();
            self.pack_used = 0;
        }
        let off = u16::try_from(self.pack_used).map_err(|_| "page offset".to_string())?;
        let page = self.pack_page;
        let start = self.pack_used;
        let end = start + bytes.len();
        self.page_mut(page)[start..end].copy_from_slice(bytes);
        self.pack_used = end;
        Ok((page, off))
    }

    fn finish(mut self, superblock: &Superblock, ns_blob: &[u8]) -> Result<Vec<u8>, String> {
        let ns_page = superblock.ns_blob_page;
        if ns_blob.len() > BODY {
            return Err("namespace directory does not fit one page".into());
        }
        while (self.pages.len() as u32) <= ns_page {
            self.alloc();
        }
        self.page_mut(ns_page)[..ns_blob.len()].copy_from_slice(ns_blob);
        let page_count = self.pages.len() as u32;
        write_superblock(self.page_mut(0), superblock, page_count);
        for page in &mut self.pages {
            let crc = crc32(&page[..BODY]);
            page[BODY..].copy_from_slice(&crc.to_le_bytes());
        }
        let mut out = Vec::with_capacity(self.pages.len() * PAGE);
        for page in &self.pages {
            out.extend_from_slice(page);
        }
        Ok(out)
    }
}

fn write_superblock(page: &mut [u8; PAGE], sb: &Superblock, page_count: u32) {
    page[..8].copy_from_slice(JOIN_PAGED_MAGIC);
    page[8..12].copy_from_slice(&sb.log_pages.to_le_bytes());
    page[12..16].copy_from_slice(&page_count.to_le_bytes());
    page[16..20].copy_from_slice(&sb.identity_hash_page.to_le_bytes());
    page[20..24].copy_from_slice(&sb.identity_hash_pages.to_le_bytes());
    page[24..28].copy_from_slice(&sb.ns_blob_page.to_le_bytes());
    page[28..32].copy_from_slice(&sb.ns_blob_len.to_le_bytes());
}

fn parse_superblock(page: &[u8; PAGE]) -> Result<Superblock, String> {
    check_page(page)?;
    if &page[..8] != JOIN_PAGED_MAGIC {
        return Err("bad join magic".into());
    }
    Ok(Superblock {
        log_pages: u32::from_le_bytes(page[8..12].try_into().unwrap()),
        page_count: u32::from_le_bytes(page[12..16].try_into().unwrap()),
        identity_hash_page: u32::from_le_bytes(page[16..20].try_into().unwrap()),
        identity_hash_pages: u32::from_le_bytes(page[20..24].try_into().unwrap()),
        ns_blob_page: u32::from_le_bytes(page[24..28].try_into().unwrap()),
        ns_blob_len: u32::from_le_bytes(page[28..32].try_into().unwrap()),
    })
}

fn check_page(page: &[u8; PAGE]) -> Result<(), String> {
    let expected = u32::from_le_bytes(page[BODY..].try_into().unwrap());
    if crc32(&page[..BODY]) != expected {
        Err("join checksum mismatch".into())
    } else {
        Ok(())
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn fnv_u32s(parts: &[u32]) -> u64 {
    let mut bytes = [0u8; 16];
    let mut n = 0;
    for part in parts {
        bytes[n..n + 4].copy_from_slice(&part.to_le_bytes());
        n += 4;
    }
    fnv(&bytes[..n])
}

fn hash_pages_for(count: usize, slots: usize) -> u32 {
    let need = count.saturating_mul(2).max(1);
    need.div_ceil(slots).max(1) as u32
}

fn put_id_hash(
    pages: &mut PageWriter,
    start: u32,
    n_pages: u32,
    hash: u32,
    id: u32,
) -> Result<(), String> {
    let slots = n_pages as usize * ID_SLOTS;
    let mut slot = (hash as usize) % slots.max(1);
    for _ in 0..slots {
        let page = start + (slot / ID_SLOTS) as u32;
        let off = (slot % ID_SLOTS) * ID_SLOT;
        let body = &mut pages.page_mut(page)[off..off + ID_SLOT];
        let stored_id = u32::from_le_bytes(body[4..8].try_into().unwrap());
        if stored_id == u32::MAX {
            body[..4].copy_from_slice(&hash.to_le_bytes());
            body[4..8].copy_from_slice(&id.to_le_bytes());
            return Ok(());
        }
        slot = (slot + 1) % slots;
    }
    Err("intern hash table is full".into())
}

fn init_id_hash(pages: &mut PageWriter, start: u32, n_pages: u32) {
    for i in 0..n_pages {
        let page = pages.page_mut(start + i);
        for s in 0..ID_SLOTS {
            let off = s * ID_SLOT;
            page[off + 4..off + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        }
    }
}

fn put_ptr_hash(
    pages: &mut PageWriter,
    start: u32,
    n_pages: u32,
    hash: u64,
    rec_page: u32,
    rec_off: u16,
) -> Result<(), String> {
    let slots = n_pages as usize * HASH_SLOTS;
    let mut slot = (hash as usize) % slots.max(1);
    for _ in 0..slots {
        let page = start + (slot / HASH_SLOTS) as u32;
        let off = (slot % HASH_SLOTS) * HASH_SLOT;
        let body = &mut pages.page_mut(page)[off..off + HASH_SLOT];
        let stored_page = u32::from_le_bytes(body[8..12].try_into().unwrap());
        if stored_page == 0 {
            body[..8].copy_from_slice(&hash.to_le_bytes());
            body[8..12].copy_from_slice(&rec_page.to_le_bytes());
            body[12..14].copy_from_slice(&rec_off.to_le_bytes());
            return Ok(());
        }
        slot = (slot + 1) % slots;
    }
    Err("identity hash table is full".into())
}

fn write_u32_list(pages: &mut PageWriter, ids: &[u32]) -> ListRef {
    let count = ids.len() as u32;
    let n_pages = (ids.len().max(1)).div_ceil(U32_PER_PAGE) as u32;
    let start = pages.alloc_n(n_pages);
    for (i, id) in ids.iter().enumerate() {
        let page = start + (i / U32_PER_PAGE) as u32;
        let off = (i % U32_PER_PAGE) * 4;
        pages.page_mut(page)[off..off + 4].copy_from_slice(&id.to_le_bytes());
    }
    ListRef {
        page: start,
        pages: n_pages,
        count,
    }
}

fn write_parent_dir(
    pages: &mut PageWriter,
    mut rows: Vec<(u32, u32, u16, u32)>,
) -> Result<DirRef, String> {
    rows.sort_unstable_by_key(|row| row.0);
    let count = rows.len() as u32;
    let n_pages = (rows.len().max(1)).div_ceil(PARENTS_PER_PAGE) as u32;
    let start = pages.alloc_n(n_pages);
    for (i, (parent, list_page, list_off, n)) in rows.into_iter().enumerate() {
        let packed_n = u16::try_from(n)
            .map_err(|_| "parent list exceeds packed directory bound".to_string())?;
        let page = start + (i / PARENTS_PER_PAGE) as u32;
        let off = (i % PARENTS_PER_PAGE) * PARENT_ENTRY;
        let buf = pages.page_mut(page);
        buf[off..off + 4].copy_from_slice(&parent.to_le_bytes());
        buf[off + 4..off + 8].copy_from_slice(&list_page.to_le_bytes());
        buf[off + 8..off + 10].copy_from_slice(&list_off.to_le_bytes());
        buf[off + 10..off + 12].copy_from_slice(&packed_n.to_le_bytes());
    }
    Ok(DirRef {
        page: start,
        pages: n_pages,
        count,
    })
}

fn write_list_dir(pages: &mut PageWriter, mut rows: Vec<(u32, ListRef)>) -> DirRef {
    rows.sort_unstable_by_key(|row| row.0);
    let count = rows.len() as u32;
    let n_pages = (rows.len().max(1)).div_ceil(LIST_DIRS_PER_PAGE) as u32;
    let start = pages.alloc_n(n_pages);
    for (i, (parent, list)) in rows.into_iter().enumerate() {
        let page = start + (i / LIST_DIRS_PER_PAGE) as u32;
        let off = (i % LIST_DIRS_PER_PAGE) * LIST_DIR_ENTRY;
        let buf = pages.page_mut(page);
        buf[off..off + 4].copy_from_slice(&parent.to_le_bytes());
        buf[off + 4..off + 8].copy_from_slice(&list.page.to_le_bytes());
        buf[off + 8..off + 12].copy_from_slice(&list.pages.to_le_bytes());
        buf[off + 12..off + 16].copy_from_slice(&list.count.to_le_bytes());
    }
    DirRef {
        page: start,
        pages: n_pages,
        count,
    }
}

fn encode_identity_row(
    ns: u32,
    kind: u32,
    key: u32,
    meta: &LiveMeta,
    owned: &[(u32, u32)],
) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(32 + owned.len() * 8);
    buf.extend_from_slice(&ns.to_le_bytes());
    buf.extend_from_slice(&kind.to_le_bytes());
    buf.extend_from_slice(&key.to_le_bytes());
    buf.extend_from_slice(&meta.gen.to_le_bytes());
    buf.push(u8::from(meta.hidden));
    buf.extend_from_slice(&meta.action_id.unwrap_or(ACTION_NONE).to_le_bytes());
    buf.extend_from_slice(&meta.schema_version.to_le_bytes());
    let n = u32::try_from(owned.len()).map_err(|_| "too many properties".to_string())?;
    buf.extend_from_slice(&n.to_le_bytes());
    for (prop, value) in owned {
        buf.extend_from_slice(&prop.to_le_bytes());
        buf.extend_from_slice(&value.to_le_bytes());
    }
    if buf.len() > BODY {
        return Err("identity row exceeds one page".into());
    }
    Ok(buf)
}

fn parse_identity_row(bytes: &[u8]) -> Result<IdentityRow, String> {
    if bytes.len() < 33 {
        return Err("short identity row".into());
    }
    let ns = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let kind = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let key = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let gen = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
    let hidden = bytes[20] != 0;
    let action = u32::from_le_bytes(bytes[21..25].try_into().unwrap());
    let schema_version = u32::from_le_bytes(bytes[25..29].try_into().unwrap());
    let n = u32::from_le_bytes(bytes[29..33].try_into().unwrap()) as usize;
    let mut cur = 33;
    let mut owned = Vec::with_capacity(n);
    for _ in 0..n {
        if cur + 8 > bytes.len() {
            return Err("short identity pairs".into());
        }
        let prop = u32::from_le_bytes(bytes[cur..cur + 4].try_into().unwrap());
        let value = u32::from_le_bytes(bytes[cur + 4..cur + 8].try_into().unwrap());
        owned.push((prop, value));
        cur += 8;
    }
    Ok((
        (ns, kind, key),
        LiveMeta {
            gen,
            hidden,
            action_id: if action == ACTION_NONE {
                None
            } else {
                Some(action)
            },
            schema_version,
        },
        owned,
    ))
}

fn encode_ns_blob(entries: &[(String, NsDir)]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let n = u32::try_from(entries.len()).map_err(|_| "too many namespaces".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    for (name, dir) in entries {
        let len = u16::try_from(name.len()).map_err(|_| "namespace too long".to_string())?;
        body.extend_from_slice(&len.to_le_bytes());
        body.extend_from_slice(name.as_bytes());
        body.extend_from_slice(&dir.ns_id.to_le_bytes());
        body.extend_from_slice(&dir.intern_len.to_le_bytes());
        body.extend_from_slice(&dir.intern_loc_page.to_le_bytes());
        body.extend_from_slice(&dir.intern_hash_page.to_le_bytes());
        body.extend_from_slice(&dir.intern_hash_pages.to_le_bytes());
        write_kind_list(&mut body, &dir.by_kind)?;
        write_prop_dirs(&mut body, &dir.by_prop)?;
        write_prop_dirs(&mut body, &dir.amounts)?;
        write_rollup_dirs(&mut body, &dir.rollups)?;
        let dn =
            u32::try_from(dir.declared.len()).map_err(|_| "too many declared sums".to_string())?;
        body.extend_from_slice(&dn.to_le_bytes());
        for (leaf, sum) in &dir.declared {
            body.extend_from_slice(&leaf.to_le_bytes());
            body.extend_from_slice(&sum.to_le_bytes());
        }
    }
    Ok(body)
}

fn write_kind_list(body: &mut Vec<u8>, kinds: &[(u32, ListRef)]) -> Result<(), String> {
    let n = u32::try_from(kinds.len()).map_err(|_| "too many kinds".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    for (kind, list) in kinds {
        body.extend_from_slice(&kind.to_le_bytes());
        body.extend_from_slice(&list.page.to_le_bytes());
        body.extend_from_slice(&list.pages.to_le_bytes());
        body.extend_from_slice(&list.count.to_le_bytes());
    }
    Ok(())
}

fn write_prop_dirs(body: &mut Vec<u8>, dirs: &[((u32, u32), DirRef)]) -> Result<(), String> {
    let n = u32::try_from(dirs.len()).map_err(|_| "too many property dirs".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    for ((kind, prop), dir) in dirs {
        body.extend_from_slice(&kind.to_le_bytes());
        body.extend_from_slice(&prop.to_le_bytes());
        body.extend_from_slice(&dir.page.to_le_bytes());
        body.extend_from_slice(&dir.pages.to_le_bytes());
        body.extend_from_slice(&dir.count.to_le_bytes());
    }
    Ok(())
}

fn write_rollup_dirs(body: &mut Vec<u8>, dirs: &[((u32, u32, u32), DirRef)]) -> Result<(), String> {
    let n = u32::try_from(dirs.len()).map_err(|_| "too many rollup dirs".to_string())?;
    body.extend_from_slice(&n.to_le_bytes());
    for ((leaf, join, sum), dir) in dirs {
        body.extend_from_slice(&leaf.to_le_bytes());
        body.extend_from_slice(&join.to_le_bytes());
        body.extend_from_slice(&sum.to_le_bytes());
        body.extend_from_slice(&dir.page.to_le_bytes());
        body.extend_from_slice(&dir.pages.to_le_bytes());
        body.extend_from_slice(&dir.count.to_le_bytes());
    }
    Ok(())
}

fn take_u32(cur: &mut &[u8]) -> Result<u32, String> {
    if cur.len() < 4 {
        return Err("short paged directory".into());
    }
    let v = u32::from_le_bytes(cur[..4].try_into().unwrap());
    *cur = &cur[4..];
    Ok(v)
}

fn take_str(cur: &mut &[u8]) -> Result<String, String> {
    if cur.len() < 2 {
        return Err("short paged name".into());
    }
    let len = u16::from_le_bytes(cur[..2].try_into().unwrap()) as usize;
    *cur = &cur[2..];
    if cur.len() < len {
        return Err("short paged name".into());
    }
    let s = std::str::from_utf8(&cur[..len]).map_err(|_| "not utf8".to_string())?;
    *cur = &cur[len..];
    Ok(s.to_string())
}

fn parse_ns_blob(mut cur: &[u8]) -> Result<HashMap<String, NsDir>, String> {
    let n = take_u32(&mut cur)? as usize;
    let mut out = HashMap::new();
    for _ in 0..n {
        let name = take_str(&mut cur)?;
        let ns_id = take_u32(&mut cur)?;
        let intern_len = take_u32(&mut cur)?;
        let intern_loc_page = take_u32(&mut cur)?;
        let intern_hash_page = take_u32(&mut cur)?;
        let intern_hash_pages = take_u32(&mut cur)?;
        let by_kind = read_kind_list(&mut cur)?;
        let by_prop = read_prop_dirs(&mut cur)?;
        let amounts = read_prop_dirs(&mut cur)?;
        let rollups = read_rollup_dirs(&mut cur)?;
        let dn = take_u32(&mut cur)? as usize;
        let mut declared = Vec::with_capacity(dn);
        for _ in 0..dn {
            declared.push((take_u32(&mut cur)?, take_u32(&mut cur)?));
        }
        out.insert(
            name,
            NsDir {
                intern_len,
                intern_loc_page,
                intern_hash_page,
                intern_hash_pages,
                ns_id,
                by_kind,
                by_prop,
                amounts,
                rollups,
                declared,
            },
        );
    }
    Ok(out)
}

fn read_kind_list(cur: &mut &[u8]) -> Result<Vec<(u32, ListRef)>, String> {
    let n = take_u32(cur)? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let kind = take_u32(cur)?;
        out.push((
            kind,
            ListRef {
                page: take_u32(cur)?,
                pages: take_u32(cur)?,
                count: take_u32(cur)?,
            },
        ));
    }
    Ok(out)
}

fn read_prop_dirs(cur: &mut &[u8]) -> Result<PropDir, String> {
    let n = take_u32(cur)? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let kind = take_u32(cur)?;
        let prop = take_u32(cur)?;
        out.push((
            (kind, prop),
            DirRef {
                page: take_u32(cur)?,
                pages: take_u32(cur)?,
                count: take_u32(cur)?,
            },
        ));
    }
    Ok(out)
}

fn read_rollup_dirs(cur: &mut &[u8]) -> Result<RollupDir, String> {
    let n = take_u32(cur)? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let leaf = take_u32(cur)?;
        let join = take_u32(cur)?;
        let sum = take_u32(cur)?;
        out.push((
            (leaf, join, sum),
            DirRef {
                page: take_u32(cur)?,
                pages: take_u32(cur)?,
                count: take_u32(cur)?,
            },
        ));
    }
    Ok(out)
}

pub(crate) fn is_paged_magic(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[..8] == JOIN_PAGED_MAGIC
}

pub(crate) fn peek_log_pages(path: &Path) -> Result<u32, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut page = [0u8; PAGE];
    file.read_exact(&mut page).map_err(|e| e.to_string())?;
    Ok(parse_superblock(&page)?.log_pages)
}

pub(crate) fn peek_log_pages_bytes(bytes: &[u8]) -> Result<u32, String> {
    if bytes.len() < PAGE {
        return Err("paged projection is empty".into());
    }
    let mut page = [0u8; PAGE];
    page.copy_from_slice(&bytes[..PAGE]);
    Ok(parse_superblock(&page)?.log_pages)
}

pub(crate) fn encode(
    log_pages: u32,
    ns_names: &[String],
    identity: &HashMap<LiveId, LiveMeta>,
    hidden_props: &HashMap<LiveId, Vec<(u32, u32)>>,
    joins: &HashMap<String, JoinMaps>,
) -> Result<Vec<u8>, String> {
    let mut pages = PageWriter::new();
    let mut ns_entries = Vec::new();
    let mut identity_rows = Vec::new();

    for (i, name) in ns_names.iter().enumerate() {
        let ns_id = i as u32;
        let Some(maps) = joins.get(name) else {
            continue;
        };
        let intern_len = maps.intern.len() as u32;
        let loc_pages = (maps.intern.len().max(1)).div_ceil(LOCS_PER_PAGE) as u32;
        let intern_loc_page = pages.alloc_n(loc_pages);
        let intern_hash_pages = hash_pages_for(maps.intern.len(), ID_SLOTS);
        let intern_hash_page = pages.alloc_n(intern_hash_pages);
        init_id_hash(&mut pages, intern_hash_page, intern_hash_pages);

        for (id, value) in maps.intern.iter().enumerate() {
            let mut rec = Vec::with_capacity(2 + value.len());
            let len =
                u16::try_from(value.len()).map_err(|_| "intern string too long".to_string())?;
            rec.extend_from_slice(&len.to_le_bytes());
            rec.extend_from_slice(value.as_bytes());
            let (page, off) = pages.pack(&rec)?;
            let loc_i = intern_loc_page + (id / LOCS_PER_PAGE) as u32;
            let loc_off = (id % LOCS_PER_PAGE) * LOC_ENTRY;
            let loc = pages.page_mut(loc_i);
            loc[loc_off..loc_off + 4].copy_from_slice(&page.to_le_bytes());
            loc[loc_off + 4..loc_off + 6].copy_from_slice(&off.to_le_bytes());
            put_id_hash(
                &mut pages,
                intern_hash_page,
                intern_hash_pages,
                fnv(value.as_bytes()) as u32,
                id as u32,
            )?;
        }

        let mut by_kind = Vec::new();
        for (kind, keys) in maps.kind_entries() {
            let mut ids: Vec<u32> = keys.iter().copied().collect();
            ids.sort_unstable();
            by_kind.push((kind, write_u32_list(&mut pages, &ids)));
        }

        let mut by_prop = Vec::new();
        for ((kind, prop), parents) in maps.prop_entries() {
            let mut rows = Vec::new();
            for (parent, mut children) in parents {
                children.sort_unstable();
                rows.push((parent, write_u32_list(&mut pages, &children)));
            }
            by_prop.push(((kind, prop), write_list_dir(&mut pages, rows)));
        }

        let mut amounts = Vec::new();
        for ((kind, prop), table) in maps.amount_entries() {
            let mut rows = Vec::new();
            for (key, amount) in table {
                let mut rec = Vec::with_capacity(12);
                rec.extend_from_slice(&1u32.to_le_bytes());
                rec.extend_from_slice(&amount.to_le_bytes());
                let (list_page, list_off) = pages.pack(&rec)?;
                rows.push((key, list_page, list_off, 1));
            }
            amounts.push(((kind, prop), write_parent_dir(&mut pages, rows)?));
        }

        let mut rollups = Vec::new();
        for (key, table) in maps.rollup_entries() {
            let mut rows = Vec::new();
            for (parent, (count, total)) in table {
                let mut rec = Vec::with_capacity(16);
                rec.extend_from_slice(&count.to_le_bytes());
                rec.extend_from_slice(&total.to_le_bytes());
                let (list_page, list_off) = pages.pack(&rec)?;
                // Packed directory n is unused; the record holds the u32 count.
                rows.push((parent, list_page, list_off, 1));
            }
            rollups.push((key, write_parent_dir(&mut pages, rows)?));
        }

        ns_entries.push((
            name.clone(),
            NsDir {
                intern_len,
                intern_loc_page,
                intern_hash_page,
                intern_hash_pages,
                ns_id,
                by_kind,
                by_prop,
                amounts,
                rollups,
                declared: maps.declared_entries(),
            },
        ));
    }

    for (&id, meta) in identity {
        let owned = if meta.hidden {
            hidden_props.get(&id).cloned().unwrap_or_default()
        } else {
            let ns = ns_names
                .get(id.0 as usize)
                .map(String::as_str)
                .unwrap_or("");
            joins
                .get(ns)
                .and_then(|maps| maps.owned_pairs(id.1, id.2))
                .unwrap_or_default()
        };
        identity_rows.push((id, meta, owned));
    }

    let identity_hash_pages = hash_pages_for(identity_rows.len(), HASH_SLOTS);
    let identity_hash_page = pages.alloc_n(identity_hash_pages);
    for (id, meta, owned) in &identity_rows {
        let rec = encode_identity_row(id.0, id.1, id.2, meta, owned)?;
        let (rec_page, rec_off) = pages.pack(&rec)?;
        put_ptr_hash(
            &mut pages,
            identity_hash_page,
            identity_hash_pages,
            fnv_u32s(&[id.0, id.1, id.2]),
            rec_page,
            rec_off,
        )?;
    }

    let ns_blob = encode_ns_blob(&ns_entries)?;
    let ns_blob_page = pages.alloc();
    let sb = Superblock {
        log_pages,
        page_count: 0,
        identity_hash_page,
        identity_hash_pages,
        ns_blob_page,
        ns_blob_len: ns_blob.len() as u32,
    };
    pages.finish(&sb, &ns_blob)
}

impl PageCache {
    fn get<'a>(
        &'a mut self,
        file: &mut File,
        page: u32,
        page_count: u32,
    ) -> Result<&'a [u8; PAGE], String> {
        if page >= page_count {
            return Err("paged projection page out of range".into());
        }
        if self.map.contains_key(&page) {
            if let Some(pos) = self.lru.iter().position(|p| *p == page) {
                self.lru.remove(pos);
            }
            self.lru.push_back(page);
            return Ok(self.map.get(&page).expect("cached"));
        }
        if self.map.len() >= self.cap {
            if let Some(old) = self.lru.pop_front() {
                self.map.remove(&old);
            }
        }
        let mut buf = Box::new([0u8; PAGE]);
        file.seek(SeekFrom::Start(u64::from(page) * PAGE as u64))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut buf[..]).map_err(|e| e.to_string())?;
        check_page(&buf)?;
        self.map.insert(page, buf);
        self.lru.push_back(page);
        Ok(self.map.get(&page).expect("inserted"))
    }
}

impl PagedFile {
    pub(crate) fn open(path: &Path, budget: ServingBudget) -> Result<Self, String> {
        let cap = (budget.bytes() / PAGE).max(1);
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let mut page0 = [0u8; PAGE];
        file.read_exact(&mut page0).map_err(|e| e.to_string())?;
        let superblock = parse_superblock(&page0)?;
        if superblock.page_count == 0 {
            return Err("paged projection is empty".into());
        }
        let mut cache = PageCache {
            cap,
            map: HashMap::new(),
            lru: VecDeque::new(),
        };
        cache.map.insert(0, Box::new(page0));
        cache.lru.push_back(0);
        let ns_page = cache.get(&mut file, superblock.ns_blob_page, superblock.page_count)?;
        let len = superblock.ns_blob_len as usize;
        if len > BODY {
            return Err("namespace directory too large".into());
        }
        let namespaces = parse_ns_blob(&ns_page[..len])?;
        let mut ns_names = vec![String::new(); namespaces.len().max(1)];
        for (name, dir) in &namespaces {
            let i = dir.ns_id as usize;
            if ns_names.len() <= i {
                ns_names.resize(i + 1, String::new());
            }
            ns_names[i] = name.clone();
        }
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            cache: Mutex::new(cache),
            superblock,
            namespaces,
            ns_names,
            fault: Mutex::new(None),
        })
    }

    pub(crate) fn fail_closed(&self) -> Result<(), String> {
        match self.fault.lock() {
            Ok(g) => match g.as_ref() {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            },
            Err(_) => Err("paged file lock".into()),
        }
    }

    fn set_fault(&self, err: String) {
        if let Ok(mut g) = self.fault.lock() {
            if g.is_none() {
                *g = Some(err);
            }
        }
    }

    pub(crate) fn log_pages(&self) -> u32 {
        self.superblock.log_pages
    }

    pub(crate) fn ns_names(&self) -> &[String] {
        &self.ns_names
    }

    pub(crate) fn ns_dir(&self, namespace: &str) -> Option<&NsDir> {
        self.namespaces.get(namespace)
    }

    fn page(&self, id: u32) -> Result<[u8; PAGE], String> {
        self.fail_closed()?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| "paged file lock".to_string())?;
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| "paged cache lock".to_string())?;
        match cache.get(&mut file, id, self.superblock.page_count) {
            Ok(page) => Ok(*page),
            Err(err) => {
                self.set_fault(err.clone());
                Err(err)
            }
        }
    }

    pub(crate) fn intern_string(&self, dir: &NsDir, id: u32) -> Option<String> {
        if id >= dir.intern_len {
            return None;
        }
        let loc_page = dir.intern_loc_page + id / LOCS_PER_PAGE as u32;
        let loc_off = (id as usize % LOCS_PER_PAGE) * LOC_ENTRY;
        let loc = self.page(loc_page).ok()?;
        let rec_page = u32::from_le_bytes(loc[loc_off..loc_off + 4].try_into().ok()?);
        let rec_off = u16::from_le_bytes(loc[loc_off + 4..loc_off + 6].try_into().ok()?) as usize;
        let rec = self.page(rec_page).ok()?;
        if rec_off + 2 > BODY {
            return None;
        }
        let len = u16::from_le_bytes(rec[rec_off..rec_off + 2].try_into().ok()?) as usize;
        rec.get(rec_off + 2..rec_off + 2 + len)
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(str::to_string)
    }

    pub(crate) fn intern_id(&self, dir: &NsDir, value: &str) -> Option<u32> {
        let hash = fnv(value.as_bytes()) as u32;
        let slots = dir.intern_hash_pages as usize * ID_SLOTS;
        if slots == 0 {
            return None;
        }
        let mut slot = (hash as usize) % slots;
        for _ in 0..slots {
            let page = dir.intern_hash_page + (slot / ID_SLOTS) as u32;
            let off = (slot % ID_SLOTS) * ID_SLOT;
            let rec = self.page(page).ok()?;
            let stored_hash = u32::from_le_bytes(rec[off..off + 4].try_into().ok()?);
            let stored_id = u32::from_le_bytes(rec[off + 4..off + 8].try_into().ok()?);
            if stored_id == u32::MAX {
                return None;
            }
            if stored_hash == hash && self.intern_string(dir, stored_id).as_deref() == Some(value) {
                return Some(stored_id);
            }
            slot = (slot + 1) % slots;
        }
        None
    }

    fn identity_at(&self, rec_page: u32, rec_off: usize) -> Option<IdentityRow> {
        let page = self.page(rec_page).ok()?;
        parse_identity_row(page.get(rec_off..BODY)?).ok()
    }

    pub(crate) fn identity(&self, id: LiveId) -> Option<(LiveMeta, Vec<(u32, u32)>)> {
        let hash = fnv_u32s(&[id.0, id.1, id.2]);
        let slots = self.superblock.identity_hash_pages as usize * HASH_SLOTS;
        if slots == 0 {
            return None;
        }
        let mut slot = (hash as usize) % slots;
        for _ in 0..slots {
            let page_id = self.superblock.identity_hash_page + (slot / HASH_SLOTS) as u32;
            let off = (slot % HASH_SLOTS) * HASH_SLOT;
            let rec = self.page(page_id).ok()?;
            let stored_hash = u64::from_le_bytes(rec[off..off + 8].try_into().ok()?);
            let rec_page = u32::from_le_bytes(rec[off + 8..off + 12].try_into().ok()?);
            if rec_page == 0 {
                return None;
            }
            if stored_hash == hash {
                let rec_off = u16::from_le_bytes(rec[off + 12..off + 14].try_into().ok()?) as usize;
                if let Some((got, meta, owned)) = self.identity_at(rec_page, rec_off) {
                    if got == id {
                        return Some((meta, owned));
                    }
                }
            }
            slot = (slot + 1) % slots;
        }
        None
    }

    fn list_ids(&self, list: ListRef) -> Vec<u32> {
        let mut out = Vec::with_capacity(list.count as usize);
        for i in 0..list.count as usize {
            let page = list.page + (i / U32_PER_PAGE) as u32;
            let off = (i % U32_PER_PAGE) * 4;
            let Ok(rec) = self.page(page) else {
                break;
            };
            out.push(u32::from_le_bytes(rec[off..off + 4].try_into().unwrap()));
        }
        out
    }

    fn list_contains(&self, list: ListRef, key: u32) -> bool {
        let mut lo = 0usize;
        let mut hi = list.count as usize;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let page = list.page + (mid / U32_PER_PAGE) as u32;
            let off = (mid % U32_PER_PAGE) * 4;
            let Ok(rec) = self.page(page) else {
                return false;
            };
            let got = u32::from_le_bytes(rec[off..off + 4].try_into().unwrap());
            match got.cmp(&key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return true,
            }
        }
        false
    }

    fn list_dir_lookup(&self, dir: DirRef, parent: u32) -> Option<ListRef> {
        let mut lo = 0usize;
        let mut hi = dir.count as usize;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let page = dir.page + (mid / LIST_DIRS_PER_PAGE) as u32;
            let off = (mid % LIST_DIRS_PER_PAGE) * LIST_DIR_ENTRY;
            let rec = self.page(page).ok()?;
            let got = u32::from_le_bytes(rec[off..off + 4].try_into().ok()?);
            match got.cmp(&parent) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    return Some(ListRef {
                        page: u32::from_le_bytes(rec[off + 4..off + 8].try_into().ok()?),
                        pages: u32::from_le_bytes(rec[off + 8..off + 12].try_into().ok()?),
                        count: u32::from_le_bytes(rec[off + 12..off + 16].try_into().ok()?),
                    });
                }
            }
        }
        None
    }

    fn dir_lookup(&self, dir: DirRef, parent: u32) -> Option<(u32, u16, u16)> {
        let mut lo = 0usize;
        let mut hi = dir.count as usize;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let page = dir.page + (mid / PARENTS_PER_PAGE) as u32;
            let off = (mid % PARENTS_PER_PAGE) * PARENT_ENTRY;
            let rec = self.page(page).ok()?;
            let got = u32::from_le_bytes(rec[off..off + 4].try_into().ok()?);
            match got.cmp(&parent) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let list_page = u32::from_le_bytes(rec[off + 4..off + 8].try_into().ok()?);
                    let list_off = u16::from_le_bytes(rec[off + 8..off + 10].try_into().ok()?);
                    let n = u16::from_le_bytes(rec[off + 10..off + 12].try_into().ok()?);
                    return Some((list_page, list_off, n));
                }
            }
        }
        None
    }

    pub(crate) fn kind_contains(&self, dir: &NsDir, kind: u32, key: u32) -> bool {
        dir.by_kind
            .iter()
            .find(|(k, _)| *k == kind)
            .is_some_and(|(_, list)| self.list_contains(*list, key))
    }

    pub(crate) fn key_visible(&self, dir: &NsDir, key: u32) -> bool {
        dir.by_kind
            .iter()
            .any(|(_, list)| self.list_contains(*list, key))
    }

    pub(crate) fn kind_ids(&self, dir: &NsDir, kind: u32) -> HashSet<u32> {
        dir.by_kind
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, list)| self.list_ids(*list).into_iter().collect())
            .unwrap_or_default()
    }

    pub(crate) fn kind_len(&self, dir: &NsDir, kind: u32) -> usize {
        dir.by_kind
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, list)| list.count as usize)
            .unwrap_or(0)
    }

    pub(crate) fn hop_children(&self, dir: &NsDir, kind: u32, prop: u32, parent: u32) -> Vec<u32> {
        let Some((_, dref)) = dir.by_prop.iter().find(|(k, _)| *k == (kind, prop)) else {
            return Vec::new();
        };
        let Some(list) = self.list_dir_lookup(*dref, parent) else {
            return Vec::new();
        };
        self.list_ids(list)
    }

    pub(crate) fn prop_parents(&self, dir: &NsDir, kind: u32, prop: u32) -> Vec<(u32, Vec<u32>)> {
        let Some((_, dref)) = dir.by_prop.iter().find(|(k, _)| *k == (kind, prop)) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(dref.count as usize);
        for i in 0..dref.count as usize {
            let page = dref.page + (i / LIST_DIRS_PER_PAGE) as u32;
            let off = (i % LIST_DIRS_PER_PAGE) * LIST_DIR_ENTRY;
            let Ok(rec) = self.page(page) else {
                break;
            };
            let parent = u32::from_le_bytes(rec[off..off + 4].try_into().unwrap());
            let list = ListRef {
                page: u32::from_le_bytes(rec[off + 4..off + 8].try_into().unwrap()),
                pages: u32::from_le_bytes(rec[off + 8..off + 12].try_into().unwrap()),
                count: u32::from_le_bytes(rec[off + 12..off + 16].try_into().unwrap()),
            };
            out.push((parent, self.list_ids(list)));
        }
        out
    }

    pub(crate) fn amount(&self, dir: &NsDir, kind: u32, prop: u32, key: u32) -> Option<i64> {
        let (_, dref) = dir.amounts.iter().find(|(k, _)| *k == (kind, prop))?;
        let (page, off, _) = self.dir_lookup(*dref, key)?;
        let rec = self.page(page).ok()?;
        let off = off as usize;
        if off + 12 > BODY {
            return None;
        }
        Some(i64::from_le_bytes(rec[off + 4..off + 12].try_into().ok()?))
    }

    pub(crate) fn has_amounts(&self, dir: &NsDir, kind: u32, prop: u32) -> bool {
        dir.amounts
            .iter()
            .any(|(k, d)| *k == (kind, prop) && d.count > 0)
    }

    pub(crate) fn rollup(
        &self,
        dir: &NsDir,
        key: (u32, u32, u32),
        parent: u32,
    ) -> Option<(u32, i64)> {
        let (_, dref) = dir.rollups.iter().find(|(k, _)| *k == key)?;
        let (page, off, n) = self.dir_lookup(*dref, parent)?;
        let rec = self.page(page).ok()?;
        let off = off as usize;
        let _ = n;
        if off + 12 > BODY {
            return None;
        }
        let count = u32::from_le_bytes(rec[off..off + 4].try_into().ok()?);
        let total = i64::from_le_bytes(rec[off + 4..off + 12].try_into().ok()?);
        Some((count, total))
    }

    pub(crate) fn identities(&self) -> Result<Vec<IdentityRow>, String> {
        let mut out = Vec::new();
        let slots = self.superblock.identity_hash_pages as usize * HASH_SLOTS;
        for slot in 0..slots {
            let page_id = self.superblock.identity_hash_page + (slot / HASH_SLOTS) as u32;
            let off = (slot % HASH_SLOTS) * HASH_SLOT;
            let rec = self.page(page_id)?;
            let rec_page = u32::from_le_bytes(rec[off + 8..off + 12].try_into().unwrap());
            if rec_page == 0 {
                continue;
            }
            let rec_off = u16::from_le_bytes(rec[off + 12..off + 14].try_into().unwrap()) as usize;
            match self.identity_at(rec_page, rec_off) {
                Some(row) => out.push(row),
                None => {
                    self.fail_closed()?;
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn has_rollup(&self, dir: &NsDir, key: (u32, u32, u32)) -> bool {
        dir.rollups.iter().any(|(k, _)| *k == key)
    }

    pub(crate) fn prop_keys(&self, dir: &NsDir) -> Vec<(u32, u32)> {
        dir.by_prop.iter().map(|(k, _)| *k).collect()
    }

    pub(crate) fn decode_all(&self) -> Result<crate::joins::Checkpoint, String> {
        let mut loaded = crate::joins::Checkpoint {
            pages: self.superblock.log_pages,
            joins: HashMap::new(),
            ns: self.ns_names.clone(),
            identity: HashMap::new(),
            hidden_props: HashMap::new(),
        };
        for (name, dir) in &self.namespaces {
            let mut maps = JoinMaps::default();
            maps.reserve(dir.intern_len as usize);
            for id in 0..dir.intern_len {
                let value = match self.intern_string(dir, id) {
                    Some(value) => value,
                    None => {
                        self.fail_closed()?;
                        return Err("missing intern string".into());
                    }
                };
                let got = maps.intern(&value);
                if got != id {
                    return Err("intern id mismatch".into());
                }
            }
            for (kind, list) in &dir.by_kind {
                for key in self.list_ids(*list) {
                    let pairs = match self.identity((dir.ns_id, *kind, key)) {
                        Some((_, owned)) => owned,
                        None => {
                            self.fail_closed()?;
                            Vec::new()
                        }
                    };
                    maps.insert_visible_ids(*kind, key, pairs)?;
                }
            }
            for ((leaf, join, sum), dref) in &dir.rollups {
                for i in 0..dref.count as usize {
                    let page = dref.page + (i / PARENTS_PER_PAGE) as u32;
                    let off = (i % PARENTS_PER_PAGE) * PARENT_ENTRY;
                    let rec = self.page(page)?;
                    let parent = u32::from_le_bytes(rec[off..off + 4].try_into().unwrap());
                    if let Some(row) = self.rollup(dir, (*leaf, *join, *sum), parent) {
                        maps.insert_rollup((*leaf, *join, *sum), parent, row);
                    }
                }
            }
            for (leaf, sum) in &dir.declared {
                maps.insert_declared(*leaf, *sum);
            }
            maps.recover_cached_sums();
            loaded.joins.insert(name.clone(), maps);
        }
        for (id, meta, owned) in self.identities()? {
            if meta.hidden {
                loaded.hidden_props.insert(id, owned);
            }
            loaded.identity.insert(id, meta);
        }
        Ok(loaded)
    }
}

pub(crate) fn persist_pages(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if !bytes.len().is_multiple_of(PAGE) {
        return Err("paged projection is not page aligned".into());
    }
    let tmp = path.with_extension("tmp");
    let result = (|| {
        let mut file = crate::fault::create(&tmp)?;
        crate::fault::write_all(&mut file, bytes)?;
        crate::fault::sync_data(&file)?;
        crate::durable::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub(crate) fn decode_path(path: &Path) -> Result<crate::joins::Checkpoint, String> {
    let file = PagedFile::open(path, ServingBudget::from_bytes(PAGE * 32).expect("nonzero"))?;
    file.decode_all()
}
