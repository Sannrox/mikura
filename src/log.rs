//! 4KiB checksummed pages + group-commit pointer.
//! Rebuild trusts only pages 1..=committed.

use crc32fast::Hasher as Crc;
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use crate::codec::{read_str, take, write_str};
use crate::crypto::{
    decode_catalog, encode_catalog, encrypted_overhead, open_page_body, read_page_tenant,
    seal_page_body, tenant_of_namespace, Dek, DekStatus, LogKeys, WrappedDek, STORE_KEY_TENANT,
};
use crate::store::ObjectRecord;

pub const PAGE: usize = 4096;
const MAGIC: &[u8; 8] = crate::LOG_FORMAT;
const V3_MAGIC: &[u8; 8] = crate::LOG_FORMAT_ENCRYPTED;
/// Pre-namespace magic. Read only by [`read_v1_records`].
const V1_MAGIC: &[u8; 8] = b"MIKURAV1";
const CRC_LEN: usize = 4;
const USED_LEN: usize = 2;
const PAGE_HDR: usize = CRC_LEN + USED_LEN;
const SUPER_COMMIT_OFF: usize = CRC_LEN + 8 + 2;
const UNCERTAIN_COMMIT: &str =
    "log state uncertain after a failed commit or rollback; reopen the store";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncPolicy {
    None,
    Page,
    Group(u32),
}

impl SyncPolicy {
    fn durable(self) -> bool {
        !matches!(self, Self::None)
    }

    fn group_size(self) -> u32 {
        match self {
            Self::None => u32::MAX,
            Self::Page => 1,
            Self::Group(n) => n,
        }
    }
}

pub struct LogWriter {
    file: File,
    page: Vec<u8>,
    used: usize,
    written: u32,
    committed: u32,
    sync: SyncPolicy,
    fsync_count: u64,
    hold_commits: bool,
    synced: u32,
    failed: bool,
    keys: Option<Arc<LogKeys>>,
    catalog: BTreeMap<String, WrappedDek>,
    deks: HashMap<String, Dek>,
    revoked: HashSet<String>,
    withheld: HashSet<String>,
    page_tenant: Option<String>,
}

impl LogWriter {
    pub fn create(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = File::create(path).map_err(|e| e.to_string())?;
        let mut writer = Self::blank(file, sync, 0);
        writer.write_superblock()?;
        Ok(writer)
    }

