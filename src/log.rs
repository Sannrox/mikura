//! 4KiB checksummed pages + group-commit pointer.
//! Rebuild trusts only pages 1..=committed.

use crc32fast::Hasher as Crc;
#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{File, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::codec::{read_str, take, write_str};
use crate::crypto::{
    decode_catalog, encode_catalog, encrypted_overhead, open_page_body, read_page_tenant,
    seal_page_body, tenant_of_namespace, Dek, DekStatus, LogKeys, WrappedDek, STORE_KEY_TENANT,
};
use crate::store::ObjectRecord;
use crate::UNCERTAIN_COMMIT;

pub const PAGE: usize = 4096;
const MAGIC: &[u8; 8] = crate::LOG_FORMAT;
const V3_MAGIC: &[u8; 8] = crate::LOG_FORMAT_ENCRYPTED;
const V4_MAGIC: &[u8; 8] = crate::LOG_FORMAT_MANIFEST;
/// Pre-namespace magic. Read only by [`read_v1_records`].
const V1_MAGIC: &[u8; 8] = b"MIKURAV1";
const STUB_FLAG_OFF: usize = CRC_LEN + 8 + 2;
const STUB_ENCRYPTED: u8 = 1;

const CRC_LEN: usize = 4;
const USED_LEN: usize = 2;
const PAGE_HDR: usize = CRC_LEN + USED_LEN;
const SUPER_COMMIT_OFF: usize = CRC_LEN + 8 + 2;

fn io_err(error: String) -> std::io::Error {
    std::io::Error::other(error)
}

/// Object namespace next to a `MIKURAV4` stub: `{log}.obj`.
pub(crate) fn objects_dir(log: &Path) -> PathBuf {
    let mut path = log.as_os_str().to_os_string();
    path.push(".obj");
    PathBuf::from(path)
}

/// Publish a staging log as `dst`. A V4 staging directory is renamed beside
/// the stub so the object namespace appears with the stub or not at all.
pub(crate) fn publish_log(staging: &Path, dst: &Path) -> Result<(), String> {
    std::fs::hard_link(staging, dst).map_err(|e| format!("publish {}: {e}", dst.display()))?;
    let from = objects_dir(staging);
    if from.exists() {
        if let Err(error) = std::fs::rename(&from, objects_dir(dst)) {
            let _ = std::fs::remove_file(dst);
            return Err(format!("publish {}: {error}", dst.display()));
        }
    }
    crate::durable::sync_parent(dst)
}

pub(crate) fn remove_log(path: &Path) {
    let objects = objects_dir(path);
    let _ = std::fs::remove_dir_all(&objects);
    let _ = std::fs::remove_file(&objects);
    let _ = std::fs::remove_file(path);
}

fn stub_page(encrypted: bool) -> Vec<u8> {
    let mut page = vec![0u8; PAGE];
    page[CRC_LEN..CRC_LEN + 8].copy_from_slice(V4_MAGIC);
    page[CRC_LEN + 8..CRC_LEN + 10].copy_from_slice(&(PAGE as u16).to_le_bytes());
    page[STUB_FLAG_OFF] = u8::from(encrypted) * STUB_ENCRYPTED;
    seal_page(&mut page);
    page
}

pub(crate) fn is_manifest_stub(page: &[u8]) -> bool {
    superblock_magic(page).ok() == Some(V4_MAGIC)
}

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
    /// Atomic so [`Self::write_staged`] can fail closed through `&self`.
    failed: AtomicBool,
    keys: Option<Arc<LogKeys>>,
    catalog: BTreeMap<String, WrappedDek>,
    deks: HashMap<String, Dek>,
    revoked: HashSet<String>,
    withheld: HashSet<String>,
    page_tenant: Option<String>,
    objects: Option<crate::objects::ObjectStore>,
    manifest_version: u64,
    segments: Vec<crate::objects::Segment>,
    prefix_segments: Vec<crate::objects::Segment>,
    checkpoint_key: String,
    projection_key: String,
    prefix_key: String,
    checkpoint_pages: u32,
    pending_pages: Vec<Vec<u8>>,
    encrypted_log: bool,
    /// True when this process opened without the writer probe or lock
    /// (ADR 0047 §5). Writes fail closed until [`Self::claim_writer`].
    read_only: bool,
    /// True after a writer-mode swap lost the version precondition.
    fenced: AtomicBool,
    #[cfg(test)]
    stage_hook: Option<StageHook>,
    #[cfg(test)]
    crash_before_swap: bool,
}

/// Where [`LogWriter::write_staged`] is when a test hook runs.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitStage {
    /// Pages are written past the commit point but not synced.
    PagesWritten,
    /// Half of the new superblock is written: its checksum does not match.
    SuperblockTorn,
    /// Pages and superblock are synced: the commit is durable.
    Durable,
}

#[cfg(test)]
pub(crate) type StageHook = Arc<dyn Fn(CommitStage) + Send + Sync>;

/// Sealed pages and superblock for one commit, built by [`LogWriter::stage`]
/// through `&self` so the live projection keeps serving readers while
/// [`LogWriter::write_staged`] writes and syncs them (#261, ADR 0040).
pub(crate) struct StagedCommit {
    pages: Vec<Vec<u8>>,
    superblock: Vec<u8>,
    committed: u32,
    fsyncs: u64,
    segment: Option<crate::objects::Segment>,
    manifest_version: u64,
}

/// Create or truncate the log at `path` for writing. The lock is taken
/// before truncation, so a create never clears a log another writer holds.
fn create_locked(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = crate::fault::open_rw_create(path)?;
    lock_writer(&file, path)?;
    crate::fault::set_len(&file, 0)?;
    Ok(file)
}

/// Take the exclusive writer lock on the open log `file`, failing at once
/// with [`crate::LOG_IN_USE`] when another writer holds it (#342). The OS
/// drops the lock when the file closes or the process dies, so a crashed
/// holder never blocks a later open. The lock is advisory and local to one
/// host: readers that never lock, such as backup, are unaffected, and it
/// does not fence writers on other machines sharing a network filesystem.
fn lock_writer(file: &File, path: &Path) -> Result<(), String> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(format!(
            "{}: {} is open for writing by another process or Store",
            crate::LOG_IN_USE,
            path.display()
        )),
        Err(TryLockError::Error(e)) => Err(format!("lock {}: {e}", path.display())),
    }
}

impl LogWriter {
    pub fn create(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        Self::create_manifest(path, sync, None, crate::objects::ObjectBackend::File)
    }

    pub fn create_with_keys(path: &Path, sync: SyncPolicy, keys: LogKeys) -> Result<Self, String> {
        Self::create_manifest(path, sync, Some(keys), crate::objects::ObjectBackend::File)
    }

    pub fn create_with_backend(
        path: &Path,
        sync: SyncPolicy,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::create_manifest(path, sync, None, backend)
    }

    pub fn create_with_keys_and_backend(
        path: &Path,
        sync: SyncPolicy,
        keys: LogKeys,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::create_manifest(path, sync, Some(keys), backend)
    }

    /// Superblock `MIKURAV2` log. Used by [`crate::Store::migrate_unnamespaced`].
    pub fn create_superblock(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let file = create_locked(path)?;
        let mut writer = Self::blank(file, sync, 0);
        writer.write_superblock()?;
        Ok(writer)
    }