    pub fn create_with_keys(path: &Path, sync: SyncPolicy, keys: LogKeys) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = File::create(path).map_err(|e| e.to_string())?;
        let mut writer = Self::blank(file, sync, 0);
        writer.install_keys(Arc::new(keys))?;
        writer.mint_store_dek()?;
        writer.write_superblock()?;
        Ok(writer)
    }

    pub fn open(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        Self::open_inner(path, sync, None)
    }

    pub fn open_with_keys(path: &Path, sync: SyncPolicy, keys: LogKeys) -> Result<Self, String> {
        Self::open_inner(path, sync, Some(keys))
    }

    fn blank(file: File, sync: SyncPolicy, committed: u32) -> Self {
        Self {
            file,
            page: vec![0u8; PAGE],
            used: 0,
            written: committed,
            committed,
            sync,
            fsync_count: 0,
            hold_commits: false,
            synced: committed,
            failed: false,
            keys: None,
            catalog: BTreeMap::new(),
            deks: HashMap::new(),
            revoked: HashSet::new(),
            withheld: HashSet::new(),
            page_tenant: None,
        }
    }

    fn open_inner(path: &Path, sync: SyncPolicy, keys: Option<LogKeys>) -> Result<Self, String> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        let mut superblock = vec![0u8; PAGE];
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        std::io::Read::read_exact(&mut file, &mut superblock).map_err(|e| e.to_string())?;
        let magic = superblock_magic(&superblock)?;
        if magic == V1_MAGIC {
            return Err(
                "bad magic: MIKURAV1 logs need Store::migrate_unnamespaced before open".into(),
            );
        }
        if magic != MAGIC && magic != V3_MAGIC {
            return Err("bad magic".into());
        }
        if magic == V3_MAGIC && keys.is_none() {
            return Err("encrypted log requires keys".into());
        }
        if magic == MAGIC && keys.is_some() {
            return Err("log is not encrypted".into());
        }
        let committed = superblock_commit(&superblock);
        let end = PAGE as u64 * u64::from(1 + committed);
        file.seek(SeekFrom::Start(end)).map_err(|e| e.to_string())?;
        let mut writer = Self::blank(file, sync, committed);
        if let Some(keys) = keys {
            writer.install_keys(Arc::new(keys))?;
            writer.load_catalog(&superblock)?;
        }
        Ok(writer)
    }

    fn install_keys(&mut self, keys: Arc<LogKeys>) -> Result<(), String> {
        self.keys = Some(keys);
        Ok(())
    }

    fn mint_store_dek(&mut self) -> Result<(), String> {
        let keys = self
            .keys
            .clone()
            .ok_or_else(|| "encrypted log requires keys".to_string())?;
        let (dek, wrapped) = keys.mint(STORE_KEY_TENANT)?;
        self.catalog.insert(String::new(), wrapped);
        self.deks.insert(STORE_KEY_TENANT.to_string(), dek);
        Ok(())
    }

    fn load_catalog(&mut self, superblock: &[u8]) -> Result<(), String> {
        let mut cur = &superblock[SUPER_COMMIT_OFF + 4..];
        // Trailing CRC padding is zeros after the catalog.
        let catalog = decode_catalog(&mut cur)?;
        let keys = self
            .keys
            .clone()
            .ok_or_else(|| "encrypted log requires keys".to_string())?;
        for (tenant, wrapped) in catalog {
            let dek_key = if tenant.is_empty() {
                STORE_KEY_TENANT.to_string()
            } else {
                tenant.clone()
            };
            match keys.unwrap_status(&wrapped)? {
                DekStatus::Ready(dek) => {
                    self.deks.insert(dek_key, dek);
                    self.catalog.insert(tenant, wrapped);
                }
                DekStatus::Withheld => {
                    self.withheld.insert(dek_key);
                    self.catalog.insert(tenant, wrapped);
                }
                DekStatus::Shredded => {
                    self.revoked.insert(dek_key);
                    self.catalog.insert(tenant, wrapped);
                }
            }
        }
        if !self.deks.contains_key(STORE_KEY_TENANT) {
            return Err("encrypted log is missing a usable store key".into());
        }
        Ok(())
    }

    pub(crate) fn keys(&self) -> Option<&LogKeys> {
        self.keys.as_deref()
    }

    pub(crate) fn keys_arc(&self) -> Option<Arc<LogKeys>> {
        self.keys.clone()
    }

    pub(crate) fn store_dek(&self) -> Option<&Dek> {
        self.deks.get(STORE_KEY_TENANT)
    }

    pub(crate) fn tenant_revoked(&self, tenant: &str) -> bool {
        self.revoked.contains(tenant)
    }

    pub(crate) fn tenant_withheld(&self, tenant: &str) -> bool {
        self.withheld.contains(tenant)
    }

    pub(crate) fn withheld(&self) -> &HashSet<String> {
        &self.withheld
    }

    pub(crate) fn tenant_dek(&self, tenant: &str) -> Option<&Dek> {
        self.deks.get(tenant)
    }

    pub(crate) fn deks(&self) -> &HashMap<String, Dek> {
        &self.deks
    }

    pub(crate) fn tenant_key_usable(&self, tenant: &str) -> bool {
        if self.keys.is_none() {
            return true;
        }
        if self.revoked.contains(tenant) || self.withheld.contains(tenant) {
            return false;
        }
        let Some(keys) = self.keys.as_ref() else {
            return true;
        };
        let Some(wrapped) = self.catalog.get(tenant) else {
            return self.deks.contains_key(tenant);
        };
        matches!(keys.unwrap_status(wrapped), Ok(DekStatus::Ready(_)))
    }

    pub(crate) fn revalidate_catalog(&mut self) -> Result<(), String> {
        let Some(keys) = self.keys.clone() else {
            return Ok(());
        };
        let entries: Vec<_> = self.catalog.clone().into_iter().collect();
        for (tenant, wrapped) in entries {
            let name = if tenant.is_empty() {
                STORE_KEY_TENANT.to_string()
            } else {
                tenant
            };
            match keys.unwrap_status(&wrapped)? {
                DekStatus::Ready(_) => {}
                DekStatus::Withheld => {
                    self.deks.remove(&name);
                    self.withheld.insert(name);
                }
                DekStatus::Shredded => {
                    if name == STORE_KEY_TENANT {
                        return Err("encrypted log is missing a usable store key".into());
                    }
                    self.deks.remove(&name);
                    self.revoked.insert(name);
                }
            }
        }
        Ok(())
    }

    fn ensure_tenant_dek(&mut self, tenant: &str) -> Result<(), String> {
        if self.revoked.contains(tenant) {
            return Err("encrypted writes refuse revoked tenant".into());
        }
        if self.deks.contains_key(tenant) {
            self.revalidate_catalog()?;
            if self.revoked.contains(tenant) {
                return Err("encrypted writes refuse revoked tenant".into());
            }
            if self.withheld.contains(tenant) {
                return Err("encrypted writes refuse tenant whose key is unavailable".into());
            }
            if self.deks.contains_key(tenant) {
                return Ok(());
            }
        }
        if self.withheld.contains(tenant) || self.catalog.contains_key(tenant) {
            return Err("encrypted writes refuse tenant whose key is unavailable".into());
        }
        let keys = self
            .keys
            .clone()
            .ok_or_else(|| "encrypted log requires keys".to_string())?;
        if keys.grant_for(tenant).is_none() {
            return Err("encrypted writes refuse tenant without unwrap grant".into());
        }
        let (dek, wrapped) = keys.mint(tenant)?;
        self.catalog.insert(tenant.to_string(), wrapped);
        self.deks.insert(tenant.to_string(), dek);
        Ok(())
    }

    pub fn fsync_count(&self) -> u64 {
        self.fsync_count
    }

    pub fn committed_pages(&self) -> u32 {
        self.committed
    }

    pub fn written_pages(&self) -> u32 {
        self.written
    }

    pub fn page_used(&self) -> usize {
        self.used
    }

    /// True after a superblock write failed and left the commit point unknown,
    /// or after a rollback failed and left the live projection unreliable.
    pub fn is_failed(&self) -> bool {
        self.failed
    }

    /// Refuse every further write until the log is reopened.
    pub fn poison(&mut self) {
        self.failed = true;
    }

    pub(crate) fn ensure_usable(&self) -> Result<(), String> {
        if self.failed {
            return Err(UNCERTAIN_COMMIT.into());
        }
        Ok(())
    }

    /// Appended records the superblock does not cover yet.
    pub fn has_pending(&self) -> bool {
        self.used > 0 || self.written > self.committed
    }

    /// While held, full pages are still written and data-synced in group
    /// sized steps, but nothing advances the superblock (the commit point),
    /// so only an explicit [`Self::flush`] makes them part of the log.
    pub fn hold_commits(&mut self, hold: bool) {
        self.hold_commits = hold;
        self.synced = self.committed;
    }

    pub fn holds_commits(&self) -> bool {
        self.hold_commits
    }

    /// Drop every page and record the superblock does not cover, as a crash
    /// before commit would (ADR 0001).
    pub fn discard_pending(&mut self) -> Result<(), String> {
        self.ensure_usable()?;
        self.page = vec![0u8; PAGE];
        self.used = 0;
        self.page_tenant = None;
        self.written = self.committed;
        self.synced = self.committed;
        let end = PAGE as u64 * u64::from(1 + self.committed);
        self.file.set_len(end).map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(end))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn page_capacity(&self, tenant: Option<&str>) -> usize {
        match tenant {
            Some(tenant) if self.keys.is_some() => {
                let overhead = CRC_LEN + encrypted_overhead(tenant.len()) + USED_LEN;
                PAGE.saturating_sub(overhead)
            }
            _ => PAGE - PAGE_HDR,
        }
    }

    /// Fail closed when `record` cannot fit on a 4 KiB page.
    pub(crate) fn check_record_fits(&self, record: &ObjectRecord) -> Result<(), String> {
        let framed = framed_record(&encode_body(record)?)?;
        let tenant = if self.keys.is_some() {
            Some(tenant_of_namespace(&record.namespace)?)
        } else {
            None
        };
        if framed.len() > self.page_capacity(tenant) {
            return Err("record larger than a page".into());
        }
        Ok(())
    }

    pub fn append_record(&mut self, record: &ObjectRecord) -> Result<(), String> {
        self.ensure_usable()?;
        let framed = framed_record(&encode_body(record)?)?;
        let tenant = if self.keys.is_some() {
            Some(tenant_of_namespace(&record.namespace)?.to_string())
        } else {
            None
        };
        if let Some(tenant) = tenant.as_deref() {
            self.ensure_tenant_dek(tenant)?;
        }
        if framed.len() > self.page_capacity(tenant.as_deref()) {
            return Err("record larger than a page".into());
        }
        if let Some(tenant) = tenant.as_deref() {
            if self.used > 0 && self.page_tenant.as_deref() != Some(tenant) {
                self.flush_page()?;
            }
            if self.used == 0 {
                self.page_tenant = Some(tenant.to_string());
            }
        }
        if self.used + framed.len() > self.page_capacity(self.page_tenant.as_deref()) {
            self.flush_page()?;
            if let Some(tenant) = tenant {
                self.page_tenant = Some(tenant);
            }
        }
        let start = PAGE_HDR + self.used;
        self.page[start..start + framed.len()].copy_from_slice(&framed);
        self.used += framed.len();
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), String> {
        self.ensure_usable()?;
        self.flush_page()?;
        self.commit_pending()?;
        self.file.flush().map_err(|e| e.to_string())
    }

    fn flush_page(&mut self) -> Result<(), String> {
        if self.used == 0 {
            return Ok(());
        }
        self.page[CRC_LEN..PAGE_HDR].copy_from_slice(&(self.used as u16).to_le_bytes());
        let disk = if self.keys.is_some() {
            self.seal_current_page()?
        } else {
            let mut disk = self.page.clone();
            seal_page(&mut disk);
            disk
        };
        self.file.write_all(&disk).map_err(|e| e.to_string())?;
        self.written += 1;
        self.page = vec![0u8; PAGE];
        self.used = 0;
        self.page_tenant = None;
        if self.hold_commits {
            if self.sync.durable() && self.written - self.synced >= self.sync.group_size() {
                self.sync_data()?;
                self.synced = self.written;
            }
            return Ok(());
        }
        if self.sync == SyncPolicy::None {
            self.committed = self.written;
            return self.write_superblock();
        }
        let pending = self.written - self.committed;
        if pending >= self.sync.group_size() {
            self.commit_pending()?;
        }
        Ok(())
    }

    fn commit_pending(&mut self) -> Result<(), String> {
        if self.written == self.committed {
            return Ok(());
        }
        self.sync_data()?;
        self.committed = self.written;
        self.write_superblock()
    }

    /// A failed superblock write leaves the on-disk commit point unknown: the
    /// new superblock may or may not have landed. The writer refuses every
    /// further write and never truncates, so the next open recovers from
    /// whatever the disk holds.
    fn write_superblock(&mut self) -> Result<(), String> {
        self.write_superblock_body().map_err(|error| {
            self.failed = true;
            format!("{UNCERTAIN_COMMIT}: {error}")
        })
    }

    fn seal_current_page(&self) -> Result<Vec<u8>, String> {
        let tenant = self
            .page_tenant
            .as_deref()
            .ok_or_else(|| "encrypted page is missing tenant".to_string())?;
        let dek = self
            .deks
            .get(tenant)
            .ok_or_else(|| "encrypted page is missing tenant key".to_string())?;
        let inner_len = PAGE
            .checked_sub(CRC_LEN + encrypted_overhead(tenant.len()))
            .ok_or_else(|| "encrypted page overhead exceeds page".to_string())?;
        let mut inner = vec![0u8; inner_len];
        inner[..USED_LEN].copy_from_slice(&self.page[CRC_LEN..PAGE_HDR]);
        inner[USED_LEN..USED_LEN + self.used]
            .copy_from_slice(&self.page[PAGE_HDR..PAGE_HDR + self.used]);
        let sealed = seal_page_body(dek, tenant, self.written + 1, &inner)?;
        if CRC_LEN + sealed.len() > PAGE {
            return Err("encrypted page overflow".into());
        }
        let mut disk = vec![0u8; PAGE];
        disk[CRC_LEN..CRC_LEN + sealed.len()].copy_from_slice(&sealed);
        seal_page(&mut disk);
        Ok(disk)
    }

    fn write_superblock_body(&mut self) -> Result<(), String> {
        let superblock = if self.keys.is_some() {
            superblock_page_v3(self.committed, &self.catalog)?
        } else {
            superblock_page(self.committed)
        };
        let pos = self.file.stream_position().map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        self.file
            .write_all(&superblock)
            .map_err(|e| e.to_string())?;
        self.sync_data()?;
        self.file
            .seek(SeekFrom::Start(pos.max(PAGE as u64)))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn sync_data(&mut self) -> Result<(), String> {
        if self.sync.durable() {
            self.file.sync_data().map_err(|e| e.to_string())?;
            self.fsync_count += 1;
        }
        Ok(())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn read_records(path: &Path) -> Result<Vec<ObjectRecord>, String> {
    read_records_with_keys(path, None)
}

pub(crate) fn read_records_with_keys(
    path: &Path,
    keys: Option<&LogKeys>,
) -> Result<Vec<ObjectRecord>, String> {
    read_committed_records(path, None, 0, keys)
}

/// Committed records on pages `(after_page + 1)..=committed_pages`.
/// `after_page == 0` is a full replay.
#[cfg_attr(not(test), allow(dead_code))]
pub fn read_records_after(path: &Path, after_page: u32) -> Result<Vec<ObjectRecord>, String> {
    read_records_after_with_keys(path, after_page, None)
}

pub(crate) fn read_records_after_with_keys(
    path: &Path,
    after_page: u32,
    keys: Option<&LogKeys>,
) -> Result<Vec<ObjectRecord>, String> {
    read_committed_records(path, None, after_page, keys)
}

/// Committed records that `keep` accepts, with the page that holds each, in
/// log order (ADR 0036). Every commit ends on a page boundary, so a page that a
/// commit returned as `committed_pages` is an exact commit point.
pub(crate) fn read_records_with_pages_keys(
    path: &Path,
    keys: Option<&LogKeys>,
    mut keep: impl FnMut(&ObjectRecord) -> bool,
) -> Result<Vec<(u32, ObjectRecord)>, String> {
    let mut paged = Vec::new();
    visit_committed_pages(path, None, 0, keys, |index, records| {
        paged.extend(
            records
                .into_iter()
                .filter(|record| keep(record))
                .map(|record| (index, record)),
        );
    })?;
    Ok(paged)
}

/// Committed records of a pre-namespace `MIKURAV1` log, each assigned the
/// caller's `namespace` (ADR 0030). Any other magic fails closed.
pub fn read_v1_records(path: &Path, namespace: &str) -> Result<Vec<ObjectRecord>, String> {
    if namespace.is_empty() {
        return Err("namespace required".into());
    }
    read_committed_records(path, Some(namespace), 0, None)
}

/// `v1_namespace` selects the `MIKURAV1` body layout and the namespace its
/// records are assigned; `None` reads `MIKURAV2`.
fn read_committed_records(
    path: &Path,
    v1_namespace: Option<&str>,
    after_page: u32,
    keys: Option<&LogKeys>,
) -> Result<Vec<ObjectRecord>, String> {
    let mut all = Vec::new();
    visit_committed_pages(path, v1_namespace, after_page, keys, |_, records| {
        all.extend(records);
    })?;
    Ok(all)
}

/// Verifies and decodes committed pages after `after_page`, handing each
/// page's records to `visit` with the page index.
fn visit_committed_pages(
    path: &Path,
    v1_namespace: Option<&str>,
    after_page: u32,
    keys: Option<&LogKeys>,
    mut visit: impl FnMut(u32, Vec<ObjectRecord>),
) -> Result<(), String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut superblock = vec![0u8; PAGE];
    file.read_exact(&mut superblock)
        .map_err(|_| "missing superblock".to_string())?;
    let committed = match v1_namespace {
        None => parse_superblock(&superblock)?,
        Some(_) => parse_v1_superblock(&superblock)?,
    };
    let magic = if v1_namespace.is_some() {
        V1_MAGIC
    } else {
        superblock_magic(&superblock)?
    };
    if magic == V3_MAGIC && keys.is_none() {
        return Err("encrypted log requires keys".into());
    }
    let deks = if magic == V3_MAGIC {
        Some(unwrap_catalog(
            keys.ok_or("encrypted log requires keys")?,
            &superblock,
        )?)
    } else {
        None
    };
    let needed = PAGE as u64 * u64::from(1 + committed);
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len < needed {
        return Err("committed pages missing".into());
    }
    if after_page > committed {
        return Err("checkpoint page is past committed_pages".into());
    }
    if after_page < committed {
        let start = PAGE as u64 * u64::from(after_page + 1);
        file.seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        let mut page = vec![0u8; PAGE];
        for index in (after_page + 1)..=committed {
            file.read_exact(&mut page)
                .map_err(|_| format!("committed page {index}: short read"))?;
            verify_page(&page).map_err(|e| format!("committed page {index}: {e}"))?;
            let mut records = Vec::new();
            if let Some(deks) = deks.as_ref() {
                decode_encrypted_page(&page, index, deks, &mut records)
                    .map_err(|e| format!("committed page {index}: {e}"))?;
            } else {
                decode_data_page(&page, v1_namespace, &mut records)?;
            }
            visit(index, records);
        }
    }
    Ok(())
}

fn unwrap_catalog(keys: &LogKeys, superblock: &[u8]) -> Result<HashMap<String, Dek>, String> {
    let mut cur = &superblock[SUPER_COMMIT_OFF + 4..];
    let catalog = decode_catalog(&mut cur)?;
    let mut deks = HashMap::new();
    for (tenant, wrapped) in catalog {
        if let Some(dek) = keys.unwrap_dek(&wrapped)? {
            let dek_key = if tenant.is_empty() {
                STORE_KEY_TENANT.to_string()
            } else {
                tenant
            };
            deks.insert(dek_key, dek);
        }
    }
    Ok(deks)
}

fn decode_encrypted_page(
    page: &[u8],
    index: u32,
    deks: &HashMap<String, Dek>,
    records: &mut Vec<ObjectRecord>,
) -> Result<(), String> {
    let (tenant, body) = read_page_tenant(&page[CRC_LEN..])?;
    let Some(dek) = deks.get(tenant) else {
        return Ok(());
    };
    let inner = open_page_body(dek, tenant, index, body)?;
    let mut fake = vec![0u8; PAGE];
    if CRC_LEN + inner.len() > PAGE {
        return Err("encrypted page inner overflow".into());
    }
    fake[CRC_LEN..CRC_LEN + inner.len()].copy_from_slice(&inner);
    decode_data_page(&fake, None, records)
}

/// A sealed `MIKURAV2` superblock that commits pages `1..=committed`.
pub(crate) fn superblock_page(committed: u32) -> Vec<u8> {
    let mut superblock = vec![0u8; PAGE];
    superblock[CRC_LEN..CRC_LEN + 8].copy_from_slice(MAGIC);
    superblock[CRC_LEN + 8..CRC_LEN + 10].copy_from_slice(&(PAGE as u16).to_le_bytes());
    superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4].copy_from_slice(&committed.to_le_bytes());
    seal_page(&mut superblock);
    superblock
}