    fn create_manifest(
        path: &Path,
        sync: SyncPolicy,
        keys: Option<LogKeys>,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        let file = create_locked(path)?;
        let objects =
            crate::objects::ObjectStore::create(&objects_dir(path), sync.durable(), &backend)?;
        let probe = objects.probe()?;
        let mut writer = Self::blank(file, sync, 0);
        writer.objects = Some(objects);
        writer.encrypted_log = keys.is_some();
        writer.fsync_count += probe;
        if let Some(keys) = keys {
            writer.install_keys(Arc::new(keys))?;
            writer.mint_store_dek()?;
        }
        writer.write_stub()?;
        writer.swap_snapshot()?;
        Ok(writer)
    }

    /// V4 log that keeps `catalog` instead of minting a new store key.
    pub(crate) fn create_manifest_with_catalog(
        path: &Path,
        sync: SyncPolicy,
        keys: LogKeys,
        catalog: BTreeMap<String, crate::crypto::WrappedDek>,
    ) -> Result<Self, String> {
        let file = create_locked(path)?;
        let objects = crate::objects::ObjectStore::create(
            &objects_dir(path),
            sync.durable(),
            &crate::objects::ObjectBackend::File,
        )?;
        let probe = objects.probe()?;
        let mut writer = Self::blank(file, sync, 0);
        writer.objects = Some(objects);
        writer.encrypted_log = true;
        writer.fsync_count += probe;
        writer.install_keys(Arc::new(keys))?;
        writer.load_manifest_catalog(&catalog)?;
        writer.write_stub()?;
        writer.swap_snapshot()?;
        Ok(writer)
    }

    pub(crate) fn append_copied_pages(&mut self, pages: Vec<Vec<u8>>) -> Result<(), String> {
        if pages.is_empty() {
            return Ok(());
        }
        self.pending_pages = pages;
        self.written = self.committed
            + u32::try_from(self.pending_pages.len())
                .map_err(|_| "too many copied pages".to_string())?;
        self.commit_pending()
    }

    pub fn open(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        Self::open_inner(path, sync, None, crate::objects::ObjectBackend::File, false)
    }

    pub fn open_with_keys(path: &Path, sync: SyncPolicy, keys: LogKeys) -> Result<Self, String> {
        Self::open_inner(
            path,
            sync,
            Some(keys),
            crate::objects::ObjectBackend::File,
            false,
        )
    }

    pub fn open_with_backend(
        path: &Path,
        sync: SyncPolicy,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::open_inner(path, sync, None, backend, false)
    }

    pub fn open_with_keys_and_backend(
        path: &Path,
        sync: SyncPolicy,
        keys: LogKeys,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::open_inner(path, sync, Some(keys), backend, false)
    }

    /// Open at the current snapshot without the writer lock, probe, or
    /// collection (ADR 0047 §5, §12).
    pub fn open_read_only(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        Self::open_inner(path, sync, None, crate::objects::ObjectBackend::File, true)
    }

    pub fn open_read_only_with_keys(
        path: &Path,
        sync: SyncPolicy,
        keys: LogKeys,
    ) -> Result<Self, String> {
        Self::open_inner(
            path,
            sync,
            Some(keys),
            crate::objects::ObjectBackend::File,
            true,
        )
    }

    pub fn open_read_only_with_backend(
        path: &Path,
        sync: SyncPolicy,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::open_inner(path, sync, None, backend, true)
    }