fn superblock_page_v3(
    committed: u32,
    catalog: &BTreeMap<String, WrappedDek>,
) -> Result<Vec<u8>, String> {
    let mut superblock = vec![0u8; PAGE];
    superblock[CRC_LEN..CRC_LEN + 8].copy_from_slice(V3_MAGIC);
    superblock[CRC_LEN + 8..CRC_LEN + 10].copy_from_slice(&(PAGE as u16).to_le_bytes());
    superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4].copy_from_slice(&committed.to_le_bytes());
    let encoded = encode_catalog(catalog)?;
    let start = SUPER_COMMIT_OFF + 4;
    if start + encoded.len() > PAGE {
        return Err("key catalog does not fit in the superblock".into());
    }
    superblock[start..start + encoded.len()].copy_from_slice(&encoded);
    seal_page(&mut superblock);
    Ok(superblock)
}

/// Copy a superblock, set `committed`, and reseal so backup keeps a V3 catalog.
pub(crate) fn superblock_with_committed(page: &[u8], committed: u32) -> Result<Vec<u8>, String> {
    verify_page(page).map_err(|e| format!("superblock: {e}"))?;
    let mut superblock = page.to_vec();
    superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4].copy_from_slice(&committed.to_le_bytes());
    seal_page(&mut superblock);
    Ok(superblock)
}

pub(crate) fn superblock_magic(superblock: &[u8]) -> Result<&[u8; 8], String> {
    verify_page(superblock).map_err(|e| format!("superblock: {e}"))?;
    superblock[CRC_LEN..CRC_LEN + 8]
        .try_into()
        .map_err(|_| "bad magic".to_string())
}

/// The stored CRC of a sealed page.
pub(crate) fn page_crc(page: &[u8]) -> u32 {
    u32::from_le_bytes(page[..CRC_LEN].try_into().unwrap())
}

pub(crate) fn parse_superblock(superblock: &[u8]) -> Result<u32, String> {
    verify_page(superblock).map_err(|e| format!("superblock: {e}"))?;
    let magic = &superblock[CRC_LEN..CRC_LEN + 8];
    if magic == V1_MAGIC {
        return Err("bad magic: MIKURAV1 logs need Store::migrate_unnamespaced before open".into());
    } else if magic != MAGIC && magic != V3_MAGIC {
        return Err("bad magic".into());
    }
    Ok(superblock_commit(superblock))
}

fn parse_v1_superblock(superblock: &[u8]) -> Result<u32, String> {
    verify_page(superblock).map_err(|e| format!("superblock: {e}"))?;
    let magic = &superblock[CRC_LEN..CRC_LEN + 8];
    if magic == MAGIC {
        return Err("log is already namespaced (MIKURAV2)".into());
    } else if magic != V1_MAGIC {
        return Err("bad magic".into());
    }
    Ok(superblock_commit(superblock))
}