    pub fn open_read_only_with_keys_and_backend(
        path: &Path,
        sync: SyncPolicy,
        keys: LogKeys,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::open_inner(path, sync, Some(keys), backend, true)
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
            failed: AtomicBool::new(false),
            keys: None,
            catalog: BTreeMap::new(),
            deks: HashMap::new(),
            revoked: HashSet::new(),
            withheld: HashSet::new(),
            page_tenant: None,
            objects: None,
            manifest_version: 0,
            segments: Vec::new(),
            prefix_segments: Vec::new(),
            checkpoint_key: String::new(),
            projection_key: String::new(),
            prefix_key: String::new(),
            checkpoint_pages: 0,
            pending_pages: Vec::new(),
            encrypted_log: false,
            read_only: false,
            fenced: AtomicBool::new(false),
            #[cfg(test)]
            stage_hook: None,
            #[cfg(test)]
            crash_before_swap: false,
        }
    }

    fn open_inner(
        path: &Path,
        sync: SyncPolicy,
        keys: Option<LogKeys>,
        backend: crate::objects::ObjectBackend,
        read_only: bool,
    ) -> Result<Self, String> {
        let mut file = if read_only {
            crate::fault::open_read(path)?
        } else {
            crate::fault::open_rw(path)?
        };
        if !read_only {
            lock_writer(&file, path)?;
        }
        let mut superblock = vec![0u8; PAGE];
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        std::io::Read::read_exact(&mut file, &mut superblock).map_err(|e| e.to_string())?;
        let magic = superblock_magic(&superblock)?;
        if magic == V1_MAGIC {
            return Err(
                "bad magic: MIKURAV1 logs need Store::migrate_unnamespaced before open".into(),
            );
        }
        if magic == V4_MAGIC {
            return Self::open_manifest(file, path, &superblock, sync, keys, backend, read_only);
        }
        if read_only {
            return Err("read-only open requires a swap-committed log".into());
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

    fn open_manifest(
        file: File,
        path: &Path,
        stub: &[u8],
        sync: SyncPolicy,
        keys: Option<LogKeys>,
        backend: crate::objects::ObjectBackend,
        read_only: bool,
    ) -> Result<Self, String> {
        let encrypted = stub.get(STUB_FLAG_OFF).copied().unwrap_or(0) & STUB_ENCRYPTED != 0;
        if encrypted && keys.is_none() {
            return Err("encrypted log requires keys".into());
        }
        if !encrypted && keys.is_some() {
            return Err("log is not encrypted".into());
        }
        let objects =
            crate::objects::ObjectStore::open(&objects_dir(path), sync.durable(), &backend)?;
        let mut manifest = load_current_manifest(&objects, encrypted)?;
        // One writer (ADR 0047). Collection is a write: the swap fences a
        // displaced collector. The stub flock is local and does not cover
        // an HTTP namespace. A read-only opener never probes or collects.
        let collect_fsyncs = if read_only {
            0
        } else {
            collect_unreferenced_objects(&objects, &mut manifest, false)?
        };
        let probe = if read_only { 0 } else { objects.probe()? };
        let mut writer = Self::blank(file, sync, manifest.committed_pages);
        writer.objects = Some(objects);
        writer.manifest_version = manifest.version;
        writer.segments = std::mem::take(&mut manifest.segments);
        writer.prefix_segments = std::mem::take(&mut manifest.prefix_segments);
        writer.checkpoint_key = manifest.checkpoint_key;
        writer.projection_key = manifest.projection_key;
        writer.prefix_key = manifest.prefix_key;
        writer.checkpoint_pages = manifest.checkpoint_pages;
        writer.encrypted_log = encrypted;
        writer.read_only = read_only;
        writer.fsync_count += probe + collect_fsyncs;
        if let Some(keys) = keys {
            writer.install_keys(Arc::new(keys))?;
            if encrypted {
                writer.load_manifest_catalog(&manifest.catalog)?;
            } else {
                writer.mint_store_dek()?;
            }
        }
        Ok(writer)
    }

    fn load_manifest_catalog(
        &mut self,
        catalog: &std::collections::BTreeMap<String, crate::crypto::WrappedDek>,
    ) -> Result<(), String> {
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
            match keys.unwrap_status(wrapped)? {
                DekStatus::Ready(dek) => {
                    self.deks.insert(dek_key, dek);
                    self.catalog.insert(tenant.clone(), wrapped.clone());
                }
                DekStatus::Withheld => {
                    self.withheld.insert(dek_key);
                    self.catalog.insert(tenant.clone(), wrapped.clone());
                }
                DekStatus::Shredded => {
                    self.revoked.insert(dek_key);
                    self.catalog.insert(tenant.clone(), wrapped.clone());
                }
            }
        }
        if !self.deks.contains_key(STORE_KEY_TENANT) {
            return Err("encrypted log is missing a usable store key".into());
        }
        Ok(())
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

    /// Non-writing eligibility check used by stream admission so a tenant
    /// that cannot be encrypted is refused before the record enters a
    /// shared buffer (ADR 0040). Does not mint a DEK.
    pub(crate) fn check_tenant_writable_for(&self, record: &ObjectRecord) -> Result<(), String> {
        if self.keys.is_none() {
            return Ok(());
        }
        let tenant = tenant_of_namespace(&record.namespace)?;
        self.check_tenant_writable(tenant)
    }

    fn check_tenant_writable(&self, tenant: &str) -> Result<(), String> {
        if self.revoked.contains(tenant) {
            return Err("encrypted writes refuse revoked tenant".into());
        }
        if self.withheld.contains(tenant) {
            return Err("encrypted writes refuse tenant whose key is unavailable".into());
        }
        if self.deks.contains_key(tenant) {
            return Ok(());
        }
        if self.catalog.contains_key(tenant) {
            return Err("encrypted writes refuse tenant whose key is unavailable".into());
        }
        let Some(keys) = self.keys.as_ref() else {
            return Ok(());
        };
        if keys.grant_for(tenant).is_none() {
            return Err("encrypted writes refuse tenant without unwrap grant".into());
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

    pub fn storage_calls(&self) -> crate::objects::StorageCalls {
        self.objects
            .as_ref()
            .map(crate::objects::ObjectStore::storage_calls)
            .unwrap_or_default()
    }

    pub fn committed_pages(&self) -> u32 {
        self.committed
    }

    pub(crate) fn has_object_store(&self) -> bool {
        self.objects.is_some()
    }

    pub(crate) fn snapshot_version(&self) -> u64 {
        self.manifest_version
    }

    pub(crate) fn live_manifest(&self) -> Result<Option<crate::objects::Manifest>, String> {
        let Some(objects) = self.objects.as_ref() else {
            return Ok(None);
        };
        Ok(Some(load_current_manifest(objects, self.encrypted_log)?))
    }

    pub(crate) fn adopt_snapshot(&mut self, mut manifest: crate::objects::Manifest) {
        self.committed = manifest.committed_pages;
        self.written = manifest.committed_pages;
        self.synced = manifest.committed_pages;
        self.manifest_version = manifest.version;
        self.segments = std::mem::take(&mut manifest.segments);
        self.prefix_segments = std::mem::take(&mut manifest.prefix_segments);
        self.checkpoint_key = manifest.checkpoint_key;
        self.projection_key = manifest.projection_key;
        self.prefix_key = manifest.prefix_key;
        self.checkpoint_pages = manifest.checkpoint_pages;
    }

    /// Writer-start probe. Call once when promoting a read-only opener.
    pub(crate) fn probe_for_write(&mut self) -> Result<(), String> {
        let store = self
            .objects
            .as_ref()
            .ok_or_else(|| "promote requires a swap-committed log".to_string())?;
        self.fsync_count += store.probe()?;
        Ok(())
    }

    /// Compare-and-swap the current snapshot to fence the previous writer.
    /// `Ok(true)` this process owns the next version. `Ok(false)` another
    /// writer landed first; this opener stays read-only.
    pub(crate) fn claim_writer(&mut self) -> Result<bool, String> {
        if !self.read_only {
            return Ok(true);
        }
        let store = self
            .objects
            .as_ref()
            .ok_or_else(|| "promote requires a swap-committed log".to_string())?;
        let snapshot = self.snapshot_manifest();
        let bytes = crate::objects::encode_manifest(&snapshot)?;
        match store.cas(self.manifest_version, &bytes) {
            Ok((version, fsyncs)) => {
                self.manifest_version = version;
                self.fsync_count += fsyncs;
                self.read_only = false;
                Ok(true)
            }
            Err(err) if err.contains(crate::LOG_FENCED) => Ok(false),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn checkpoint_object_key(&self) -> &str {
        &self.checkpoint_key
    }

    pub(crate) fn projection_object_key(&self) -> &str {
        &self.projection_key
    }

    pub(crate) fn get_object(&self, key: &str) -> Result<Vec<u8>, String> {
        let store = self
            .objects
            .as_ref()
            .ok_or_else(|| "manifest log is missing its object namespace".to_string())?;
        store.get(key)
    }

    pub fn written_pages(&self) -> u32 {
        self.written
    }

    pub fn page_used(&self) -> usize {
        self.used
    }

    /// Release the writer lock while keeping the file open, as the OS does
    /// when the holding process dies.
    #[cfg(test)]
    pub(crate) fn release_lock(&self) {
        let _ = self.file.unlock();
    }

    /// True after a superblock write failed and left the commit point unknown,
    /// or after a rollback failed and left the live projection unreliable.
    pub fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub(crate) fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub(crate) fn is_fenced(&self) -> bool {
        if self.fenced.load(Ordering::Relaxed) {
            return true;
        }
        if self.read_only {
            return false;
        }
        matches!(
            self.live_manifest(),
            Ok(Some(manifest)) if manifest.version > self.manifest_version
        )
    }

    /// Refuse every further write until the log is reopened.
    pub fn poison(&mut self) {
        self.failed.store(true, Ordering::Relaxed);
    }

    fn note_cas_err(&self, err: &str) {
        self.failed.store(true, Ordering::Relaxed);
        if err.contains(crate::LOG_FENCED) {
            self.fenced.store(true, Ordering::Relaxed);
        }
    }

    pub(crate) fn ensure_usable(&self) -> Result<(), String> {
        if self.read_only {
            return Err("read-only store refuses writes".into());
        }
        if self.is_fenced() {
            self.fenced.store(true, Ordering::Relaxed);
            self.failed.store(true, Ordering::Relaxed);
            return Err(format!(
                "{}: writer lost the manifest swap",
                crate::LOG_FENCED
            ));
        }
        if self.is_failed() {
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
        self.pending_pages.clear();
        if self.objects.is_some() {
            return Ok(());
        }
        let end = PAGE as u64 * u64::from(1 + self.committed);
        crate::fault::set_len(&self.file, end)?;
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
        self.frame(record).map(drop)
    }

    /// Framed bytes and, on an encrypted log, the page tenant of `record`.
    /// Fails closed when the record cannot fit on one page.
    fn frame(&self, record: &ObjectRecord) -> Result<(Vec<u8>, Option<String>), String> {
        let framed = framed_record(&encode_body(record)?)?;
        let tenant = if self.keys.is_some() {
            Some(tenant_of_namespace(&record.namespace)?.to_string())
        } else {
            None
        };
        if framed.len() > self.page_capacity(tenant.as_deref()) {
            return Err("record larger than a page".into());
        }
        Ok((framed, tenant))
    }

    /// A record of `len` framed bytes for `tenant` cannot join a page that
    /// already holds `used` bytes for `page_tenant`: an encrypted page holds
    /// one tenant, and every page holds at most its capacity.
    fn starts_new_page(
        &self,
        used: usize,
        page_tenant: Option<&str>,
        len: usize,
        tenant: Option<&str>,
    ) -> bool {
        used > 0
            && ((tenant.is_some() && page_tenant != tenant)
                || used + len > self.page_capacity(tenant))
    }

    pub fn append_record(&mut self, record: &ObjectRecord) -> Result<(), String> {
        self.ensure_usable()?;
        let (framed, tenant) = self.frame(record)?;
        if let Some(tenant) = tenant.as_deref() {
            self.ensure_tenant_dek(tenant)?;
        }
        if self.starts_new_page(
            self.used,
            self.page_tenant.as_deref(),
            framed.len(),
            tenant.as_deref(),
        ) {
            self.flush_page()?;
        }
        if self.used == 0 {
            self.page_tenant = tenant;
        }
        let start = PAGE_HDR + self.used;
        self.page[start..start + framed.len()].copy_from_slice(&framed);
        self.used += framed.len();
        Ok(())
    }

    /// True when [`Self::revalidate_catalog`] would change nothing, so a
    /// commit staged through `&self` sees the same key state as one that
    /// revalidates first. An unwrap error counts as a change.
    fn catalog_is_current(&self) -> bool {
        let Some(keys) = self.keys.as_ref() else {
            return true;
        };
        self.catalog.iter().all(|(tenant, wrapped)| {
            let name = if tenant.is_empty() {
                STORE_KEY_TENANT
            } else {
                tenant.as_str()
            };
            match keys.unwrap_status(wrapped) {
                Ok(DekStatus::Ready(_)) => true,
                Ok(DekStatus::Withheld) => {
                    self.withheld.contains(name) && !self.deks.contains_key(name)
                }
                Ok(DekStatus::Shredded) => {
                    name != STORE_KEY_TENANT
                        && self.revoked.contains(name)
                        && !self.deks.contains_key(name)
                }
                Err(_) => false,
            }
        })
    }

    /// Seal `records` into the pages and superblock of one commit after the
    /// current commit point, without changing the writer.
    ///
    /// `None` means the commit needs the exclusive path: the writer has
    /// pending or held pages, a key would be minted, or revalidating the key
    /// catalog would change it. Records must already be admitted by the store.
    pub(crate) fn stage(&self, records: &[ObjectRecord]) -> Option<StagedCommit> {
        if self.ensure_usable().is_err()
            || self.has_pending()
            || self.hold_commits
            || records.is_empty()
            || !self.catalog_is_current()
        {
            return None;
        }
        let mut pages = Vec::new();
        let mut page = vec![0u8; PAGE];
        let mut used = 0;
        let mut page_tenant: Option<String> = None;
        for record in records {
            let (framed, tenant) = self.frame(record).ok()?;
            if let Some(tenant) = tenant.as_deref() {
                let ready = self.deks.contains_key(tenant)
                    && !self.revoked.contains(tenant)
                    && !self.withheld.contains(tenant);
                if !ready {
                    return None;
                }
            }
            if self.starts_new_page(
                used,
                page_tenant.as_deref(),
                framed.len(),
                tenant.as_deref(),
            ) {
                let page_no = self.written + 1 + pages.len() as u32;
                pages.push(
                    self.seal_page_bytes(&mut page, used, page_tenant.as_deref(), page_no)
                        .ok()?,
                );
                page = vec![0u8; PAGE];
                used = 0;
            }
            if used == 0 {
                page_tenant = tenant;
            }
            page[PAGE_HDR + used..PAGE_HDR + used + framed.len()].copy_from_slice(&framed);
            used += framed.len();
        }
        let page_no = self.written + 1 + pages.len() as u32;
        pages.push(
            self.seal_page_bytes(&mut page, used, page_tenant.as_deref(), page_no)
                .ok()?,
        );
        let committed = self.written + pages.len() as u32;
        let superblock = if self.objects.is_some() {
            Vec::new()
        } else {
            self.superblock_bytes(committed).ok()?
        };
        Some(StagedCommit {
            pages,
            superblock,
            committed,
            fsyncs: 0,
            segment: None,
            manifest_version: 0,
        })
    }

    /// Write and sync a staged commit through `&self`, with the group-sync
    /// steps of a held [`Self::flush`]: pages, a data sync, then the
    /// superblock and its sync. Only the caller that holds the store's
    /// single-writer lock may call this, and the writer state stays at the
    /// old commit point until [`Self::adopt_staged`].
    ///
    /// A failure before the superblock write truncates the new pages, so the
    /// log is unchanged. A failed superblock write leaves the commit point
    /// unknown and poisons the writer, as [`Self::flush`] does.
    pub(crate) fn write_staged(&self, staged: &mut StagedCommit) -> Result<(), String> {
        self.ensure_usable()?;
        if self.objects.is_some() {
            return self.write_staged_manifest(staged);
        }
        let mut file = &self.file;
        let start = PAGE as u64 * u64::from(1 + self.written);
        let mut fsyncs = 0;
        let pages = (|| -> std::io::Result<()> {
            file.seek(SeekFrom::Start(start))?;
            let mut written = self.written;
            let mut synced = self.committed;
            for page in &staged.pages {
                crate::fault::write_all(&mut file, page).map_err(io_err)?;
                written += 1;
                if self.sync.durable() && written - synced >= self.sync.group_size() {
                    crate::fault::sync_data(file).map_err(io_err)?;
                    fsyncs += 1;
                    synced = written;
                }
            }
            #[cfg(test)]
            self.run_stage_hook(CommitStage::PagesWritten);
            if self.sync.durable() {
                crate::fault::sync_data(file).map_err(io_err)?;
                fsyncs += 1;
            }
            Ok(())
        })();
        if let Err(error) = pages {
            if crate::fault::crashed() {
                return Err(error.to_string());
            }
            let truncated = crate::fault::set_len(file, start)
                .map_err(io_err)
                .and_then(|()| file.seek(SeekFrom::Start(start)).map(drop));
            if let Err(truncate) = truncated {
                self.failed.store(true, Ordering::Relaxed);
                return Err(format!("{UNCERTAIN_COMMIT}: {error}; {truncate}"));
            }
            return Err(error.to_string());
        }
        let end = PAGE as u64 * u64::from(1 + staged.committed);
        let superblock = (|| -> std::io::Result<()> {
            file.seek(SeekFrom::Start(0))?;
            // Half a superblock on disk, as a short write would leave it.
            let (head, tail) = staged.superblock.split_at(PAGE / 2);
            crate::fault::write_all(&mut file, head).map_err(io_err)?;
            #[cfg(test)]
            self.run_stage_hook(CommitStage::SuperblockTorn);
            crate::fault::write_all(&mut file, tail).map_err(io_err)?;
            if self.sync.durable() {
                crate::fault::sync_data(file).map_err(io_err)?;
                fsyncs += 1;
            }
            file.seek(SeekFrom::Start(end)).map(drop)
        })();
        if let Err(error) = superblock {
            self.failed.store(true, Ordering::Relaxed);
            return Err(format!("{UNCERTAIN_COMMIT}: {error}"));
        }
        staged.fsyncs = fsyncs;
        #[cfg(test)]
        self.run_stage_hook(CommitStage::Durable);
        Ok(())
    }

    /// Committed records on pages `1..=committed_pages` of the log at `path`
    /// that `keep` accepts, with the page that holds each, in log order
    /// (ADR 0036). Every commit ends on a page boundary, so a page that a
    /// commit returned as `committed_pages` is an exact commit point.
    ///
    /// The commit point, format, and tenant keys come from this writer, not
    /// from the superblock on disk. A split commit may be rewriting that
    /// superblock while a reader holds the store; pages up to the applied
    /// commit point never change, so this read never sees a torn superblock
    /// or a commit that is not yet applied (#261). A tenant whose key is no
    /// longer usable is skipped, as a fresh catalog unwrap would.
    pub(crate) fn read_applied_pages(
        &self,
        path: &Path,
        mut keep: impl FnMut(&ObjectRecord) -> bool,
    ) -> Result<Vec<(u32, ObjectRecord)>, String> {
        let deks = self.keys.is_some().then(|| {
            self.deks
                .iter()
                .filter(|(tenant, _)| self.tenant_key_usable(tenant))
                .map(|(tenant, dek)| (tenant.clone(), dek.clone()))
                .collect::<HashMap<_, _>>()
        });
        let mut file = crate::fault::open_read(path)?;
        let mut paged = Vec::new();
        if self.objects.is_some() {
            visit_manifest_pages(
                &objects_dir(path),
                ManifestCover {
                    prefix: &self.prefix_segments,
                    tail: &self.segments,
                    checkpoint_pages: self.checkpoint_pages,
                    committed: self.committed,
                },
                0,
                deks.as_ref(),
                |index, records| {
                    paged.extend(
                        records
                            .into_iter()
                            .filter(|record| keep(record))
                            .map(|record| (index, record)),
                    );
                },
            )?;
            return Ok(paged);
        }
        let needed = PAGE as u64 * u64::from(1 + self.committed);
        if file.metadata().map_err(|e| e.to_string())?.len() < needed {
            return Err("committed pages missing".into());
        }
        visit_pages(
            &mut file,
            None,
            0..=self.committed,
            deks.as_ref(),
            |index, records| {
                paged.extend(
                    records
                        .into_iter()
                        .filter(|record| keep(record))
                        .map(|record| (index, record)),
                );
            },
        )?;
        Ok(paged)
    }

    /// Committed records on pages `(after_page + 1)..=committed_pages` of the
    /// snapshot this writer has adopted, not a later snapshot on disk.
    pub(crate) fn records_after(
        &self,
        path: &Path,
        after_page: u32,
    ) -> Result<Vec<ObjectRecord>, String> {
        if self.objects.is_none() {
            return read_records_after_with_keys(path, after_page, self.keys());
        }
        let deks = self.keys.is_some().then(|| {
            self.deks
                .iter()
                .filter(|(tenant, _)| self.tenant_key_usable(tenant))
                .map(|(tenant, dek)| (tenant.clone(), dek.clone()))
                .collect::<HashMap<_, _>>()
        });
        let mut records = Vec::new();
        visit_manifest_pages(
            &objects_dir(path),
            ManifestCover {
                prefix: &self.prefix_segments,
                tail: &self.segments,
                checkpoint_pages: self.checkpoint_pages,
                committed: self.committed,
            },
            after_page,
            deks.as_ref(),
            |_, page| records.extend(page),
        )?;
        Ok(records)
    }

    /// Collect unreferenced objects under this writer's authority (ADR 0047 §11).
    ///
    /// The successor snapshot is the owner record. A process that loses the
    /// swap refuses. Staged objects of a still-live writer stay until that
    /// writer is fenced.
    pub(crate) fn collect_unreferenced(&mut self) -> Result<(), String> {
        self.ensure_usable()?;
        if self.has_pending() {
            return Err("uncommitted pages; commit first".into());
        }
        let Some(objects) = self.objects.as_ref() else {
            return Ok(());
        };
        let mut snapshot = self.snapshot_manifest();
        let collect = collect_unreferenced_objects(objects, &mut snapshot, true);
        // Adopt the claimed version even when a later delete fails, so the
        // next commit does not CAS against our own successor and self-fence.
        self.manifest_version = snapshot.version;
        self.fsync_count += collect?;
        Ok(())
    }

    /// Move the writer to a commit [`Self::write_staged`] made durable.
    pub(crate) fn adopt_staged(&mut self, staged: &StagedCommit) {
        self.written = staged.committed;
        self.committed = staged.committed;
        self.synced = staged.committed;
        self.fsync_count += staged.fsyncs;
        if let Some(segment) = staged.segment.clone() {
            self.segments.push(segment);
            self.manifest_version = staged.manifest_version;
            self.pending_pages.clear();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_crash_before_swap(&mut self, crash: bool) {
        self.crash_before_swap = crash;
    }

    #[cfg(test)]
    pub(crate) fn set_stage_hook(&mut self, hook: Option<StageHook>) {
        self.stage_hook = hook;
    }

    #[cfg(test)]
    fn run_stage_hook(&self, stage: CommitStage) {
        if let Some(hook) = &self.stage_hook {
            hook(stage);
        }
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
        let mut page = std::mem::take(&mut self.page);
        let sealed = self.seal_page_bytes(
            &mut page,
            self.used,
            self.page_tenant.as_deref(),
            self.written + 1,
        );
        self.page = page;
        let sealed = sealed?;
        if self.objects.is_some() {
            self.pending_pages.push(sealed);
        } else {
            crate::fault::write_all(&mut self.file, &sealed)?;
        }
        self.written += 1;
        self.page = vec![0u8; PAGE];
        self.used = 0;
        self.page_tenant = None;
        if self.hold_commits {
            if self.objects.is_none()
                && self.sync.durable()
                && self.written - self.synced >= self.sync.group_size()
            {
                self.sync_data()?;
                self.synced = self.written;
            }
            return Ok(());
        }
        if self.sync == SyncPolicy::None {
            return self.commit_pending();
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
        if self.objects.is_some() {
            if let Some(segment) = self.put_pending_segment()? {
                #[cfg(test)]
                {
                    self.run_stage_hook(CommitStage::PagesWritten);
                    if self.crash_before_swap {
                        return Err("crash before swap".into());
                    }
                    self.run_stage_hook(CommitStage::SuperblockTorn);
                }
                self.segments.push(segment);
            }
            let committed = self.written;
            self.committed = committed;
            self.synced = committed;
            self.swap_snapshot()?;
            #[cfg(test)]
            self.run_stage_hook(CommitStage::Durable);
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
            self.failed.store(true, Ordering::Relaxed);
            format!("{UNCERTAIN_COMMIT}: {error}")
        })
    }

    /// Disk bytes of data page `page_no` holding `used` bytes of `page`.
    /// An encrypted page seals for `tenant`.
    fn seal_page_bytes(
        &self,
        page: &mut [u8],
        used: usize,
        tenant: Option<&str>,
        page_no: u32,
    ) -> Result<Vec<u8>, String> {
        page[CRC_LEN..PAGE_HDR].copy_from_slice(&(used as u16).to_le_bytes());
        if self.keys.is_none() {
            let mut disk = page.to_vec();
            seal_page(&mut disk);
            return Ok(disk);
        }
        let tenant = tenant.ok_or_else(|| "encrypted page is missing tenant".to_string())?;
        let dek = self
            .deks
            .get(tenant)
            .ok_or_else(|| "encrypted page is missing tenant key".to_string())?;
        let inner_len = PAGE
            .checked_sub(CRC_LEN + encrypted_overhead(tenant.len()))
            .ok_or_else(|| "encrypted page overhead exceeds page".to_string())?;
        let mut inner = vec![0u8; inner_len];
        inner[..USED_LEN].copy_from_slice(&page[CRC_LEN..PAGE_HDR]);
        inner[USED_LEN..USED_LEN + used].copy_from_slice(&page[PAGE_HDR..PAGE_HDR + used]);
        let sealed = seal_page_body(dek, tenant, page_no, &inner)?;
        if CRC_LEN + sealed.len() > PAGE {
            return Err("encrypted page overflow".into());
        }
        let mut disk = vec![0u8; PAGE];
        disk[CRC_LEN..CRC_LEN + sealed.len()].copy_from_slice(&sealed);
        seal_page(&mut disk);
        Ok(disk)
    }

    fn superblock_bytes(&self, committed: u32) -> Result<Vec<u8>, String> {
        if self.keys.is_some() {
            superblock_page_v3(committed, &self.catalog)
        } else {
            Ok(superblock_page(committed))
        }
    }

    fn write_stub(&mut self) -> Result<(), String> {
        let stub = stub_page(self.encrypted_log);
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        crate::fault::write_all(&mut self.file, &stub)?;
        self.sync_data()?;
        Ok(())
    }

    fn snapshot_manifest(&self) -> crate::objects::Manifest {
        crate::objects::Manifest {
            version: self.manifest_version,
            committed_pages: self.committed,
            encrypted: self.encrypted_log,
            segments: self.segments.clone(),
            catalog: self.catalog.clone(),
            checkpoint_key: self.checkpoint_key.clone(),
            projection_key: self.projection_key.clone(),
            prefix_key: self.prefix_key.clone(),
            checkpoint_pages: self.checkpoint_pages,
            // Not encoded (they live in `prefix_key`); kept so live_keys
            // protects frozen coverage during collect.
            prefix_segments: self.prefix_segments.clone(),
        }
    }

    /// Freeze covered segments into a prefix object and name derived
    /// checkpoint and projection objects. Puts run as one wave, then one CAS.
    /// Tail after this is empty.
    pub(crate) fn publish_checkpoint(
        &mut self,
        checkpoint_pages: u32,
        checkpoint: &[u8],
        projection: Option<&[u8]>,
    ) -> Result<(), String> {
        if self.objects.is_none() {
            return Ok(());
        }
        if checkpoint_pages == 0 {
            return Ok(());
        }
        let store = self
            .objects
            .as_ref()
            .ok_or_else(|| "manifest log is missing its object namespace".to_string())?;
        let frozen: Vec<_> = self
            .prefix_segments
            .iter()
            .cloned()
            .chain(self.segments.iter().cloned())
            .collect();
        let prefix_bytes = encode_prefix(&frozen, checkpoint_pages)?;
        let (prefix_key, mut fsyncs) = put_unique_object(
            store,
            "prefix",
            self.manifest_version,
            checkpoint_pages,
            &prefix_bytes,
        )?;
        let (checkpoint_key, ckpt_fsyncs) = put_unique_object(
            store,
            "ckpt",
            self.manifest_version,
            checkpoint_pages,
            checkpoint,
        )?;
        fsyncs += ckpt_fsyncs;
        if let Some(projection) = projection {
            let (projection_key, proj_fsyncs) = put_unique_object(
                store,
                "proj",
                self.manifest_version,
                checkpoint_pages,
                projection,
            )?;
            self.projection_key = projection_key;
            fsyncs += proj_fsyncs;
        }
        self.prefix_segments = frozen;
        self.segments.clear();
        self.prefix_key = prefix_key;
        self.checkpoint_key = checkpoint_key;
        self.checkpoint_pages = checkpoint_pages;
        self.fsync_count += fsyncs;
        self.swap_snapshot()
    }

    fn swap_snapshot(&mut self) -> Result<(), String> {
        let Some(store) = self.objects.as_ref() else {
            return Ok(());
        };
        let mut snapshot = self.snapshot_manifest();
        let bytes = crate::objects::encode_manifest(&snapshot)?;
        let (version, fsyncs) = store
            .cas(self.manifest_version, &bytes)
            .inspect_err(|err| {
                self.note_cas_err(err);
            })?;
        snapshot.version = version;
        self.manifest_version = version;
        self.fsync_count += fsyncs;
        Ok(())
    }

    fn put_pending_segment(&mut self) -> Result<Option<crate::objects::Segment>, String> {
        if self.pending_pages.is_empty() {
            return Ok(None);
        }
        let store = self
            .objects
            .as_ref()
            .ok_or_else(|| "manifest log is missing its object namespace".to_string())?;
        let first_page = self.committed + 1;
        let page_count = u32::try_from(self.pending_pages.len())
            .map_err(|_| "too many pages in one segment".to_string())?;
        let mut bytes = Vec::with_capacity(self.pending_pages.len() * PAGE);
        for page in &self.pending_pages {
            bytes.extend_from_slice(page);
        }
        let (segment, fsyncs) = self.put_segment_object(store, first_page, page_count, &bytes)?;
        self.fsync_count += fsyncs;
        self.pending_pages.clear();
        Ok(Some(segment))
    }

    /// Live coverage is `first_page`; the suffix is unique per put so a
    /// leftover object never fences this writer. The snapshot CAS is the ack.
    fn put_segment_object(
        &self,
        store: &crate::objects::ObjectStore,
        first_page: u32,
        page_count: u32,
        bytes: &[u8],
    ) -> Result<(crate::objects::Segment, u64), String> {
        for _ in 0..8 {
            let key = crate::objects::unique_object_key("seg", self.manifest_version, first_page);
            match store.put(&key, bytes) {
                Ok(fsyncs) => {
                    return Ok((
                        crate::objects::Segment {
                            key,
                            first_page,
                            page_count,
                        },
                        fsyncs,
                    ));
                }
                Err(error) if error.contains("already exists") => continue,
                Err(error) => return Err(error),
            }
        }
        Err("could not allocate a unique segment key".into())
    }

    fn write_staged_manifest(&self, staged: &mut StagedCommit) -> Result<(), String> {
        let store = self
            .objects
            .as_ref()
            .ok_or_else(|| "manifest log is missing its object namespace".to_string())?;
        let first_page = self.committed + 1;
        let page_count = u32::try_from(staged.pages.len())
            .map_err(|_| "too many pages in one segment".to_string())?;
        let mut bytes = Vec::with_capacity(staged.pages.len() * PAGE);
        for page in &staged.pages {
            bytes.extend_from_slice(page);
        }
        let (segment, mut fsyncs) =
            self.put_segment_object(store, first_page, page_count, &bytes)?;
        #[cfg(test)]
        {
            self.run_stage_hook(CommitStage::PagesWritten);
            if self.crash_before_swap {
                return Err("crash before swap".into());
            }
            self.run_stage_hook(CommitStage::SuperblockTorn);
        }
        let mut snapshot = self.snapshot_manifest();
        snapshot.segments.push(segment.clone());
        snapshot.committed_pages = staged.committed;
        let encoded = crate::objects::encode_manifest(&snapshot)?;
        let (version, cas_fsyncs) =
            store
                .cas(self.manifest_version, &encoded)
                .inspect_err(|err| {
                    self.note_cas_err(err);
                })?;
        fsyncs += cas_fsyncs;
        staged.fsyncs = fsyncs;
        staged.segment = Some(segment);
        staged.manifest_version = version;
        #[cfg(test)]
        self.run_stage_hook(CommitStage::Durable);
        Ok(())
    }

    fn write_superblock_body(&mut self) -> Result<(), String> {
        let superblock = self.superblock_bytes(self.committed)?;
        let pos = self.file.stream_position().map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        crate::fault::write_all(&mut self.file, &superblock)?;
        self.sync_data()?;
        self.file
            .seek(SeekFrom::Start(pos.max(PAGE as u64)))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn sync_data(&mut self) -> Result<(), String> {
        if self.sync.durable() {
            crate::fault::sync_data(&self.file)?;
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
    visit: impl FnMut(u32, Vec<ObjectRecord>),
) -> Result<(), String> {
    let mut file = crate::fault::open_read(path)?;
    let mut superblock = vec![0u8; PAGE];
    file.read_exact(&mut superblock)
        .map_err(|_| "missing superblock".to_string())?;
    let magic = if v1_namespace.is_some() {
        V1_MAGIC
    } else {
        superblock_magic(&superblock)?
    };
    if magic == V4_MAGIC {
        if v1_namespace.is_some() {
            return Err("bad magic".into());
        }
        return visit_committed_manifest_pages(path, &superblock, after_page, keys, visit);
    }
    #[allow(unused_mut)]
    let mut committed = match v1_namespace {
        None => parse_superblock(&superblock)?,
        Some(_) => parse_v1_superblock(&superblock)?,
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
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    #[cfg(test)]
    {
        committed = crate::fault::maybe_orphan_pages(committed, len, PAGE);
    }
    let needed = PAGE as u64 * u64::from(1 + committed);
    if len < needed {
        return Err("committed pages missing".into());
    }
    if after_page > committed {
        return Err("checkpoint page is past committed_pages".into());
    }
    visit_pages(
        &mut file,
        v1_namespace,
        after_page..=committed,
        deks.as_ref(),
        visit,
    )
}

fn visit_committed_manifest_pages(
    path: &Path,
    stub: &[u8],
    after_page: u32,
    keys: Option<&LogKeys>,
    visit: impl FnMut(u32, Vec<ObjectRecord>),
) -> Result<(), String> {
    let encrypted = stub.get(STUB_FLAG_OFF).copied().unwrap_or(0) & STUB_ENCRYPTED != 0;
    if encrypted && keys.is_none() {
        return Err("encrypted log requires keys".into());
    }
    let store = crate::objects::ObjectStore::open(
        &objects_dir(path),
        false,
        &crate::objects::ObjectBackend::File,
    )?;
    let manifest = load_current_manifest(&store, encrypted)?;
    if after_page > manifest.committed_pages {
        return Err("checkpoint page is past committed_pages".into());
    }
    let deks = if encrypted {
        Some(unwrap_manifest_catalog(
            keys.ok_or("encrypted log requires keys")?,
            &manifest,
        )?)
    } else {
        None
    };
    visit_manifest_pages(
        &objects_dir(path),
        ManifestCover {
            prefix: &manifest.prefix_segments,
            tail: &manifest.segments,
            checkpoint_pages: manifest.checkpoint_pages,
            committed: manifest.committed_pages,
        },
        after_page,
        deks.as_ref(),
        visit,
    )
}

struct ManifestCover<'a> {
    prefix: &'a [crate::objects::Segment],
    tail: &'a [crate::objects::Segment],
    checkpoint_pages: u32,
    committed: u32,
}

fn unwrap_manifest_catalog(
    keys: &LogKeys,
    manifest: &crate::objects::Manifest,
) -> Result<HashMap<String, Dek>, String> {
    let mut deks = HashMap::new();
    for (tenant, wrapped) in &manifest.catalog {
        if let Some(dek) = keys.unwrap_dek(wrapped)? {
            let dek_key = if tenant.is_empty() {
                STORE_KEY_TENANT.to_string()
            } else {
                tenant.clone()
            };
            deks.insert(dek_key, dek);
        }
    }
    Ok(deks)
}

fn visit_manifest_pages(
    root: &Path,
    cover: ManifestCover<'_>,
    after_page: u32,
    deks: Option<&HashMap<String, Dek>>,
    mut visit: impl FnMut(u32, Vec<ObjectRecord>),
) -> Result<(), String> {
    let store =
        crate::objects::ObjectStore::open(root, false, &crate::objects::ObjectBackend::File)?;
    ensure_manifest_pages(
        &store,
        cover.prefix,
        cover.tail,
        cover.checkpoint_pages,
        after_page,
        cover.committed,
    )?;
    if after_page >= cover.committed {
        return Ok(());
    }
    let committed = cover.committed;
    let segments: Vec<&crate::objects::Segment> = if after_page >= cover.checkpoint_pages {
        cover.tail.iter().collect()
    } else {
        cover.prefix.iter().chain(cover.tail.iter()).collect()
    };
    for segment in segments {
        let last = segment.first_page + segment.page_count - 1;
        if last <= after_page || segment.first_page > committed {
            continue;
        }
        let bytes = store
            .get(&segment.key)
            .map_err(|_| "committed pages missing".to_string())?;
        let expected = PAGE * segment.page_count as usize;
        if bytes.len() != expected {
            return Err("committed pages missing".into());
        }
        for offset in 0..segment.page_count {
            let index = segment.first_page + offset;
            if index <= after_page || index > committed {
                continue;
            }
            let start = offset as usize * PAGE;
            let page = &bytes[start..start + PAGE];
            verify_page(page).map_err(|e| format!("committed page {index}: {e}"))?;
            let mut records = Vec::new();
            if let Some(deks) = deks {
                decode_encrypted_page(page, index, deks, &mut records)
                    .map_err(|e| format!("committed page {index}: {e}"))?;
            } else {
                decode_data_page(page, None, &mut records)?;
            }
            visit(index, records);
        }
    }
    Ok(())
}

/// Pages after a named checkpoint are the tail. Frozen segments are one
/// prefix object, not a per-segment `object_len` at open (ADR 0047 §10).
fn ensure_manifest_pages(
    store: &crate::objects::ObjectStore,
    prefix: &[crate::objects::Segment],
    tail: &[crate::objects::Segment],
    checkpoint_pages: u32,
    after_page: u32,
    committed: u32,
) -> Result<(), String> {
    if committed == 0 {
        return Ok(());
    }
    if after_page >= checkpoint_pages {
        let start = checkpoint_pages;
        return ensure_span(store, tail, start, committed);
    }
    ensure_span(store, prefix, 0, checkpoint_pages)?;
    ensure_span(store, tail, checkpoint_pages, committed)
}

fn ensure_span(
    store: &crate::objects::ObjectStore,
    segments: &[crate::objects::Segment],
    covered: u32,
    committed: u32,
) -> Result<(), String> {
    if covered == committed {
        return Ok(());
    }
    let mut covered = covered;
    for segment in segments {
        let expected = PAGE as u64 * u64::from(segment.page_count);
        if store.object_len(&segment.key)? != expected {
            return Err("committed pages missing".into());
        }
        if segment.first_page != covered + 1 {
            return Err("committed pages missing".into());
        }
        covered = segment.first_page + segment.page_count - 1;
    }
    if covered != committed {
        return Err("committed pages missing".into());
    }
    Ok(())
}

fn put_unique_object(
    store: &crate::objects::ObjectStore,
    kind: &str,
    writer_version: u64,
    stamp: u32,
    bytes: &[u8],
) -> Result<(String, u64), String> {
    for _ in 0..8 {
        let key = crate::objects::unique_object_key(kind, writer_version, stamp);
        match store.put(&key, bytes) {
            Ok(fsyncs) => return Ok((key, fsyncs)),
            Err(error) if error.contains("already exists") => continue,
            Err(error) => return Err(error),
        }
    }
    Err("could not allocate unique snapshot object keys".into())
}

fn encode_prefix(
    segments: &[crate::objects::Segment],
    checkpoint_pages: u32,
) -> Result<Vec<u8>, String> {
    let mut prefix = crate::objects::Manifest::empty(false);
    prefix.committed_pages = checkpoint_pages;
    prefix.segments = segments.to_vec();
    crate::objects::encode_manifest(&prefix)
}

fn decode_prefix(bytes: &[u8]) -> Result<Vec<crate::objects::Segment>, String> {
    Ok(crate::objects::decode_manifest(bytes)?.segments)
}

/// Committed page `index` of a log, V2/V3 file or V4 segment.
pub(crate) fn read_committed_page(path: &Path, index: u32) -> Result<Vec<u8>, String> {
    if index == 0 {
        let mut file = crate::fault::open_read(path)?;
        let mut page = vec![0u8; PAGE];
        file.read_exact(&mut page)
            .map_err(|_| "missing superblock".to_string())?;
        verify_page(&page).map_err(|e| format!("superblock: {e}"))?;
        return Ok(page);
    }
    let mut file = crate::fault::open_read(path)?;
    let mut stub = vec![0u8; PAGE];
    file.read_exact(&mut stub)
        .map_err(|_| "missing superblock".to_string())?;
    if is_manifest_stub(&stub) {
        return read_manifest_page(path, &stub, index);
    }
    let committed = parse_superblock(&stub)?;
    if index > committed {
        return Err(format!("page {index} is not committed"));
    }
    file.seek(SeekFrom::Start(PAGE as u64 * u64::from(index)))
        .map_err(|e| e.to_string())?;
    let mut page = vec![0u8; PAGE];
    file.read_exact(&mut page)
        .map_err(|_| format!("committed page {index}: short read"))?;
    verify_page(&page).map_err(|e| format!("committed page {index}: {e}"))?;
    Ok(page)
}

fn read_manifest_page(path: &Path, stub: &[u8], index: u32) -> Result<Vec<u8>, String> {
    let store = crate::objects::ObjectStore::open(
        &objects_dir(path),
        false,
        &crate::objects::ObjectBackend::File,
    )?;
    let encrypted = stub.get(STUB_FLAG_OFF).copied().unwrap_or(0) & STUB_ENCRYPTED != 0;
    let manifest = load_current_manifest(&store, encrypted)?;
    let (segment, offset) = manifest.page_span(index)?;
    let bytes = store
        .get(&segment.key)
        .map_err(|_| "committed pages missing".to_string())?;
    let start = offset as usize * PAGE;
    let end = start + PAGE;
    if bytes.len() < end {
        return Err("committed pages missing".into());
    }
    let page = bytes[start..end].to_vec();
    verify_page(&page).map_err(|e| format!("committed page {index}: {e}"))?;
    Ok(page)
}

pub(crate) fn manifest_commit_point(path: &Path) -> Result<Option<u32>, String> {
    let mut file = crate::fault::open_read(path)?;
    let mut stub = vec![0u8; PAGE];
    file.read_exact(&mut stub)
        .map_err(|_| "missing superblock".to_string())?;
    if !is_manifest_stub(&stub) {
        return Ok(None);
    }
    let store = crate::objects::ObjectStore::open(
        &objects_dir(path),
        false,
        &crate::objects::ObjectBackend::File,
    )?;
    let encrypted = stub.get(STUB_FLAG_OFF).copied().unwrap_or(0) & STUB_ENCRYPTED != 0;
    Ok(Some(
        load_current_manifest(&store, encrypted)?.committed_pages,
    ))
}

/// Swap a successor snapshot (the maintenance owner), then delete objects
/// the new snapshot does not name whose stager is fenced. `required` fails
/// when the swap is refused; open-for-write still collects already-fenced
/// leftovers and leaves the log open.
///
/// The successor body matches the current snapshot. Exclusive create (file)
/// and 412 (HTTP) fence a second collector before any byte compare. A lost
/// swap response that reads back identical bytes is the same owner and the
/// same delete set.
fn collect_unreferenced_objects(
    objects: &crate::objects::ObjectStore,
    manifest: &mut crate::objects::Manifest,
    required: bool,
) -> Result<u64, String> {
    let encoded = crate::objects::encode_manifest(manifest)?;
    let (version, fsyncs) = match objects.cas(manifest.version, &encoded) {
        Ok(claimed) => claimed,
        Err(err) if !required && err.contains(crate::LOG_FENCED) => {
            // Not the owner. Do not delete the winner's successor snapshot.
            return Ok(0);
        }
        Err(err) => return Err(err),
    };
    manifest.version = version;
    let live = manifest.live_keys();
    let claimed = version;
    let still_owner = || crate::objects::owns_manifest_version(objects, claimed);
    if required {
        objects.collect(&live, version, &still_owner)?;
    } else {
        // Open must succeed if a collector was killed mid-delete; rerun finishes.
        let _ = objects.collect(&live, version, &still_owner);
    }
    Ok(fsyncs)
}

fn load_current_manifest(
    store: &crate::objects::ObjectStore,
    encrypted: bool,
) -> Result<crate::objects::Manifest, String> {
    match store.current()? {
        Some((version, bytes)) => {
            let mut parsed = crate::objects::decode_manifest(&bytes)?;
            parsed.version = version;
            if parsed.encrypted != encrypted {
                return Err("manifest encryption flag does not match the stub".into());
            }
            if !parsed.prefix_key.is_empty() {
                let prefix = store.get(&parsed.prefix_key)?;
                parsed.prefix_segments = decode_prefix(&prefix)?;
            }
            Ok(parsed)
        }
        None => {
            if store.has_durable_objects()? {
                return Err("committed pages missing".into());
            }
            Ok(crate::objects::Manifest::empty(encrypted))
        }
    }
}

/// Decode data pages `pages` (page 0 is the superblock and is never in the
/// range) of an open log. `deks` decrypts an encrypted log.
fn visit_pages(
    file: &mut File,
    v1_namespace: Option<&str>,
    pages: std::ops::RangeInclusive<u32>,
    deks: Option<&HashMap<String, Dek>>,
    mut visit: impl FnMut(u32, Vec<ObjectRecord>),
) -> Result<(), String> {
    let (after_page, committed) = pages.into_inner();
    if after_page >= committed {
        return Ok(());
    }
    let start = PAGE as u64 * u64::from(after_page + 1);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    let mut page = vec![0u8; PAGE];
    for index in (after_page + 1)..=committed {
        file.read_exact(&mut page)
            .map_err(|_| format!("committed page {index}: short read"))?;
        verify_page(&page).map_err(|e| format!("committed page {index}: {e}"))?;
        let mut records = Vec::new();
        if let Some(deks) = deks {
            decode_encrypted_page(&page, index, deks, &mut records)
                .map_err(|e| format!("committed page {index}: {e}"))?;
        } else {
            decode_data_page(&page, v1_namespace, &mut records)?;
        }
        visit(index, records);
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

pub(crate) fn superblock_catalog(
    superblock: &[u8],
) -> Result<BTreeMap<String, crate::crypto::WrappedDek>, String> {
    let mut cur = &superblock[SUPER_COMMIT_OFF + 4..];
    decode_catalog(&mut cur)
}

/// Wrapped-DEK catalog of a V3 superblock or the current V4 snapshot.
#[cfg(test)]
pub(crate) fn wrapped_catalog(
    path: &Path,
) -> Result<BTreeMap<String, crate::crypto::WrappedDek>, String> {
    let mut file = crate::fault::open_read(path)?;
    let mut page = vec![0u8; PAGE];
    file.read_exact(&mut page)
        .map_err(|_| "missing superblock".to_string())?;
    if is_manifest_stub(&page) {
        let store = crate::objects::ObjectStore::open(
            &objects_dir(path),
            false,
            &crate::objects::ObjectBackend::File,
        )?;
        let encrypted = page.get(STUB_FLAG_OFF).copied().unwrap_or(0) & STUB_ENCRYPTED != 0;
        return Ok(load_current_manifest(&store, encrypted)?.catalog);
    }
    superblock_catalog(&page)
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
        let mut writer = LogWriter::create_superblock(&path, SyncPolicy::None).unwrap();
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
        let mut writer = LogWriter::create_superblock(&path, SyncPolicy::Group(2)).unwrap();
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
        let mut writer = LogWriter::create_superblock(&path, SyncPolicy::Group(2)).unwrap();
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
        let mut writer = LogWriter::create_superblock(&path, SyncPolicy::None).unwrap();
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
        let mut writer = LogWriter::create_superblock(&path, SyncPolicy::None).unwrap();
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