fn superblock_commit(superblock: &[u8]) -> u32 {
    u32::from_le_bytes(
        superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4]
            .try_into()
            .unwrap(),
    )
}

fn seal_page(page: &mut [u8]) {
    let mut hasher = Crc::new();
    hasher.update(&page[CRC_LEN..]);
    page[..CRC_LEN].copy_from_slice(&hasher.finalize().to_le_bytes());
}

pub(crate) fn verify_page(page: &[u8]) -> Result<(), String> {
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

fn framed_record(body: &[u8]) -> Result<Vec<u8>, String> {
    let len = u16::try_from(body.len()).map_err(|_| "record too large".to_string())?;
    let mut out = Vec::with_capacity(2 + body.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

fn encode_body(record: &ObjectRecord) -> Result<Vec<u8>, String> {
    if record.namespace.is_empty() {
        return Err("namespace required".into());
    }
    let mut body = Vec::new();
    body.extend_from_slice(&record.gen.to_le_bytes());
    body.push(u8::from(record.hidden));
    write_str(&mut body, &record.namespace)?;
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
    // Trailing Action id string when present. Schema version follows as a
    // little-endian u32 only when it is not 1 (ADR 0035). Version-only
    // records write an empty Action string then the u32.
    let version = if record.schema_version == 0 {
        1
    } else {
        record.schema_version
    };
    let action_id = record.action_id.as_deref().filter(|id| !id.is_empty());
    if version > 1 {
        write_str(&mut body, action_id.unwrap_or(""))?;
        body.extend_from_slice(&version.to_le_bytes());
    } else if let Some(action_id) = action_id {
        write_str(&mut body, action_id)?;
    }
    Ok(body)
}

#[cfg(test)]
thread_local! {
    static PAGES_DECODED: Cell<u32> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_pages_decoded() -> u32 {
    PAGES_DECODED.with(|c| c.replace(0))
}

/// Records of one verified `MIKURAV2` data page.
pub(crate) fn decode_page(page: &[u8]) -> Result<Vec<ObjectRecord>, String> {
    let mut records = Vec::new();
    decode_data_page(page, None, &mut records)?;
    Ok(records)
}

fn decode_data_page(
    page: &[u8],
    v1_namespace: Option<&str>,
    records: &mut Vec<ObjectRecord>,
) -> Result<(), String> {
    #[cfg(test)]
    PAGES_DECODED.with(|c| c.set(c.get() + 1));
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
        records.push(decode_body(&cur[..len], v1_namespace)?);
        cur = &cur[len..];
    }
    Ok(())
}

/// A `MIKURAV1` body is the `MIKURAV2` body without the namespace field;
/// `v1_namespace` supplies it.
fn decode_body(body: &[u8], v1_namespace: Option<&str>) -> Result<ObjectRecord, String> {
    let mut cur = body;
    let gen = u64::from_le_bytes(take::<8>(&mut cur)?.try_into().unwrap());
    let hidden = take::<1>(&mut cur)?[0] != 0;
    let namespace = match v1_namespace {
        Some(namespace) => namespace.to_string(),
        None => read_str(&mut cur)?,
    };
    if namespace.is_empty() {
        return Err("namespace required".into());
    }
    let kind = read_str(&mut cur)?;
    let key = read_str(&mut cur)?;
    let nprops = u16::from_le_bytes(take::<2>(&mut cur)?.try_into().unwrap()) as usize;
    let mut props = HashMap::new();
    for _ in 0..nprops {
        let k = read_str(&mut cur)?;
        let v = read_str(&mut cur)?;
        props.insert(k, v);
    }
    let (action_id, schema_version) = if cur.is_empty() {
        (None, 1)
    } else {
        let id = read_str(&mut cur)?;
        let action_id = if id.is_empty() { None } else { Some(id) };
        if cur.is_empty() {
            (action_id, 1)
        } else if cur.len() == 4 {
            let version = u32::from_le_bytes(take::<4>(&mut cur)?.try_into().unwrap());
            if version == 0 {
                return Err("schema version must be at least 1".into());
            }
            (action_id, version)
        } else {
            return Err("trailing body bytes".into());
        }
    };
    Ok(ObjectRecord {
        gen,
        namespace,
        kind,
        key,
        hidden,
        action_id,
        props,
        schema_version,
    })
}

/// Write `records` as a committed pre-namespace `MIKURAV1` log, one record
/// per page, for migration tests.
#[cfg(test)]
pub(crate) fn write_v1_log(path: &Path, records: &[ObjectRecord]) -> Result<(), String> {
    let mut bytes = vec![0u8; PAGE];
    for record in records {
        // The V1 body is the V2 body without the namespace string.
        let body = encode_body(record)?;
        let namespace_field = 2 + record.namespace.len();
        let mut v1 = body[..9].to_vec();
        v1.extend_from_slice(&body[9 + namespace_field..]);
        let framed = framed_record(&v1)?;
        let mut page = vec![0u8; PAGE];
        page[CRC_LEN..PAGE_HDR].copy_from_slice(&(framed.len() as u16).to_le_bytes());
        page[PAGE_HDR..PAGE_HDR + framed.len()].copy_from_slice(&framed);
        seal_page(&mut page);
        bytes.extend_from_slice(&page);
    }
    bytes[CRC_LEN..CRC_LEN + 8].copy_from_slice(V1_MAGIC);
    bytes[CRC_LEN + 8..CRC_LEN + 10].copy_from_slice(&(PAGE as u16).to_le_bytes());
    bytes[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4]
        .copy_from_slice(&(records.len() as u32).to_le_bytes());
    seal_page(&mut bytes[..PAGE]);
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_records_after_skips_prefix_pages() {
        let dir = std::env::temp_dir().join("mikura-log-tail");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.mikura");
        let mut writer = LogWriter::create(&path, SyncPolicy::None).unwrap();
        writer.append_record(&wide_record(0)).unwrap();
        writer.flush().unwrap();
        let prefix = writer.committed_pages();
        writer.append_record(&wide_record(1)).unwrap();
        writer.flush().unwrap();
        let all = read_records(&path).unwrap();
        let tail = read_records_after(&path, prefix).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].key, "k01");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphan_page_is_not_authority() {
        let dir = std::env::temp_dir().join("mikura-log-orphan");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.mikura");
        let mut writer = LogWriter::create(&path, SyncPolicy::None).unwrap();
        writer
            .append_record(&ObjectRecord {
                gen: 1,
                namespace: "ns".into(),
                kind: "Customer".into(),
                key: "c1".into(),
                hidden: false,
                action_id: None,
                schema_version: 1,
                props: HashMap::from([("region".into(), "eu".into())]),
            })
            .unwrap();
        writer.flush().unwrap();
        let before = read_records(&path).unwrap();
        assert_eq!(before.len(), 1);
        let mut bytes = std::fs::read(&path).unwrap();
        let mut page = vec![0u8; PAGE];
        page[CRC_LEN..PAGE_HDR].copy_from_slice(&0u16.to_le_bytes());
        seal_page(&mut page);
        bytes.extend_from_slice(&page);
        std::fs::write(&path, bytes).unwrap();
        let after = read_records(&path).unwrap();
        assert_eq!(after.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn wide_record(key: usize) -> ObjectRecord {
        ObjectRecord {
            gen: 1,
            namespace: "ns".into(),
            kind: "Item".into(),
            key: format!("k{key:02}"),
            hidden: false,
            action_id: None,
            schema_version: 1,
            props: HashMap::from([("payload".into(), "x".repeat(1500))]),
        }
    }

    #[test]
    fn held_pages_sync_in_groups_but_only_flush_commits_them() {
        let dir = std::env::temp_dir().join("mikura-log-hold");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.mikura");
        let mut writer = LogWriter::create(&path, SyncPolicy::Group(2)).unwrap();
        writer.hold_commits(true);
        for key in 0..12 {
            writer.append_record(&wide_record(key)).unwrap();
        }
        assert!(writer.written_pages() >= 4);
        assert_eq!(writer.committed_pages(), 0);
        assert!(writer.fsync_count() >= 2, "data syncs keep their cadence");
        assert!(writer.has_pending());

        writer.discard_pending().unwrap();
        assert!(!writer.has_pending());
        assert_eq!(writer.written_pages(), 0);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), PAGE as u64);
        assert!(read_records(&path).unwrap().is_empty());

        for key in 0..12 {
            writer.append_record(&wide_record(key)).unwrap();
        }
        writer.hold_commits(false);
        writer.flush().unwrap();
        assert_eq!(writer.committed_pages(), writer.written_pages());
        assert_eq!(read_records(&path).unwrap().len(), 12);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_superblock_write_poisons_the_writer_and_never_truncates() {
        let dir = std::env::temp_dir().join("mikura-log-uncertain");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.mikura");
        let mut writer = LogWriter::create(&path, SyncPolicy::Group(2)).unwrap();
        writer.append_record(&wide_record(0)).unwrap();
        writer.flush().unwrap();
        writer.hold_commits(true);
        for key in 1..4 {
            writer.append_record(&wide_record(key)).unwrap();
        }
        writer.flush_page().unwrap();
        writer.hold_commits(false);
        let length = std::fs::metadata(&path).unwrap().len();

        writer.file = File::open(&path).unwrap();
        let error = writer.flush().unwrap_err();
        assert!(error.contains("uncertain"), "{error}");
        assert!(writer.is_failed());
        assert!(writer.append_record(&wide_record(4)).is_err());
        assert!(writer.flush().is_err());
        assert!(writer.discard_pending().is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), length);

        let mut reopened = LogWriter::open(&path, SyncPolicy::Group(2)).unwrap();
        assert!(!reopened.is_failed());
        assert_eq!(read_records(&path).unwrap().len(), 1);
        reopened.append_record(&wide_record(5)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn historical_record_without_action_id_is_none() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u64.to_le_bytes());
        body.push(0);
        write_str(&mut body, "ns").unwrap();
        write_str(&mut body, "Customer").unwrap();
        write_str(&mut body, "c1").unwrap();
        body.extend_from_slice(&0u16.to_le_bytes());
        let record = decode_body(&body, None).unwrap();
        assert_eq!(record.action_id, None);
        assert_eq!(record.namespace, "ns");
        assert_eq!(record.kind, "Customer");
        assert_eq!(record.key, "c1");
    }

    /// Tagged v0.1.0 `decode_body`: any bytes after properties fail closed.
    fn decode_body_reject_trailing(body: &[u8]) -> Result<ObjectRecord, String> {
        let mut cur = body;
        let gen = u64::from_le_bytes(take::<8>(&mut cur)?.try_into().unwrap());
        let hidden = take::<1>(&mut cur)?[0] != 0;
        let namespace = read_str(&mut cur)?;
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
        Ok(ObjectRecord {
            gen,
            namespace,
            kind,
            key,
            hidden,
            action_id: None,
            props,
            schema_version: 1,
        })
    }

    #[test]
    fn none_action_id_omits_trailer_so_trailing_reject_readers_hold() {
        let none = ObjectRecord {
            gen: 1,
            namespace: "ns".into(),
            kind: "Customer".into(),
            key: "c1".into(),
            hidden: false,
            action_id: None,
            schema_version: 1,
            props: HashMap::from([("region".into(), "eu".into())]),
        };
        let encoded = encode_body(&none).unwrap();
        let current = decode_body(&encoded, None).unwrap();
        assert_eq!(current.action_id, None);
        let pinned = decode_body_reject_trailing(&encoded).unwrap();
        assert_eq!(pinned.kind, "Customer");
        assert_eq!(pinned.key, "c1");
        assert_eq!(pinned.props.get("region").map(String::as_str), Some("eu"));

        let some = ObjectRecord {
            action_id: Some("act-1".into()),
            ..none
        };
        let encoded = encode_body(&some).unwrap();
        assert_eq!(
            decode_body(&encoded, None).unwrap().action_id.as_deref(),
            Some("act-1")
        );
        let err = decode_body_reject_trailing(&encoded).unwrap_err();
        assert!(err.contains("trailing"), "{err}");
    }

    #[test]
    fn schema_version_trailer_is_omitted_unless_greater_than_one() {
        let v1 = ObjectRecord {
            gen: 1,
            namespace: "ns".into(),
            kind: "Customer".into(),
            key: "c1".into(),
            hidden: false,
            action_id: None,
            schema_version: 1,
            props: HashMap::from([("region".into(), "eu".into())]),
        };
        let encoded = encode_body(&v1).unwrap();
        assert_eq!(decode_body(&encoded, None).unwrap().schema_version, 1);
        decode_body_reject_trailing(&encoded).unwrap();

        let v2 = ObjectRecord {
            schema_version: 2,
            ..v1
        };
        let encoded = encode_body(&v2).unwrap();
        let decoded = decode_body(&encoded, None).unwrap();
        assert_eq!(decoded.schema_version, 2);
        assert_eq!(decoded.action_id, None);
        let err = decode_body_reject_trailing(&encoded).unwrap_err();
        assert!(err.contains("trailing"), "{err}");
    }

    #[test]
    fn second_trailing_body_field_fails_closed() {
        let body = encode_body(&ObjectRecord {
            gen: 1,
            namespace: "ns".into(),
            kind: "Customer".into(),
            key: "c1".into(),
            hidden: false,
            action_id: Some("act-1".into()),
            schema_version: 1,
            props: HashMap::new(),
        })
        .unwrap();
        let mut extra = body;
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.push(b'x');
        let err = decode_body(&extra, None).unwrap_err();
        assert!(err.contains("trailing"), "{err}");
    }

    #[test]
    fn missing_committed_page_fails_closed() {
        let dir = std::env::temp_dir().join("mikura-log-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.mikura");
        let mut writer = LogWriter::create(&path, SyncPolicy::None).unwrap();
        writer
            .append_record(&ObjectRecord {
                gen: 1,
                namespace: "ns".into(),
                kind: "Customer".into(),
                key: "c1".into(),
                hidden: false,
                action_id: None,
                schema_version: 1,
                props: HashMap::from([("region".into(), "eu".into())]),
            })
            .unwrap();
        writer.flush().unwrap();
        assert_eq!(read_records(&path).unwrap().len(), 1);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() >= PAGE * 2);
        std::fs::write(&path, &bytes[..PAGE]).unwrap();
        let err = read_records(&path).unwrap_err();
        assert!(
            err.contains("missing") || err.contains("committed"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mikurav1_superblock_fails_closed() {
        let dir = std::env::temp_dir().join("mikura-log-v1-magic");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.mikura");
        let mut writer = LogWriter::create(&path, SyncPolicy::None).unwrap();
        writer
            .append_record(&ObjectRecord {
                gen: 1,
                namespace: "ns".into(),
                kind: "Customer".into(),
                key: "c1".into(),
                hidden: false,
                action_id: None,
                schema_version: 1,
                props: HashMap::new(),
            })
            .unwrap();
        writer.flush().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[CRC_LEN..CRC_LEN + 8].copy_from_slice(b"MIKURAV1");
        let mut hasher = Crc::new();
        hasher.update(&bytes[CRC_LEN..PAGE]);
        bytes[..CRC_LEN].copy_from_slice(&hasher.finalize().to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        let err = read_records(&path).unwrap_err();
        assert!(err.contains("magic"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_namespace_fails_closed() {
        let err = encode_body(&ObjectRecord {
            gen: 1,
            namespace: String::new(),
            kind: "Customer".into(),
            key: "c1".into(),
            hidden: false,
            action_id: None,
            schema_version: 1,
            props: HashMap::new(),
        })
        .unwrap_err();
        assert!(err.contains("namespace"), "{err}");
    }
}
