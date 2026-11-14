use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::acl::PropertyAcl;
use crate::actions::{Action, ChangeSet, Edit, CHANGE_SET_BOUND, CHANGE_SET_KIND};
use crate::crypto::{tenant_of_namespace, LogKeys};
use crate::joins::{JoinMaps, LiveMeta};
use crate::log::{
    read_records_after_with_keys, read_records_with_keys, read_v1_records, LogWriter, SyncPolicy,
};
use crate::overlay::{ConflictAudit, OverlayPatch, OVERLAY_KIND};
use crate::paged::{PagedFile, ServingBudget};
use crate::schema::{SchemaDescriptor, SCHEMA_KIND};

mod checkpoint;
mod history;
mod shared;

pub(crate) use checkpoint::checkpoint_stamp;
mod sidecar;

pub use history::{AsOf, ObjectVersion, VersionOrigin};
pub use shared::{BatchWrite, SharedStore, StoreWriteGuard};

/// Rewrite `{log}.joins` and delete `{log}.joins.delta` when the dirty-set
/// file exceeds this many bytes. Compact still also runs when there is no
/// checkpoint or dirty rows exceed a quarter of identity.
pub const JOIN_DELTA_COMPACT_BYTES: u64 = 64 * 1024 * 1024;

/// Persist the join sidecar after this many log commits when it is already
/// present. The log stays authority; a lagging sidecar is caught up on open.
const SIDECAR_PERSIST_COMMITS: u32 = 32;

/// Persist the join sidecar at least this often while commits keep landing.
const SIDECAR_PERSIST_INTERVAL: Duration = Duration::from_millis(100);

/// Reserved kind for the last committed offset of a named `StreamApply` source.
pub const SOURCE_KIND: &str = "mikura.source";

/// Property on [`SOURCE_KIND`] holding the decimal last committed offset.
pub const SOURCE_OFFSET: &str = "offset";

/// Live identity: interned `(namespace, kind, key)`.
///
/// Namespace ids live on [`Store::ns`]. Kind and key ids live in that
/// namespace's [`JoinMaps`] intern, shared with the sidecar.
type LiveId = (u32, u32, u32);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectRecord {
    pub gen: u64,
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub hidden: bool,
    #[serde(default)]
    pub action_id: Option<String>,
    pub props: HashMap<String, String>,
    /// Descriptor version this generation was written under. Omitted bytes
    /// and 0 mean 1 (ADR 0035).
    #[serde(default = "schema_version_one")]
    pub schema_version: u32,
}

fn schema_version_one() -> u32 {
    1
}

impl Default for ObjectRecord {
    fn default() -> Self {
        Self {
            gen: 0,
            namespace: String::new(),
            kind: String::new(),
            key: String::new(),
            hidden: false,
            action_id: None,
            schema_version: 1,
            props: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionCommit {
    namespace: String,
    kind: String,
    key: String,
    gen: u64,
    hidden: bool,
    /// A later hide copied this id's payload onto the same identity.
    hide_applied: bool,
    props: HashMap<String, String>,
    /// Members of a change set when this id opened one (ADR 0033).
    members: Vec<SetMember>,
}

/// A committed change-set member. `props` is the put body, filled from the
/// first visible record under the set id on that identity after the marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetMember {
    hide: bool,
    kind: String,
    key: String,
    props: Option<HashMap<String, String>>,
}

enum ActionClaim {
    New,
    Replay,
}

enum ActionIdWrite {
    Claim,
    Copy,
}

pub struct Store {
    log: PathBuf,
    writer: LogWriter,
    /// Intern table for namespace strings in [`LiveId`].
    ns: Vec<String>,
    ns_ix: HashMap<String, u32>,
    identity: HashMap<LiveId, LiveMeta>,
    hidden_props: HashMap<LiveId, Vec<(u32, u32)>>,
    joins: HashMap<String, JoinMaps>,
    dirty: HashSet<LiveId>,
    has_checkpoint: bool,
    delta_bytes: u64,
    delta_compact_bytes: u64,
    sidecar_checkpoint_stamp: u32,
    sidecar_delta_stamp: Option<u32>,
    sidecar_commits_since_persist: u32,
    sidecar_last_persist: Instant,
    sidecar_persist_commits: u32,
    sidecar_persist_interval: Duration,
    /// False until create/open finishes so a failed open cannot persist an
    /// empty checkpoint over a live log.
    ready: bool,
    action_commits: HashMap<(String, String), ActionCommit>,
    action_commits_from_log: bool,
    /// Action ids admitted by stream push and not yet on the log. Later
    /// writes on this `Store` fail closed on remap or body conflict so an
    /// unflushed buffer cannot become permanently unflushable.
    pending_action_claims: HashMap<(String, String), ActionCommit>,
    /// Admitted uncommitted records in push order. Schema replacements
    /// consult this so a later write cannot pin an unflushed buffer.
    pending_admitted: Vec<ObjectRecord>,
    /// Instance props before live overlay merge, aligned with
    /// [`Self::pending_admitted`]. Overlay fences replay flush from these
    /// so a later overlay hide cannot drop a required property the original
    /// record did not carry.
    pending_source_props: Vec<HashMap<String, String>>,
    /// Stream that admitted each [`Self::pending_admitted`] row. Same-buffer
    /// schema chains skip their own rows; sibling buffers do not.
    pending_owners: Vec<u64>,
    /// Owner passed to the in-flight [`Self::admit_uncommitted_buffered`].
    admit_owner: u64,
    /// Indexes into [`Self::pending_admitted`] fulfilled in the current
    /// uncommitted transaction. Pruned after a successful commit; cleared
    /// on abort so a failed flush keeps the fence.
    applied_pending: HashSet<usize>,
    /// True while [`Self::append_admitted_batch`] applies a stream buffer.
    /// Intervening writers must not use this path; identical schema
    /// descriptors would otherwise look like a fulfillment the stream cannot
    /// see (ADR 0040).
    applying_admitted: bool,
    /// Visible schema rows by (namespace, kind), oldest version first.
    schema_history: HashMap<(String, String), Vec<SchemaDescriptor>>,
    schema_history_from_checkpoint: bool,
    serving_budget: Option<ServingBudget>,
    paged: Option<Arc<PagedFile>>,
    paged_scratch: Option<PathBuf>,
}

fn abandoned_stream_owners() -> std::sync::MutexGuard<'static, HashSet<u64>> {
    static DROPPED: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    DROPPED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

impl Store {
    fn sidecar_path(log: &Path, suffix: &str) -> PathBuf {
        let mut path = log.as_os_str().to_os_string();
        path.push(suffix);
        PathBuf::from(path)
    }

    pub fn join_map_path(log: &Path) -> PathBuf {
        Self::sidecar_path(log, ".joins")
    }

    pub fn join_delta_path(log: &Path) -> PathBuf {
        Self::sidecar_path(log, ".joins.delta")
    }

    pub fn checkpoint_path(log: &Path) -> PathBuf {
        Self::sidecar_path(log, ".ckpt")
    }

    pub fn checkpoint_prev_path(log: &Path) -> PathBuf {
        Self::sidecar_path(log, ".ckpt.prev")
    }

    fn checkpoint_tmp_path(log: &Path) -> PathBuf {
        Self::sidecar_path(log, ".ckpt.tmp")
    }

    pub fn create(log: &Path) -> Result<Self, String> {
        Self::create_with_sync(log, SyncPolicy::Group(32))
    }

    /// Create a log on `backend`. [`ObjectBackend::File`] is [`Self::create`].
    pub fn create_with(log: &Path, backend: crate::objects::ObjectBackend) -> Result<Self, String> {
        Self::create_cleared(
            log,
            LogWriter::create_with_backend(log, SyncPolicy::Group(32), backend)?,
        )
    }

    pub fn create_with_sync(log: &Path, sync: SyncPolicy) -> Result<Self, String> {
        Self::create_cleared(log, LogWriter::create(log, sync)?)
    }

    /// Encrypted log (`MIKURAV3`). Namespaces must be tenant-prefixed
    /// (`tenant_namespace`).
    pub fn create_with_keys(log: &Path, keys: LogKeys) -> Result<Self, String> {
        Self::create_with_keys_sync(log, SyncPolicy::Group(32), keys)
    }

    pub fn create_with_keys_on(
        log: &Path,
        keys: LogKeys,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::create_cleared(
            log,
            LogWriter::create_with_keys_and_backend(log, SyncPolicy::Group(32), keys, backend)?,
        )
    }

    pub fn create_with_keys_sync(
        log: &Path,
        sync: SyncPolicy,
        keys: LogKeys,
    ) -> Result<Self, String> {
        Self::create_cleared(log, LogWriter::create_with_keys(log, sync, keys)?)
    }

    fn create_cleared(log: &Path, writer: LogWriter) -> Result<Self, String> {
        let _ = std::fs::remove_file(Self::join_map_path(log));
        let _ = std::fs::remove_file(Self::join_delta_path(log));
        let _ = std::fs::remove_file(Self::checkpoint_path(log));
        let _ = std::fs::remove_file(Self::checkpoint_prev_path(log));
        let _ = std::fs::remove_file(Self::checkpoint_tmp_path(log));
        let mut store = Self::empty(log.to_path_buf(), writer);
        store.ready = true;
        Ok(store)
    }

    /// Explicit log-format step for a pre-namespace `MIKURAV1` log (ADR 0030).
    ///
    /// Writes every committed record of `src`, assigned to `namespace`, to a
    /// new `MIKURAV2` log at `dst`, byte-for-byte otherwise, and returns the
    /// record count. [`Self::open`] on `dst` then rebuilds the projection.
    /// `src` is only read; its uncommitted tail is dropped as on open. The
    /// caller names the namespace: there is no shared default. Neither `dst`
    /// nor its sidecars may exist, and a crash leaves no partial `dst`.
    pub fn migrate_unnamespaced(src: &Path, dst: &Path, namespace: &str) -> Result<usize, String> {
        let namespace = require_namespace(namespace)?.to_string();
        Self::migrate_unnamespaced_with(src, dst, |_, _| Ok(namespace.clone()))
    }

    /// Like [`Self::migrate_unnamespaced`], but each `(kind, key)` chooses its
    /// namespace. Mapping must succeed for every committed record or nothing
    /// is written. Empty mapped namespaces fail closed.
    pub fn migrate_unnamespaced_with<F>(
        src: &Path,
        dst: &Path,
        mut namespace_for: F,
    ) -> Result<usize, String>
    where
        F: FnMut(&str, &str) -> Result<String, String>,
    {
        // Refuse rather than delete: a leftover sidecar would be trusted on
        // open, and deleting one could hit a caller's file.
        for path in [
            dst.to_path_buf(),
            crate::log::objects_dir(dst),
            Self::join_map_path(dst),
            Self::join_delta_path(dst),
            Self::checkpoint_path(dst),
            Self::checkpoint_prev_path(dst),
        ] {
            if path.exists() {
                return Err(format!(
                    "migration target {} already exists",
                    path.display()
                ));
            }
        }
        // The placeholder is overwritten before any V2 page is written.
        let pending = read_v1_records(src, "pending")?;
        let mut records = Vec::with_capacity(pending.len());
        for record in pending {
            let namespace =
                require_namespace(&namespace_for(&record.kind, &record.key)?)?.to_string();
            records.push(ObjectRecord {
                namespace,
                ..record
            });
        }
        // A per-call staging file, published with a no-clobber link, so
        // concurrent migrations to one `dst` cannot interleave or overwrite.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let staging =
            Self::sidecar_path(dst, &format!(".migrating-{}-{nonce}", std::process::id()));
        let written = (|| {
            let mut writer = LogWriter::create_superblock(&staging, SyncPolicy::Group(32))?;
            for record in &records {
                // A V1 record near the page limit may not fit the namespace.
                writer.append_record(record).map_err(|error| {
                    format!(
                        "cannot migrate {}/{} into namespace {}: {error}",
                        record.kind, record.key, record.namespace
                    )
                })?;
            }
            writer.flush()?;
            drop(writer);
            std::fs::hard_link(&staging, dst).map_err(|e| format!("publish {}: {e}", dst.display()))
        })();
        let _ = std::fs::remove_file(&staging);
        written.map(|()| records.len())
    }

    /// Copy committed pages of a `MIKURAV2`/`MIKURAV3` log to a new swap-committed
    /// `MIKURAV4` log at `dst`. `src` is only read. Dual-read of source and
    /// destination must match.
    pub fn migrate_to_manifest(src: &Path, dst: &Path) -> Result<usize, String> {
        Self::migrate_to_manifest_with(src, dst, None)
    }

    /// Encrypted `MIKURAV3` source: the same keys unwrap the copied catalog.
    pub fn migrate_to_manifest_with_keys(
        src: &Path,
        dst: &Path,
        keys: LogKeys,
    ) -> Result<usize, String> {
        Self::migrate_to_manifest_with(src, dst, Some(keys))
    }

    fn migrate_to_manifest_with(
        src: &Path,
        dst: &Path,
        keys: Option<LogKeys>,
    ) -> Result<usize, String> {
        for path in [
            dst.to_path_buf(),
            crate::log::objects_dir(dst),
            Self::join_map_path(dst),
            Self::join_delta_path(dst),
            Self::checkpoint_path(dst),
            Self::checkpoint_prev_path(dst),
        ] {
            if path.exists() {
                return Err(format!(
                    "migration target {} already exists",
                    path.display()
                ));
            }
        }
        let stub = crate::log::read_committed_page(src, 0)?;
        if crate::log::is_manifest_stub(&stub) {
            return Err("log is already a swap-committed manifest".into());
        }
        let magic = crate::log::superblock_magic(&stub)?;
        if magic == crate::LOG_FORMAT && keys.is_some() {
            return Err("log is not encrypted".into());
        }
        if magic == crate::LOG_FORMAT_ENCRYPTED && keys.is_none() {
            return Err("encrypted log requires keys".into());
        }
        if magic != crate::LOG_FORMAT && magic != crate::LOG_FORMAT_ENCRYPTED {
            return Err("bad magic".into());
        }
        let committed = crate::log::parse_superblock(&stub)?;
        let mut pages = Vec::with_capacity(committed as usize);
        for index in 1..=committed {
            pages.push(crate::log::read_committed_page(src, index)?);
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let staging =
            Self::sidecar_path(dst, &format!(".migrating-{}-{nonce}", std::process::id()));
        let records = crate::log::read_records_with_keys(src, keys.as_ref())?.len();
        let written = (|| {
            let mut writer = if let Some(keys) = keys {
                let catalog = crate::log::superblock_catalog(&stub)?;
                LogWriter::create_manifest_with_catalog(
                    &staging,
                    SyncPolicy::Group(32),
                    keys,
                    catalog,
                )?
            } else {
                LogWriter::create(&staging, SyncPolicy::Group(32))?
            };
            writer.append_copied_pages(pages)?;
            writer.flush()?;
            let committed = writer.committed_pages();
            drop(writer);
            crate::log::publish_log(&staging, dst)?;
            crate::backup::carry_checkpoint(src, dst, committed).inspect_err(|_| {
                let _ = std::fs::remove_file(Self::checkpoint_path(dst));
                crate::log::remove_log(dst);
            })?;
            Ok(())
        })();
        crate::log::remove_log(&staging);
        written.map(|()| records)
    }

    pub fn open(log: &Path) -> Result<Self, String> {
        Self::open_with_sync(log, SyncPolicy::Group(32))
    }

    /// Open a log on `backend`. A file-backend open infers an HTTP locator.
    pub fn open_with(log: &Path, backend: crate::objects::ObjectBackend) -> Result<Self, String> {
        Self::open_writer(
            log,
            LogWriter::open_with_backend(log, SyncPolicy::Group(32), backend)?,
            None,
        )
    }

    pub fn open_with_sync(log: &Path, sync: SyncPolicy) -> Result<Self, String> {
        Self::open_writer(log, LogWriter::open(log, sync)?, None)
    }

    /// Open serving `load` and `evaluate` from paged projection pages whose
    /// decoded cache stays inside `budget` (ADR 0048). Zero is invalid.
    pub fn open_with_serving_budget(log: &Path, budget: ServingBudget) -> Result<Self, String> {
        Self::open_writer(
            log,
            LogWriter::open(log, SyncPolicy::Group(32))?,
            Some(budget),
        )
    }

    /// Open a `MIKURAV3` log. Revoked or ungranted tenants are omitted.
    pub fn open_with_keys(log: &Path, keys: LogKeys) -> Result<Self, String> {
        Self::open_with_keys_sync(log, SyncPolicy::Group(32), keys)
    }

    pub fn open_with_keys_on(
        log: &Path,
        keys: LogKeys,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::open_writer(
            log,
            LogWriter::open_with_keys_and_backend(log, SyncPolicy::Group(32), keys, backend)?,
            None,
        )
    }

    pub fn open_with_keys_and_serving_budget(
        log: &Path,
        keys: LogKeys,
        budget: ServingBudget,
    ) -> Result<Self, String> {
        Self::open_writer(
            log,
            LogWriter::open_with_keys(log, SyncPolicy::Group(32), keys)?,
            Some(budget),
        )
    }

    pub fn open_with_keys_sync(
        log: &Path,
        sync: SyncPolicy,
        keys: LogKeys,
    ) -> Result<Self, String> {
        Self::open_writer(log, LogWriter::open_with_keys(log, sync, keys)?, None)
    }

    /// Open at the current snapshot without taking the writer role
    /// (ADR 0047 §5). Does not probe, collect, or lock.
    pub fn open_read_only(log: &Path) -> Result<Self, String> {
        Self::open_writer(
            log,
            LogWriter::open_read_only(log, SyncPolicy::Group(32))?,
            None,
        )
    }

    pub fn open_read_only_with(
        log: &Path,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::open_writer(
            log,
            LogWriter::open_read_only_with_backend(log, SyncPolicy::Group(32), backend)?,
            None,
        )
    }

    pub fn open_read_only_with_keys(log: &Path, keys: LogKeys) -> Result<Self, String> {
        Self::open_writer(
            log,
            LogWriter::open_read_only_with_keys(log, SyncPolicy::Group(32), keys)?,
            None,
        )
    }

    pub fn open_read_only_with_keys_on(
        log: &Path,
        keys: LogKeys,
        backend: crate::objects::ObjectBackend,
    ) -> Result<Self, String> {
        Self::open_writer(
            log,
            LogWriter::open_read_only_with_keys_and_backend(
                log,
                SyncPolicy::Group(32),
                keys,
                backend,
            )?,
            None,
        )
    }

    fn open_writer(
        log: &Path,
        writer: LogWriter,
        serving_budget: Option<ServingBudget>,
    ) -> Result<Self, String> {
        let mut store = Self::empty(log.to_path_buf(), writer);
        store.serving_budget = serving_budget;
        if store.serving_budget.is_some() && store.writer.keys().is_some() {
            return Err("serving budget is not available on encrypted stores".into());
        }
        store.install_projection()?;
        store.drop_revoked_tenants();
        store.ensure_schema_history()?;
        if store.serving_budget.is_some() {
            store.adopt_paged_serving()?;
        }
        store.ready = true;
        Ok(store)
    }

    fn drop_revoked_tenants(&mut self) {
        if self.writer.keys().is_none() {
            return;
        }
        let revoked = |namespace: &str| {
            tenant_of_namespace(namespace).ok().is_some_and(|tenant| {
                self.writer.tenant_revoked(tenant) || self.writer.tenant_withheld(tenant)
            })
        };
        let revoked_ns: HashSet<u32> = self
            .ns
            .iter()
            .enumerate()
            .filter(|(_, name)| revoked(name))
            .map(|(i, _)| i as u32)
            .collect();
        self.joins.retain(|namespace, _| !revoked(namespace));
        self.identity.retain(|id, _| !revoked_ns.contains(&id.0));
        self.hidden_props
            .retain(|id, _| !revoked_ns.contains(&id.0));
        self.action_commits
            .retain(|(namespace, _), _| !revoked(namespace));
        self.schema_history
            .retain(|(namespace, _), _| !revoked(namespace));
    }

    fn live_tenants(&self) -> Vec<String> {
        let mut tenants = BTreeSet::new();
        let mut push = |namespace: &str| {
            if let Ok(tenant) = tenant_of_namespace(namespace) {
                tenants.insert(tenant.to_string());
            }
        };
        for namespace in self.joins.keys() {
            push(namespace);
        }
        for id in self.identity.keys() {
            push(self.ns_name(id.0));
        }
        for (namespace, _) in self.action_commits.keys() {
            push(namespace);
        }
        for (namespace, _) in self.schema_history.keys() {
            push(namespace);
        }
        tenants.into_iter().collect()
    }

    fn empty(log: PathBuf, writer: LogWriter) -> Self {
        let mut store = Self {
            log,
            writer,
            ns: Vec::new(),
            ns_ix: HashMap::new(),
            identity: HashMap::new(),
            hidden_props: HashMap::new(),
            joins: HashMap::new(),
            dirty: HashSet::new(),
            has_checkpoint: false,
            delta_bytes: 0,
            delta_compact_bytes: JOIN_DELTA_COMPACT_BYTES,
            sidecar_checkpoint_stamp: 0,
            sidecar_delta_stamp: None,
            sidecar_commits_since_persist: 0,
            sidecar_last_persist: Instant::now(),
            sidecar_persist_commits: SIDECAR_PERSIST_COMMITS,
            sidecar_persist_interval: SIDECAR_PERSIST_INTERVAL,
            ready: false,
            action_commits: HashMap::new(),
            action_commits_from_log: true,
            pending_action_claims: HashMap::new(),
            pending_admitted: Vec::new(),
            pending_source_props: Vec::new(),
            pending_owners: Vec::new(),
            admit_owner: 0,
            applied_pending: HashSet::new(),
            applying_admitted: false,
            schema_history: HashMap::new(),
            schema_history_from_checkpoint: false,
            serving_budget: None,
            paged: None,
            paged_scratch: None,
        };
        if cfg!(test) {
            store.sidecar_persist_commits = 1;
            store.sidecar_persist_interval = Duration::ZERO;
        }
        store
    }

    fn intern_ns(&mut self, namespace: &str) -> u32 {
        if let Some(&id) = self.ns_ix.get(namespace) {
            return id;
        }
        let id = u32::try_from(self.ns.len()).expect("namespace intern fits u32");
        self.ns.push(namespace.to_string());
        self.ns_ix.insert(namespace.to_string(), id);
        id
    }

    fn ns_name(&self, id: u32) -> &str {
        self.ns.get(id as usize).map(String::as_str).unwrap_or("")
    }

    fn intern_live(&mut self, namespace: &str, kind: &str, key: &str) -> Result<LiveId, String> {
        let ns_id = self.intern_ns(namespace);
        let joins = self.joins_for_write(namespace)?;
        Ok((ns_id, joins.intern(kind), joins.intern(key)))
    }

    fn find_live(&self, namespace: &str, kind: &str, key: &str) -> Option<LiveId> {
        let ns_id = *self.ns_ix.get(namespace)?;
        let joins = self.joins.get(namespace)?;
        Some((
            ns_id,
            joins.intern_existing(kind)?,
            joins.intern_existing(key)?,
        ))
    }

    fn live_meta(&self, id: LiveId) -> Option<LiveMeta> {
        if let Some(paged) = &self.paged {
            return paged.identity(id).map(|(meta, _)| meta);
        }
        self.identity.get(&id).cloned()
    }

    fn hidden_owned(&self, id: LiveId) -> Option<Vec<(u32, u32)>> {
        if let Some(paged) = &self.paged {
            let (meta, owned) = paged.identity(id)?;
            return meta.hidden.then_some(owned);
        }
        self.hidden_props.get(&id).cloned()
    }

    fn ensure_writable(&mut self) -> Result<(), String> {
        let Some(paged) = self.paged.take() else {
            return Ok(());
        };
        match paged.decode_all() {
            Ok(loaded) => {
                self.adopt_checkpoint(loaded)?;
                Ok(())
            }
            Err(err) => {
                self.paged = Some(paged);
                Err(err)
            }
        }
    }

    fn adopt_paged_serving(&mut self) -> Result<(), String> {
        let Some(budget) = self.serving_budget else {
            return Ok(());
        };
        if self.paged.is_some() {
            return Ok(());
        }
        if self.writer.keys().is_some() {
            return Err("serving budget is not available on encrypted stores".into());
        }
        if !self.writer.withheld().is_empty() {
            return Ok(());
        }
        let path = Self::join_map_path(&self.log);
        if !self.sidecar_is_matching_paged(&path, self.writer.committed_pages())? {
            self.persist_paged_checkpoint(self.writer.committed_pages())?;
            return Ok(());
        }
        self.attach_serving_sidecar(&path, budget)
    }

    fn try_adopt_existing_paged(&mut self, path: &Path, pages: u32) -> Result<bool, String> {
        let Some(budget) = self.serving_budget else {
            return Ok(false);
        };
        if !self.sidecar_is_matching_paged(path, pages)? {
            return Ok(false);
        }
        self.attach_serving_sidecar(path, budget)?;
        Ok(true)
    }

    fn sidecar_is_matching_paged(&self, path: &Path, pages: u32) -> Result<bool, String> {
        if !path.exists() {
            return Ok(false);
        }
        if self.writer.keys().is_some() {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            if !crate::crypto::is_sealed(&bytes) {
                return Ok(false);
            }
            let Some(dek) = self.writer.store_dek() else {
                return Ok(false);
            };
            let Ok((_, inner)) = crate::crypto::open_blob(dek, &bytes) else {
                return Ok(false);
            };
            if !crate::paged::is_paged_magic(&inner) {
                return Ok(false);
            }
            return Ok(crate::paged::peek_log_pages_bytes(&inner)? == pages);
        }
        let mut magic = [0u8; 8];
        std::fs::File::open(path)
            .and_then(|mut file| {
                use std::io::Read;
                file.read_exact(&mut magic)
            })
            .map_err(|e| e.to_string())?;
        if magic != *crate::JOIN_FORMAT_PAGED {
            return Ok(false);
        }
        Ok(crate::paged::peek_log_pages(path)? == pages)
    }

    fn attach_serving_sidecar(&mut self, path: &Path, budget: ServingBudget) -> Result<(), String> {
        let open_path = if self.writer.keys().is_some() {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let dek = self
                .writer
                .store_dek()
                .ok_or_else(|| "encrypted log requires a sealed snapshot".to_string())?;
            let (_, inner) = crate::crypto::open_blob(dek, &bytes)?;
            if !crate::paged::is_paged_magic(&inner) {
                return Err("encrypted snapshot is not a paged projection".into());
            }
            self.write_paged_scratch(&inner)?
        } else {
            path.to_path_buf()
        };
        let file = Arc::new(PagedFile::open(&open_path, budget)?);
        let mut joins = HashMap::new();
        self.ns = file.ns_names().to_vec();
        self.rebuild_ns_ix();
        for name in file.ns_names() {
            if name.is_empty() {
                continue;
            }
            let mut maps = JoinMaps::default();
            maps.attach_paged(Arc::clone(&file), name.clone());
            joins.insert(name.clone(), maps);
        }
        self.joins = joins;
        self.identity.clear();
        self.hidden_props.clear();
        self.paged = Some(Arc::clone(&file));
        self.has_checkpoint = true;
        self.sidecar_checkpoint_stamp = file.log_pages();
        self.sidecar_delta_stamp = None;
        Ok(())
    }

    /// Encrypted serving unseals MKJOIN07 into a process-private scratch file.
    /// The on-disk sidecar stays sealed; Drop removes the scratch.
    fn write_paged_scratch(&mut self, bytes: &[u8]) -> Result<PathBuf, String> {
        self.drop_paged_scratch();
        let dir = std::env::temp_dir().join(format!(
            "mikura-paged-serve-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| e.to_string())?;
        let path = dir.join("joins");
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|e| e.to_string())?;
            file.write_all(bytes).map_err(|e| e.to_string())?;
            file.sync_data().map_err(|e| e.to_string())?;
        }
        self.paged_scratch = Some(path.clone());
        Ok(path)
    }

    fn drop_paged_scratch(&mut self) {
        if let Some(path) = self.paged_scratch.take() {
            let _ = std::fs::remove_file(&path);
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_dir(dir);
            }
        }
    }

    fn release_paged(&mut self) {
        self.paged = None;
        self.joins.clear();
        self.drop_paged_scratch();
    }

    fn rebuild_ns_ix(&mut self) {
        self.ns_ix.clear();
        for (i, name) in self.ns.iter().enumerate() {
            self.ns_ix.insert(name.clone(), i as u32);
        }
    }

    fn live_kind(&self, id: LiveId) -> Option<&str> {
        self.joins.get(self.ns_name(id.0))?.intern_get(id.1)
    }

    fn live_key(&self, id: LiveId) -> Option<&str> {
        self.joins.get(self.ns_name(id.0))?.intern_get(id.2)
    }

    fn live_strings(&self, id: LiveId) -> Result<(&str, &str, &str), String> {
        let ns = self.ns_name(id.0);
        let joins = self
            .joins
            .get(ns)
            .ok_or_else(|| "missing namespace joins".to_string())?;
        let kind = joins
            .intern_get(id.1)
            .ok_or_else(|| "missing intern kind".to_string())?;
        let key = joins
            .intern_get(id.2)
            .ok_or_else(|| "missing intern key".to_string())?;
        Ok((ns, kind, key))
    }

    fn materialize_owned(maps: &JoinMaps, owned: &[(u32, u32)]) -> HashMap<String, String> {
        if maps.intern.is_empty() {
            let mut props = HashMap::with_capacity(owned.len());
            for &(prop_id, value_id) in owned {
                let Some(prop) = maps.intern_get(prop_id) else {
                    continue;
                };
                let Some(value) = maps.intern_get(value_id) else {
                    continue;
                };
                props.insert(prop.to_string(), value.to_string());
            }
            return props;
        }
        PropertyAcl::omit_owned(&maps.intern, 0, owned, &HashSet::new())
    }

    /// Use a tiny compact bound in tests so a few dirty-set frames can exceed
    /// it without writing `JOIN_DELTA_COMPACT_BYTES` of delta.
    #[cfg(test)]
    pub(crate) fn set_join_delta_compact_bytes(&mut self, bytes: u64) {
        self.delta_compact_bytes = bytes;
    }

    /// Defer or force sidecar persist in tests. `u32::MAX` plus a long
    /// interval leaves the sidecar stale until close.
    #[cfg(test)]
    pub(crate) fn set_sidecar_persist(&mut self, commits: u32, interval: Duration) {
        self.sidecar_persist_commits = commits;
        self.sidecar_persist_interval = interval;
    }

    /// End this store as a killed process would: Drop and its sidecar
    /// persist never run, and the OS releases the writer lock.
    #[cfg(test)]
    pub(crate) fn crash(self) {
        self.writer.release_lock();
        std::mem::forget(self);
    }

    #[cfg(test)]
    pub(crate) fn release_writer_lock(&self) {
        self.writer.release_lock();
    }

    #[cfg(test)]
    pub(crate) fn set_crash_before_swap(&mut self, crash: bool) {
        self.writer.set_crash_before_swap(crash);
    }

    fn install_live(
        &mut self,
        id: LiveId,
        meta: LiveMeta,
        props: HashMap<String, String>,
    ) -> Result<(), String> {
        let ns = self.ns_name(id.0).to_string();
        {
            let joins = self.joins_for_write(&ns)?;
            let kind = joins.intern_get(id.1).unwrap_or("").to_string();
            let key = joins.intern_get(id.2).unwrap_or("").to_string();
            joins.remove(&kind, &key);
        }
        let hidden = meta.hidden;
        self.identity.insert(id, meta);
        if hidden {
            let joins = self.joins_for_write(&ns)?;
            let owned: Vec<(u32, u32)> = props
                .iter()
                .map(|(name, value)| (joins.intern(name), joins.intern(value)))
                .collect();
            self.hidden_props.insert(id, owned);
        } else {
            self.hidden_props.remove(&id);
            let joins = self.joins_for_write(&ns)?;
            let kind = joins.intern_get(id.1).unwrap_or("").to_string();
            let key = joins.intern_get(id.2).unwrap_or("").to_string();
            joins.index(&kind, &key, false, props);
        }
        Ok(())
    }

    fn remember_action(&mut self, record: &ObjectRecord) {
        let Some(id) = record.action_id.as_deref().filter(|id| !id.is_empty()) else {
            return;
        };
        let commit = self
            .action_commits
            .entry((record.namespace.clone(), id.to_string()))
            .or_insert_with(|| ActionCommit {
                namespace: record.namespace.clone(),
                kind: record.kind.clone(),
                key: record.key.clone(),
                gen: record.gen,
                hidden: record.hidden,
                hide_applied: false,
                props: record.props.clone(),
                members: if record.kind == CHANGE_SET_KIND {
                    marker_members(&record.props)
                } else {
                    Vec::new()
                },
            });
        if commit.kind == CHANGE_SET_KIND {
            if !record.hidden {
                if let Some(member) = commit.members.iter_mut().find(|member| {
                    !member.hide
                        && member.props.is_none()
                        && member.kind == record.kind
                        && member.key == record.key
                }) {
                    member.props = Some(record.props.clone());
                }
            }
            return;
        }
        if record.hidden
            && !commit.hidden
            && commit.namespace == record.namespace
            && commit.kind == record.kind
            && commit.key == record.key
            && commit.props == record.props
        {
            commit.hide_applied = true;
        }
    }

    fn ensure_action_commits(&mut self) -> Result<(), String> {
        if self.action_commits_from_log {
            return Ok(());
        }
        let since_open = std::mem::take(&mut self.action_commits);
        for record in read_records_with_keys(&self.log, self.writer.keys())? {
            self.remember_action(&record);
        }
        for (id, commit) in since_open {
            self.action_commits
                .entry(id)
                .and_modify(|logged| logged.hide_applied |= commit.hide_applied)
                .or_insert(commit);
        }
        self.action_commits_from_log = true;
        Ok(())
    }

    fn replay_overlay(
        &self,
        committed: ActionCommit,
        patch: &OverlayPatch,
        action_id: &str,
    ) -> Result<(), String> {
        if committed.kind != OVERLAY_KIND {
            return Err(format!(
                "action id {action_id} already committed on {}/{}",
                committed.kind, committed.key
            ));
        }
        let stored = OverlayPatch::from_record(&ObjectRecord {
            gen: committed.gen,
            namespace: committed.namespace,
            kind: committed.kind,
            key: committed.key,
            hidden: committed.hidden,
            action_id: Some(action_id.to_string()),
            schema_version: 1,
            props: committed.props,
        })?;
        if stored.namespace != patch.namespace
            || stored.kind != patch.kind
            || stored.key != patch.key
        {
            return Err(format!(
                "action id {action_id} already committed on {}/{}",
                stored.kind, stored.key
            ));
        }
        let mut stored_cleared = stored.cleared.clone();
        let mut patch_cleared = patch.cleared.clone();
        stored_cleared.sort();
        patch_cleared.sort();
        if stored.props != patch.props
            || stored_cleared != patch_cleared
            || stored.at != patch.canonical_at()?
        {
            return Err(format!("action id {action_id} body conflict"));
        }
        Ok(())
    }

    fn require_expected_gen(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        expected_gen: Option<u64>,
    ) -> Result<(), String> {
        let Some(expected) = expected_gen else {
            return Ok(());
        };
        let Some(id) = self.find_live(namespace, kind, key) else {
            return if expected != 0 {
                Err(format!("stale generation: expected {expected}, live none"))
            } else {
                Ok(())
            };
        };
        match self.live_meta(id) {
            Some(meta) if meta.gen != expected => Err(format!(
                "stale generation: expected {expected}, live {}",
                meta.gen
            )),
            None if expected != 0 => {
                Err(format!("stale generation: expected {expected}, live none"))
            }
            _ => Ok(()),
        }
    }

    fn apply_record(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.remember_action(&record);
        let schema_remap = if record.kind == SCHEMA_KIND && !record.hidden {
            SchemaDescriptor::from_record(&record).ok().inspect(|desc| {
                self.remember_schema(&record.namespace, desc.clone());
            })
        } else {
            None
        };
        let id = self.intern_live(&record.namespace, &record.kind, &record.key)?;
        let action_id = record
            .action_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .map(|id| {
                self.joins_for_write(&record.namespace)
                    .map(|joins| joins.intern(id))
            })
            .transpose()?;
        let namespace = record.namespace.clone();
        self.install_live(
            id,
            LiveMeta {
                gen: record.gen,
                hidden: record.hidden,
                action_id,
                schema_version: if record.schema_version == 0 {
                    1
                } else {
                    record.schema_version
                },
            },
            record.props,
        )?;
        self.dirty.insert(id);
        if let Some(desc) = schema_remap {
            if !desc.migration.is_empty() {
                self.remap_kind(&namespace, &desc)?;
            }
        }
        Ok(())
    }

    fn require_action_id(id: &str, empty: &'static str) -> Result<(), String> {
        if id.is_empty() {
            Err(empty.into())
        } else {
            Ok(())
        }
    }

    /// One clerk-assigned Action id is the retry key on every write path
    /// (ADR 0011, ADR 0019, ADR 0025). Matching body is a no-op. A remapped
    /// identity or different body fails closed.
    fn action_claim(
        &mut self,
        namespace: &str,
        action_id: &str,
        kind: &str,
        key: &str,
        hidden: bool,
        props: &HashMap<String, String>,
    ) -> Result<ActionClaim, String> {
        self.ensure_action_commits()?;
        let key_pair = (namespace.to_string(), action_id.to_string());
        let committed = self.action_commits.get(&key_pair).cloned();
        let reserved = self.pending_action_claims.get(&key_pair).cloned();
        let from_pending = committed.is_none() && reserved.is_some();
        let Some(committed) = committed.or(reserved) else {
            return Ok(ActionClaim::New);
        };
        if committed.namespace != namespace || committed.kind != kind || committed.key != key {
            return Err(format!(
                "action id {action_id} already committed on {}/{}/{}",
                committed.namespace, committed.kind, committed.key
            ));
        }
        if committed.props != *props {
            return Err(format!("action id {action_id} body conflict"));
        }
        if hidden {
            // A hide under a claimed id copies the claimed payload; the first
            // one appends, a retry after it is a replay.
            return Ok(if committed.hidden || committed.hide_applied {
                ActionClaim::Replay
            } else {
                ActionClaim::New
            });
        }
        if committed.hidden {
            return Err(format!("action id {action_id} body conflict"));
        }
        if from_pending {
            return Ok(ActionClaim::Replay);
        }
        if let Some(schema) = self.schema(namespace, kind)? {
            schema.validate(&ObjectRecord {
                gen: 0,
                namespace: namespace.to_string(),
                kind: kind.to_string(),
                key: key.to_string(),
                hidden: false,
                action_id: Some(action_id.to_string()),
                schema_version: 1,
                props: props.clone(),
            })?;
        }
        Ok(ActionClaim::Replay)
    }

    fn reserve_action(&mut self, record: &ObjectRecord) {
        let Some(id) = record.action_id.as_deref().filter(|id| !id.is_empty()) else {
            return;
        };
        self.pending_action_claims
            .entry((record.namespace.clone(), id.to_string()))
            .or_insert_with(|| ActionCommit {
                namespace: record.namespace.clone(),
                kind: record.kind.clone(),
                key: record.key.clone(),
                gen: record.gen,
                hidden: record.hidden,
                hide_applied: false,
                props: record.props.clone(),
                members: if record.kind == CHANGE_SET_KIND {
                    marker_members(&record.props)
                } else {
                    Vec::new()
                },
            });
    }

    fn load_reserved<T>(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        decode: fn(&ObjectRecord) -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        if !self.joins.contains_key(namespace) {
            return Ok(None);
        }
        let Some(id) = self.find_live(namespace, kind, key) else {
            return Ok(None);
        };
        let Some(meta) = self.live_meta(id) else {
            return Ok(None);
        };
        if meta.hidden {
            return Ok(None);
        }
        let record = self.load(namespace, kind, key, &PropertyAcl::allow_all())?;
        decode(&record).map(Some)
    }

    /// Live object for `(namespace, kind, key)` after ingest or [`Store::open`].
    ///
    /// Missing or unknown namespace fails closed. Missing identity fails
    /// closed. Hidden records return the hidden payload and stay out of join
    /// maps. Denied properties are omitted; remaining keys keep stored
    /// values. The object log remains authority; interned sidecar props are
    /// a deletable projection.
    pub fn load(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
    ) -> Result<ObjectRecord, String> {
        self.load_in_view(namespace, kind, key, acl, None)
    }

    /// [`Self::load`] with optional precomputed interned deny ids for this
    /// namespace. Evaluate computes them once per request.
    pub(crate) fn load_in_view(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
        denied: Option<&HashSet<(u32, u32)>>,
    ) -> Result<ObjectRecord, String> {
        let namespace = self.require_known_namespace(namespace)?;
        let missing = || format!("unknown identity {namespace}/{kind}/{key}");
        if let Ok(tenant) = tenant_of_namespace(namespace) {
            if !self.writer.tenant_key_usable(tenant) {
                return Err(missing());
            }
        }
        let id = match self.find_live(namespace, kind, key) {
            Some(id) => id,
            None => {
                self.paged_fail_closed()?;
                return Err(missing());
            }
        };
        let meta = match self.live_meta(id) {
            Some(meta) => meta,
            None => {
                self.paged_fail_closed()?;
                return Err(missing());
            }
        };
        let maps = self.joins_in(namespace)?;
        let denied_buf;
        let denied = match denied {
            Some(denied) => denied,
            None => {
                denied_buf = acl.interned_denies(|token| maps.intern_existing(token));
                &denied_buf
            }
        };
        let props = if meta.hidden {
            self.hidden_props_omitting(maps, kind, &id, denied)
        } else {
            maps.row_props_omitting(kind, key, denied)
        };
        // Row-filter properties are never denied, so `props` still holds them.
        // Declared drops are omitted before visibility so current load matches
        // evaluate and mutation checks (ADR 0035).
        let mut record = ObjectRecord {
            gen: meta.gen,
            namespace: namespace.to_string(),
            kind: kind.to_string(),
            key: key.to_string(),
            hidden: meta.hidden,
            action_id: match meta.action_id {
                None => None,
                Some(intern) => Some(
                    maps.intern_get(intern)
                        .ok_or_else(|| "missing intern action".to_string())?
                        .to_string(),
                ),
            },
            props,
            schema_version: if meta.schema_version == 0 {
                1
            } else {
                meta.schema_version
            },
        };
        self.apply_schema_view(&mut record, None)?;
        self.paged_fail_closed()?;
        if !acl.object_visible(kind, key, |property| {
            record.props.get(property).map(String::as_str)
        }) {
            return Err(missing());
        }
        record.props = acl.omit_denied(&record.kind, record.props);
        Ok(record)
    }

    /// Last visible `mikura.schema` row for `kind` in `namespace`, or `None`
    /// when absent or hidden. Missing or unknown namespace fails closed.
    pub fn schema(&self, namespace: &str, kind: &str) -> Result<Option<SchemaDescriptor>, String> {
        self.require_known_namespace(namespace)?;
        self.load_reserved(namespace, SCHEMA_KIND, kind, SchemaDescriptor::from_record)
    }

    /// Last visible `mikura.overlay` row for `(namespace, kind, key)`, or
    /// `None` when absent or hidden. Missing or unknown namespace fails
    /// closed.
    pub fn overlay(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<OverlayPatch>, String> {
        self.require_known_namespace(namespace)?;
        self.load_reserved(
            namespace,
            OVERLAY_KIND,
            &OverlayPatch::identity_key(kind, key),
            OverlayPatch::from_record,
        )
    }

    /// Admit a property overlay and rematerialize a visible instance.
    ///
    /// The Action id is a retry key (ADR 0019). The same id and the same
    /// patch is a replay and does not consult `expected_gen`. A different
    /// patch or another identity fails closed. `expected_gen` is the live
    /// instance generation the clerk read on a **new** id. Hidden instances
    /// stay hidden; recreate does not apply a prior overlay.
    pub fn apply_overlay(
        &mut self,
        patch: OverlayPatch,
        action_id: String,
        expected_gen: Option<u64>,
    ) -> Result<(), String> {
        self.apply_overlay_in_view(patch, action_id, expected_gen, &PropertyAcl::allow_all())
    }

    /// [`Self::apply_overlay`] honoring a request restriction (ADR 0014).
    pub fn apply_overlay_in_view(
        &mut self,
        mut patch: OverlayPatch,
        action_id: String,
        expected_gen: Option<u64>,
        acl: &PropertyAcl,
    ) -> Result<(), String> {
        Self::require_action_id(&action_id, "action id required")?;
        self.require_known_namespace(&patch.namespace)?;
        self.require_visible_in_view(&patch.namespace, &patch.kind, &patch.key, acl)?;
        for property in patch.props.keys() {
            acl.check(&patch.kind, property)
                .map_err(|err| err.to_string())?;
        }
        for property in &patch.cleared {
            acl.check(&patch.kind, property)
                .map_err(|err| err.to_string())?;
        }
        self.ensure_action_commits()?;
        if let Some(committed) = self
            .action_commits
            .get(&(patch.namespace.clone(), action_id.clone()))
            .cloned()
        {
            return self.replay_overlay(committed, &patch, &action_id);
        }
        let id = self.find_live(&patch.namespace, &patch.kind, &patch.key);
        self.require_expected_gen(&patch.namespace, &patch.kind, &patch.key, expected_gen)?;
        patch.action_id = Some(action_id.clone());
        let instance_hidden = id
            .and_then(|id| self.live_meta(id))
            .is_some_and(|meta| meta.hidden);
        let instance_exists = id.is_some_and(|id| self.live_meta(id).is_some());
        let source = if instance_exists && !instance_hidden {
            self.load(
                &patch.namespace,
                &patch.kind,
                &patch.key,
                &PropertyAcl::allow_all(),
            )?
            .props
        } else {
            HashMap::new()
        };
        if let Some(schema) = self.schema(&patch.namespace, &patch.kind)? {
            let plan = schema.conflict_plan();
            if plan.uses_newest_on(patch.props.keys().chain(patch.cleared.iter())) {
                patch
                    .canonical_at()?
                    .ok_or_else(|| "newest conflict requires overlay at".to_string())?;
            }
        }
        let rematerialized = patch.apply(source);
        self.append_uncommitted(patch.to_record()?)?;
        if !instance_hidden {
            self.write_uncommitted(
                ObjectRecord {
                    gen: 0,
                    namespace: patch.namespace,
                    kind: patch.kind,
                    key: patch.key,
                    hidden: false,
                    action_id: Some(action_id),
                    schema_version: 1,
                    props: rematerialized,
                },
                false,
                ActionIdWrite::Copy,
                true,
                false,
            )
            .map(|_| ())?;
        }
        self.commit()
    }

    /// [`Self::load`] then fail closed if the record violates `schema`.
    ///
    /// A deny list that omits a required property fails closed. Historical
    /// rows written before a descriptor still return from [`Self::load`].
    pub fn load_with_schema(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
        schema: &SchemaDescriptor,
    ) -> Result<ObjectRecord, String> {
        let record = self.load(namespace, kind, key, acl)?;
        schema.validate(&record)?;
        Ok(record)
    }

    pub fn append(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.append_uncommitted(record)?;
        self.commit()
    }

    /// Reserve intern capacity before a batch of new kind, key, property,
    /// value, or Action-id tokens. Unknown namespaces are left alone so a
    /// reserve cannot admit an empty namespace.
    pub fn reserve_intern(&mut self, namespace: &str, additional: usize) {
        if self.ensure_writable().is_err() {
            return;
        }
        if let Some(joins) = self.joins.get_mut(namespace) {
            joins.reserve(additional);
        }
    }

    /// Admit `record` the way [`Self::append_uncommitted`] does, without
    /// writing pages or updating live maps. Stream ingest push uses this so a
    /// later `load` / `evaluate` on this process cannot see a record a crash
    /// would lose (ADR 0040).
    pub fn admit_uncommitted(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.admit_uncommitted_buffered(record, true, false, 0)
            .map(|_| ())
    }

    /// Stream ingest admission. `merge_overlay` is the live overlay merge.
    /// `schema_follows_pending` treats a schema row as a replacement of a
    /// descriptor that is still only in the ingest buffer (ADR 0040).
    /// `owner` is the admitting stream; `0` is an unowned admit.
    /// `Ok(false)` is a committed Action replay: no write, do not buffer it.
    pub fn admit_uncommitted_buffered(
        &mut self,
        record: ObjectRecord,
        merge_overlay: bool,
        schema_follows_pending: bool,
        owner: u64,
    ) -> Result<bool, String> {
        if self.writer.keys().is_some() {
            self.writer.revalidate_catalog()?;
            self.drop_revoked_tenants();
        }
        self.admit_owner = owner;
        let result = self.write_uncommitted(
            record,
            merge_overlay,
            ActionIdWrite::Claim,
            false,
            schema_follows_pending,
        );
        self.admit_owner = 0;
        result
    }

    /// Fail closed when `record` cannot fit on a 4 KiB log page.
    pub fn check_record_fits(&self, record: &ObjectRecord) -> Result<(), String> {
        self.writer.check_record_fits(record)
    }

    /// Buffer a record on the writer and update live maps. Durability requires
    /// [`Self::commit`]: a crash before that leaves the uncommitted tail off
    /// the rebuild (ADR 0001). Stream apply and composition use this; a batch
    /// that must be all or nothing uses [`Self::append_batch`] (ADR 0026).
    ///
    /// A supplied Action id is unique in the namespace (ADR 0025, ADR 0028).
    /// The same id and the same body (`namespace`, `kind`, `key`, `hidden`,
    /// `props`) is a no-op. A remapped identity or a different body fails closed. A hidden record under a
    /// claimed id must copy the claimed `props`; its retry is a no-op. Source
    /// ingest may omit the id. Hide and overlay rematerialize copy provenance
    /// and do not claim a new id.
    ///
    /// Visible source writes of a non-hidden identity merge a visible
    /// `mikura.overlay` and validate a visible `mikura.schema`. Default
    /// merge is user-wins; a descriptor may select newest-timestamp-wins
    /// (ADR 0032). A visible `mikura.schema` replacement is checked against
    /// the previous visible descriptor (ADR 0013). Hide of the instance
    /// also hides that overlay. [`Self::apply_action`] uses
    /// [`Self::append_replace`] so it stays a whole-record replace.
    pub fn append_uncommitted(&mut self, record: ObjectRecord) -> Result<(), String> {
        if self.writer.keys().is_some() {
            self.writer.revalidate_catalog()?;
            self.drop_revoked_tenants();
        }
        self.write_uncommitted(record, true, ActionIdWrite::Claim, true, false)
            .map(|_| ())
    }

    /// Buffer a whole-record replace. Overlay merge is skipped; schema
    /// validation still runs on a visible instance.
    pub(crate) fn append_replace(&mut self, record: ObjectRecord) -> Result<(), String> {
        self.write_uncommitted(record, false, ActionIdWrite::Claim, true, false)
            .map(|_| ())
    }

    fn write_overlay_audit(
        &mut self,
        overlay: &OverlayPatch,
        audit: &ConflictAudit,
    ) -> Result<(), String> {
        self.write_uncommitted(
            overlay.to_audit_record(audit)?,
            false,
            ActionIdWrite::Copy,
            true,
            false,
        )
        .map(|_| ())
    }

    fn write_uncommitted(
        &mut self,
        mut record: ObjectRecord,
        merge_overlay: bool,
        action_id_write: ActionIdWrite,
        apply: bool,
        schema_follows_pending: bool,
    ) -> Result<bool, String> {
        self.ensure_writable()?;
        let claim_id = match action_id_write {
            ActionIdWrite::Claim => record.action_id.clone(),
            ActionIdWrite::Copy => None,
        };
        if let Some(id) = claim_id.as_deref().or(record.action_id.as_deref()) {
            Self::require_action_id(id, "empty action id")?;
        }
        require_namespace(&record.namespace)?;
        let source_props = record.props.clone();
        let mut overlay_audit = None;
        let mut migrated_overlays = Vec::new();
        if record.kind == SCHEMA_KIND {
            let next = SchemaDescriptor::from_record(&record)?;
            if let Some(id) = claim_id.as_deref() {
                self.ensure_action_commits()?;
                if let Some(committed) = self
                    .action_commits
                    .get(&(record.namespace.clone(), id.to_string()))
                    .cloned()
                {
                    if committed.kind == SCHEMA_KIND
                        && committed.key == record.key
                        && !committed.hidden
                        && !record.hidden
                    {
                        let stored = SchemaDescriptor::from_record(&ObjectRecord {
                            gen: committed.gen,
                            namespace: committed.namespace,
                            kind: SCHEMA_KIND.to_string(),
                            key: committed.key,
                            hidden: false,
                            action_id: Some(id.to_string()),
                            props: committed.props,
                            schema_version: 1,
                        })?;
                        let mut incoming = next.clone();
                        let mut stored = stored;
                        if incoming.version == 0 {
                            incoming.version = stored.effective_version();
                        }
                        stored.version = stored.effective_version();
                        if incoming == stored {
                            return Ok(false);
                        }
                    }
                }
            }
            if !record.hidden {
                self.refuse_conflicting_pending_schema(&record, &next, apply)?;
                if schema_follows_pending {
                    if !self.applying_admitted && !self.admitted_schema_eq(&record.namespace, &next)
                    {
                        self.pending_records_survive_schema(&record.namespace, &next)?;
                    }
                    record.props = next.to_record(record.namespace.clone())?.props;
                } else if let Some(previous) = self.load_reserved(
                    &record.namespace,
                    SCHEMA_KIND,
                    &record.key,
                    SchemaDescriptor::from_record,
                )? {
                    let next = previous.check_replacement(&next)?;
                    migrated_overlays = self.plan_overlay_migrations(&record.namespace, &next)?;
                    self.plan_instance_migrations(&record.namespace, &next)?;
                    if !self.applying_admitted && !self.admitted_schema_eq(&record.namespace, &next)
                    {
                        self.pending_records_survive_schema(&record.namespace, &next)?;
                    }
                    record.props = next.to_record(record.namespace.clone())?.props;
                } else if let Some(previous) = self
                    .schema_history
                    .get(&(record.namespace.clone(), record.key.clone()))
                    .and_then(|rows| rows.last())
                    .cloned()
                {
                    let next = previous.check_replacement(&next)?;
                    migrated_overlays = self.plan_overlay_migrations(&record.namespace, &next)?;
                    self.plan_instance_migrations(&record.namespace, &next)?;
                    if !self.applying_admitted && !self.admitted_schema_eq(&record.namespace, &next)
                    {
                        self.pending_records_survive_schema(&record.namespace, &next)?;
                    }
                    record.props = next.to_record(record.namespace.clone())?.props;
                } else if !next.migration.is_empty() {
                    return Err(format!("migrate_* is not valid on first {}", next.kind));
                } else if next.version > 1 {
                    return Err(format!(
                        "schema version {} must be 1 on first {}",
                        next.version, next.kind
                    ));
                } else {
                    if !self.applying_admitted && !self.admitted_schema_eq(&record.namespace, &next)
                    {
                        let mut first = next.clone();
                        if first.version == 0 {
                            first.version = 1;
                        }
                        self.pending_records_survive_schema(&record.namespace, &first)?;
                    }
                    if next.version == 0 {
                        let mut first = next;
                        first.version = 1;
                        record.props = first.to_record(record.namespace.clone())?.props;
                    }
                }
            }
        } else if record.kind == OVERLAY_KIND {
            OverlayPatch::from_record(&record)?;
            if apply && matches!(action_id_write, ActionIdWrite::Claim) && !self.applying_admitted {
                self.refuse_overlay_conflict_with_pending(&record)?;
                self.pending_schemas_admit_overlay(&record)?;
            }
        } else if record.kind == SOURCE_KIND {
            require_source_cursor(&record)?;
        } else if record.kind == CHANGE_SET_KIND {
            return Err(format!(
                "{CHANGE_SET_KIND} is written only by apply_change_set"
            ));
        } else if !record.hidden {
            overlay_audit =
                self.resolve_instance(&mut record, merge_overlay, !schema_follows_pending)?;
            if !self.applying_admitted {
                self.pending_schemas_admit_instance(&record)?;
            }
        }
        if let Some(id) = claim_id.as_deref() {
            if matches!(
                self.action_claim(
                    &record.namespace,
                    id,
                    &record.kind,
                    &record.key,
                    record.hidden,
                    &record.props
                )?,
                ActionClaim::Replay
            ) {
                let claim_key = (record.namespace.clone(), id.to_string());
                let pending_only = self.pending_action_claims.contains_key(&claim_key)
                    && !self.action_commits.contains_key(&claim_key);
                if !pending_only {
                    return Ok(false);
                }
            }
        }
        if apply && record.hidden {
            if let Some(id) = claim_id.as_deref() {
                let claim_key = (record.namespace.clone(), id.to_string());
                if let Some(reserved) = self.pending_action_claims.get(&claim_key) {
                    if !reserved.hidden && !self.action_commits.contains_key(&claim_key) {
                        return Err(format!(
                            "action id {id} already committed on {}/{}/{}",
                            reserved.namespace, reserved.kind, reserved.key
                        ));
                    }
                }
            }
        }
        let hide_overlay =
            record.hidden && record.kind != OVERLAY_KIND && record.kind != SCHEMA_KIND;
        let overlay_target = if hide_overlay {
            Some((
                record.namespace.clone(),
                record.kind.clone(),
                record.key.clone(),
            ))
        } else {
            None
        };
        self.assign_gen(&mut record);
        self.writer.check_record_fits(&record)?;
        for overlay in &migrated_overlays {
            self.writer.check_record_fits(overlay)?;
        }
        if let Some((overlay, audit)) = &overlay_audit {
            self.writer
                .check_record_fits(&overlay.to_audit_record(audit)?)?;
        }
        self.writer.check_tenant_writable_for(&record)?;
        if !apply {
            if claim_id.is_some() {
                self.reserve_action(&record);
            }
            self.remember_pending_record(&record, source_props);
            return Ok(true);
        }
        let held = self.writer.holds_commits();
        if !migrated_overlays.is_empty() {
            self.writer.hold_commits(true);
        }
        self.writer.append_record(&record)?;
        let schema_namespace = record.namespace.clone();
        let schema_kind = record.kind.clone();
        let schema_key = record.key.clone();
        self.note_applied_pending(&record);
        self.apply_record(record)?;
        self.maybe_refresh_schema(&schema_namespace, &schema_kind, &schema_key)?;
        let overlay_result = migrated_overlays.into_iter().try_for_each(|overlay| {
            self.write_uncommitted(overlay, false, ActionIdWrite::Copy, true, false)
                .map(|_| ())
        });
        if !held {
            self.writer.hold_commits(false);
        }
        if let Err(error) = overlay_result {
            self.abort_uncommitted()?;
            return Err(error);
        }
        if let Some((overlay, audit)) = overlay_audit {
            self.write_overlay_audit(&overlay, &audit)?;
        }
        if let Some((namespace, kind, key)) = overlay_target {
            if self.overlay(&namespace, &kind, &key)?.is_some() {
                let mut hide = OverlayPatch {
                    namespace,
                    kind,
                    key,
                    ..Default::default()
                }
                .to_record()?;
                hide.hidden = true;
                self.append_uncommitted(hide)?;
            }
        }
        Ok(true)
    }

    /// Merge a visible overlay into a visible, non-reserved source `record`
    /// and canonicalize and validate it against its kind's schema. Returns
    /// the overlay and its conflict audit when the merge must log one. Reads
    /// the live projection only, so the split commit path can admit records
    /// under shared access (#261).
    fn resolve_instance(
        &self,
        record: &mut ObjectRecord,
        merge_overlay: bool,
        use_live_schema: bool,
    ) -> Result<Option<(OverlayPatch, ConflictAudit)>, String> {
        let was_hidden = self
            .find_live(&record.namespace, &record.kind, &record.key)
            .and_then(|id| self.live_meta(id))
            .is_some_and(|meta| meta.hidden);
        let schema = if use_live_schema {
            self.load_reserved(
                &record.namespace,
                SCHEMA_KIND,
                &record.kind,
                SchemaDescriptor::from_record,
            )?
        } else {
            None
        };
        let pending_plan = if schema.is_none() && !use_live_schema {
            self.last_pending_schema(&record.namespace, &record.kind)?
                .map(|schema| schema.conflict_plan())
        } else {
            None
        };
        let mut overlay_audit = None;
        if !was_hidden && merge_overlay {
            if let Some(overlay) = self.load_reserved(
                &record.namespace,
                OVERLAY_KIND,
                &OverlayPatch::identity_key(&record.kind, &record.key),
                OverlayPatch::from_record,
            )? {
                let live_plan = schema.as_ref().map(SchemaDescriptor::conflict_plan);
                let plan = live_plan.as_ref().or(pending_plan.as_ref());
                let (merged, audit) =
                    overlay.apply_with(std::mem::take(&mut record.props), plan)?;
                record.props = merged;
                if record.action_id.is_none() {
                    record.action_id = overlay.action_id.clone();
                }
                if let Some(audit) = audit {
                    overlay_audit = Some((overlay, audit));
                }
            }
        }
        if let Some(schema) = schema {
            record.schema_version = schema.effective_version();
            schema.canonicalize_instance(record)?;
            schema.validate(record)?;
        }
        Ok(overlay_audit)
    }

    /// Next generation of `record`'s identity: one past the live generation,
    /// or the record's own (at least 1) for a new identity.
    fn assign_gen(&self, record: &mut ObjectRecord) {
        let live = self
            .find_live(&record.namespace, &record.kind, &record.key)
            .and_then(|id| self.live_meta(id));
        if let Some(existing) = live {
            record.gen = existing.gen.max(1) + 1;
        } else if record.gen == 0 {
            record.gen = 1;
        }
    }

    /// Group-commit the current writer pages. The join sidecar is persisted
    /// every 32 commits, every 100 ms, on close, or when no sidecar exists yet.
    /// The log stays authority; a stale sidecar is caught up on open.
    pub fn commit(&mut self) -> Result<(), String> {
        self.writer.flush()?;
        self.release_applied_action_claims();
        self.release_applied_pending();
        self.sidecar_commits_since_persist = self.sidecar_commits_since_persist.saturating_add(1);
        self.maybe_persist_projection(false)
    }

    /// While held, sealed pages do not advance the superblock. Stream apply
    /// holds from the first pending change until flush so records and the
    /// source cursor become durable together.
    pub fn hold_commits(&mut self, hold: bool) {
        self.writer.hold_commits(hold);
    }

    /// True when the writer has pages or a partial page the superblock does
    /// not cover yet.
    pub fn has_pending(&self) -> bool {
        self.writer.has_pending()
    }

    /// Append `records` and commit them as one unit (ADR 0026).
    ///
    /// An error from admitting a record, or from writing the log commit,
    /// drops the whole batch: no record of it stays live or becomes durable,
    /// and the store equals its committed log again. Group commit is held
    /// until the batch ends, so a large batch is never partly durable.
    /// Records already buffered by [`Self::append_uncommitted`] are committed
    /// first, so the abort never discards work the caller did not hand in.
    ///
    /// A failed superblock write leaves the commit point unknown: the store
    /// then keeps the batch as is, refuses further writes, and must be
    /// reopened, because rolling back could delete pages the log references.
    /// A rollback that itself fails has the same effect, so a half-rebuilt
    /// projection is never committed as a checkpoint.
    ///
    /// Once the log commit succeeded the batch is durable. Sidecar persist
    /// may wait for later commits, an interval, or close. A failure to
    /// persist the derived sidecar after that is reported as such and must not
    /// be retried as a failed batch; the next open rebuilds the sidecar.
    pub fn append_batch(&mut self, records: Vec<ObjectRecord>) -> Result<(), String> {
        self.atomically(|store| {
            records
                .into_iter()
                .try_for_each(|record| store.append_uncommitted(record))
        })
    }

    /// Apply records previously admitted by stream push. Intervening writers
    /// use [`Self::append_batch`]; this path is the owning buffer's flush so
    /// a buffered schema can land without looking like a conflicting later
    /// write (ADR 0040).
    pub fn append_admitted_batch(&mut self, records: Vec<ObjectRecord>) -> Result<(), String> {
        self.applying_admitted = true;
        let result = self.append_batch(records);
        self.applying_admitted = false;
        result
    }

    /// Run `append` with group commit held and commit its writes as one unit,
    /// with the abort and poison rules of [`Self::append_batch`].
    fn atomically(
        &mut self,
        append: impl FnOnce(&mut Self) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.writer.has_pending() {
            self.commit()?;
        }
        self.writer.hold_commits(true);
        let appended = append(self);
        self.writer.hold_commits(false);
        let committed = appended.and_then(|()| self.writer.flush());
        if let Err(error) = committed {
            if self.writer.is_failed() {
                return Err(error);
            }
            return Err(match self.abort_uncommitted() {
                Ok(()) => error,
                Err(abort) => {
                    self.writer.poison();
                    format!("{error}; rollback failed, reopen the store: {abort}")
                }
            });
        }
        self.release_applied_action_claims();
        self.release_applied_pending();
        self.sidecar_commits_since_persist = self.sidecar_commits_since_persist.saturating_add(1);
        self.maybe_persist_projection(false).map_err(|error| {
            format!("committed to the log; projection sidecar not persisted: {error}")
        })
    }

    fn release_applied_action_claims(&mut self) {
        self.pending_action_claims
            .retain(|key, _| !self.action_commits.contains_key(key));
    }

    fn remember_pending_record(
        &mut self,
        record: &ObjectRecord,
        source_props: HashMap<String, String>,
    ) {
        self.pending_admitted.push(record.clone());
        self.pending_source_props.push(source_props);
        self.pending_owners.push(self.admit_owner);
    }

    fn note_applied_pending(&mut self, record: &ObjectRecord) {
        if record.action_id.is_none() {
            return;
        }
        for (index, pending) in self.pending_admitted.iter().enumerate() {
            if self.applied_pending.contains(&index)
                || pending.namespace != record.namespace
                || pending.kind != record.kind
                || pending.key != record.key
            {
                continue;
            }
            if Self::pending_write_fulfills(pending, record) {
                self.applied_pending.insert(index);
                return;
            }
        }
    }

    /// Remember that `owner` was dropped without [`Self::release_admitted`].
    /// A later flush or discard of the same body can then drop leftover fences
    /// from that stream without touching a live sibling (ADR 0040).
    pub fn abandon_stream_owner(owner: u64) {
        if owner != 0 {
            abandoned_stream_owners().insert(owner);
        }
    }

    fn owner_is_abandoned(owner: u64) -> bool {
        owner != 0 && abandoned_stream_owners().contains(&owner)
    }

    /// Drop Store fences for records a stream is about to forget. Releases
    /// this `owner`'s matching entries and leftover entries from dropped
    /// streams of the same body. A live sibling of the same body keeps its
    /// fence (ADR 0040).
    pub fn release_admitted(&mut self, records: &[ObjectRecord], owner: u64) {
        for record in records {
            let indexes: Vec<usize> = self
                .pending_admitted
                .iter()
                .enumerate()
                .filter(|(index, _)| {
                    !self.applied_pending.contains(index)
                        && self.pending_release_match(*index, record)
                        && self
                            .pending_owners
                            .get(*index)
                            .is_some_and(|pending_owner| {
                                *pending_owner == owner || Self::owner_is_abandoned(*pending_owner)
                            })
                })
                .map(|(index, _)| index)
                .collect();
            self.applied_pending.extend(indexes);
            if let Some(id) = record.action_id.as_deref() {
                let key = (record.namespace.clone(), id.to_string());
                if !self.action_commits.contains_key(&key) {
                    let still_claimed =
                        self.pending_admitted
                            .iter()
                            .enumerate()
                            .any(|(i, pending)| {
                                !self.applied_pending.contains(&i)
                                    && pending.namespace == record.namespace
                                    && pending.action_id.as_deref() == Some(id)
                            });
                    if !still_claimed {
                        self.pending_action_claims.remove(&key);
                    }
                }
            }
        }
        self.release_applied_pending();
    }

    fn pending_write_fulfills(pending: &ObjectRecord, record: &ObjectRecord) -> bool {
        Self::same_admission(pending, record)
            && match (&pending.action_id, &record.action_id) {
                (Some(_), Some(_)) => true,
                (None, None) => pending.props == record.props,
                _ => false,
            }
    }

    fn same_admission(pending: &ObjectRecord, record: &ObjectRecord) -> bool {
        pending.namespace == record.namespace
            && pending.kind == record.kind
            && pending.key == record.key
            && pending.hidden == record.hidden
            && pending.action_id == record.action_id
    }

    fn pending_release_match(&self, index: usize, record: &ObjectRecord) -> bool {
        let Some(pending) = self.pending_admitted.get(index) else {
            return false;
        };
        if !Self::same_admission(pending, record) {
            return false;
        }
        if record.action_id.is_some() {
            return true;
        }
        pending.props == record.props
            || self
                .pending_source_props
                .get(index)
                .is_some_and(|source| source == &record.props)
    }

    fn release_applied_pending(&mut self) {
        let mut indexes: Vec<usize> = self.applied_pending.drain().collect();
        indexes.sort_unstable();
        for index in indexes.into_iter().rev() {
            if index < self.pending_admitted.len() {
                self.pending_admitted.remove(index);
                if index < self.pending_source_props.len() {
                    self.pending_source_props.remove(index);
                }
                if index < self.pending_owners.len() {
                    self.pending_owners.remove(index);
                }
            }
        }
    }

    /// Drop every uncommitted record and rebuild the live projection from the
    /// committed log, as a reopen after a crash would (ADR 0001).
    fn abort_uncommitted(&mut self) -> Result<(), String> {
        self.writer.discard_pending()?;
        self.identity.clear();
        self.hidden_props.clear();
        self.joins.clear();
        self.ns.clear();
        self.ns_ix.clear();
        self.dirty.clear();
        self.has_checkpoint = false;
        self.delta_bytes = 0;
        self.action_commits.clear();
        self.applied_pending.clear();
        self.schema_history.clear();
        self.schema_history_from_checkpoint = false;
        self.action_commits_from_log = true;
        self.paged = None;
        self.drop_paged_scratch();
        self.install_projection()?;
        self.drop_revoked_tenants();
        self.ensure_schema_history()?;
        if self.serving_budget.is_some() {
            self.adopt_paged_serving()?;
        }
        self.sidecar_commits_since_persist = 0;
        self.sidecar_last_persist = Instant::now();
        Ok(())
    }

    /// Writer `fsync` count. Used by ingest tests to characterize group commit.
    pub fn log_fsync_count(&self) -> u64 {
        self.writer.fsync_count()
    }

    /// Storage-call counts on the swap-committed file backend (ADR 0047 §10).
    pub fn storage_calls(&self) -> crate::StorageCalls {
        self.writer.storage_calls()
    }

    /// Whole-record replace with a clerk Action id (ADR 0006).
    ///
    /// A repeated id with the same body (`namespace`, `kind`, `key`,
    /// `hidden = false`, `props`) is a replay: no append (ADR 0011). The
    /// same id with a different body or on another identity fails closed. A
    /// replay whose body is illegal under the current descriptor also fails
    /// closed (ADR 0013). Ids are unique per namespace; lookup rebuilds from
    /// the log. `expected_gen` still applies to a new id. A legal replay that
    /// already committed does not append.
    pub fn apply_action(
        &mut self,
        action: Action,
        expected_gen: Option<u64>,
    ) -> Result<(), String> {
        self.apply_action_in_view(action, expected_gen, &PropertyAcl::allow_all())
    }

    /// [`Self::apply_action`] honoring a request restriction (ADR 0014).
    pub fn apply_action_in_view(
        &mut self,
        action: Action,
        expected_gen: Option<u64>,
        acl: &PropertyAcl,
    ) -> Result<(), String> {
        Self::require_action_id(&action.id, "action id required")?;
        self.require_known_namespace(&action.namespace)?;
        self.require_visible_in_view(&action.namespace, &action.kind, &action.key, acl)?;
        for property in action.props.keys() {
            acl.check(&action.kind, property)
                .map_err(|err| err.to_string())?;
        }
        match self.action_claim(
            &action.namespace,
            &action.id,
            &action.kind,
            &action.key,
            false,
            &action.props,
        )? {
            ActionClaim::Replay => {
                let claim_key = (action.namespace.clone(), action.id.clone());
                let fulfill_reservation = self.pending_action_claims.contains_key(&claim_key)
                    && !self.action_commits.contains_key(&claim_key);
                if !fulfill_reservation {
                    return Ok(());
                }
            }
            ActionClaim::New => {}
        }
        self.require_expected_gen(&action.namespace, &action.kind, &action.key, expected_gen)?;
        self.append_replace(ObjectRecord {
            gen: 0,
            namespace: action.namespace,
            kind: action.kind,
            key: action.key,
            hidden: false,
            action_id: Some(action.id),
            schema_version: 1,
            props: action.props,
        })?;
        self.commit()
    }

    /// Hide `(namespace, kind, key)` from evaluate. [`Self::load`] still
    /// returns the last payload. Missing identity or namespace fails closed.
    /// Denied properties this hide would copy fail closed. Overlay rows stay
    /// on the log under the existing instance-hide rule.
    pub fn hide(
        &mut self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
    ) -> Result<(), String> {
        self.require_known_namespace(namespace)?;
        self.require_visible_in_view(namespace, kind, key, acl)?;
        let current = self.load(namespace, kind, key, &PropertyAcl::allow_all())?;
        for property in current.props.keys() {
            acl.check(kind, property)
                .map_err(|err| format!("{err:?}"))?;
        }
        self.write_uncommitted(
            ObjectRecord {
                gen: 0,
                namespace: current.namespace,
                kind: current.kind,
                key: current.key,
                hidden: true,
                action_id: current.action_id,
                schema_version: 1,
                props: current.props,
            },
            true,
            ActionIdWrite::Copy,
            true,
            false,
        )
        .map(|_| ())?;
        self.commit()
    }

    /// Commit every edit of `set` under its Action id, all or nothing
    /// (ADR 0033).
    ///
    /// Edits name distinct, non-reserved identities of one namespace, at most
    /// [`CHANGE_SET_BOUND`]. A `mikura.change_set/<id>` marker opens the set
    /// on the log and members carry the id as provenance. An invisible
    /// member, a denied property, a stale `expected_gen`, or a failed write
    /// rejects the whole set and leaves the store at its committed log, with
    /// the abort rules of [`Self::append_batch`]. The same id with the same
    /// edits is a replay and does not consult `expected_gen`. A different
    /// body, or an id already used by another write, fails closed.
    pub fn apply_change_set(&mut self, set: ChangeSet) -> Result<(), String> {
        self.apply_change_set_in_view(set, &PropertyAcl::allow_all())
    }

    /// [`Self::apply_change_set`] honoring a request restriction (ADR 0014).
    pub fn apply_change_set_in_view(
        &mut self,
        set: ChangeSet,
        acl: &PropertyAcl,
    ) -> Result<(), String> {
        let ChangeSet {
            id,
            namespace,
            edits,
        } = set;
        Self::require_action_id(&id, "action id required")?;
        self.require_known_namespace(&namespace)?;
        if edits.is_empty() {
            return Err("change set has no edits".into());
        }
        if edits.len() > CHANGE_SET_BOUND {
            return Err(format!("change set exceeds {CHANGE_SET_BOUND} edits"));
        }
        let mut members = HashSet::with_capacity(edits.len());
        for edit in &edits {
            let (kind, key) = (edit.kind(), edit.key());
            if kind.starts_with("mikura.") {
                return Err(format!("change set cannot edit reserved kind {kind}"));
            }
            if !members.insert((kind, key)) {
                return Err(format!("change set edits {kind}/{key} twice"));
            }
            self.require_visible_in_view(&namespace, kind, key, acl)?;
            // A hide copies the stored payload, so every copied property must
            // be allowed, as on `hide`.
            let checked = match edit {
                Edit::Put { props, .. } => props.keys().cloned().collect::<Vec<_>>(),
                Edit::Hide { .. } => self
                    .load(&namespace, kind, key, &PropertyAcl::allow_all())?
                    .props
                    .into_keys()
                    .collect(),
            };
            for property in &checked {
                acl.check(kind, property).map_err(|err| err.to_string())?;
            }
        }
        self.ensure_action_commits()?;
        let claim_key = (namespace.clone(), id.clone());
        if let Some(committed) = self.action_commits.get(&claim_key) {
            return self.replay_change_set(committed, &edits, &id);
        }
        if let Some(reserved) = self.pending_action_claims.get(&claim_key) {
            return Err(format!(
                "action id {id} already committed on {}/{}/{}",
                reserved.namespace, reserved.kind, reserved.key
            ));
        }
        for edit in &edits {
            self.require_expected_gen(&namespace, edit.kind(), edit.key(), edit.expected_gen())?;
        }
        let marker = ObjectRecord {
            gen: 1,
            namespace: namespace.clone(),
            kind: CHANGE_SET_KIND.into(),
            key: id.clone(),
            hidden: false,
            action_id: Some(id.clone()),
            schema_version: 1,
            props: marker_props(&edits),
        };
        self.atomically(|store| {
            store
                .writer
                .check_record_fits(&marker)
                .map_err(|error| format!("change set too large: {error}"))?;
            store.writer.append_record(&marker)?;
            store.apply_record(marker)?;
            for edit in edits {
                let record = match edit {
                    Edit::Put {
                        kind, key, props, ..
                    } => ObjectRecord {
                        gen: 0,
                        namespace: namespace.clone(),
                        kind,
                        key,
                        hidden: false,
                        action_id: Some(id.clone()),
                        schema_version: 1,
                        props,
                    },
                    Edit::Hide { kind, key, .. } => ObjectRecord {
                        gen: 0,
                        hidden: true,
                        action_id: Some(id.clone()),
                        ..store.load(&namespace, &kind, &key, &PropertyAcl::allow_all())?
                    },
                };
                store
                    .write_uncommitted(record, false, ActionIdWrite::Copy, true, false)
                    .map(|_| ())?;
            }
            Ok(())
        })
    }

    /// A committed change-set id replays only with the same edits in the same
    /// order. Put bodies compare after the current descriptor canonicalizes
    /// them, as the stored bodies were; a put that descriptor rejects fails
    /// closed, as on `apply_action` (ADR 0013).
    fn replay_change_set(
        &self,
        committed: &ActionCommit,
        edits: &[Edit],
        id: &str,
    ) -> Result<(), String> {
        if committed.kind != CHANGE_SET_KIND {
            return Err(format!(
                "action id {id} already committed on {}/{}/{}",
                committed.namespace, committed.kind, committed.key
            ));
        }
        let conflict = || format!("action id {id} body conflict");
        if committed.members.len() != edits.len() {
            return Err(conflict());
        }
        for (member, edit) in committed.members.iter().zip(edits) {
            if member.kind != edit.kind() || member.key != edit.key() {
                return Err(conflict());
            }
            let props = match edit {
                Edit::Hide { .. } if member.hide => continue,
                Edit::Hide { .. } => return Err(conflict()),
                Edit::Put { .. } if member.hide => return Err(conflict()),
                Edit::Put { props, .. } => props,
            };
            let mut record = ObjectRecord {
                gen: 0,
                namespace: committed.namespace.clone(),
                kind: member.kind.clone(),
                key: member.key.clone(),
                hidden: false,
                action_id: Some(id.to_string()),
                schema_version: 1,
                props: props.clone(),
            };
            if let Some(schema) = self.schema(&committed.namespace, &member.kind)? {
                schema.canonicalize_instance(&mut record)?;
                schema.validate(&record)?;
            }
            if member.props.as_ref() != Some(&record.props) {
                return Err(conflict());
            }
        }
        Ok(())
    }

    /// Join maps for a known namespace. Missing or unknown namespace fails
    /// closed.
    pub fn joins(&self, namespace: &str) -> Result<&JoinMaps, String> {
        self.joins_in(self.require_known_namespace(namespace)?)
    }

    /// Superblock `committed_pages`. Snapshot cursors bind this value
    /// together with the uncommitted writer head.
    pub fn committed_pages(&self) -> u32 {
        self.writer.committed_pages()
    }

    /// Snapshot version this process last adopted. Zero on a superblock log.
    pub fn manifest_version(&self) -> u64 {
        self.writer.snapshot_version()
    }

    /// Highest snapshot currently named in the object namespace.
    pub fn current_manifest_version(&self) -> Result<u64, String> {
        Ok(self
            .writer
            .live_manifest()?
            .map(|manifest| manifest.version)
            .unwrap_or(0))
    }

    pub fn is_read_only(&self) -> bool {
        self.writer.is_read_only()
    }

    pub fn is_fenced(&self) -> bool {
        self.writer.is_fenced()
    }

    /// Adopt a later snapshot and apply its new pages to the live maps.
    /// No-op when this process already serves the current version.
    pub fn follow(&mut self) -> Result<(), String> {
        let Some(manifest) = self.writer.live_manifest()? else {
            return Ok(());
        };
        if manifest.version <= self.writer.snapshot_version() {
            return Ok(());
        }
        let after = self.writer.committed_pages();
        self.writer.adopt_snapshot(manifest);
        for record in self.writer.records_after(&self.log, after)? {
            self.apply_logged(record)?;
        }
        self.dirty.clear();
        Ok(())
    }

    /// Wait until this process serves `min` or `bound` elapses. Never
    /// answers from an older snapshot (ADR 0047 §5).
    pub fn wait_for_version(&mut self, min: u64, bound: Duration) -> Result<(), String> {
        let deadline = Instant::now() + bound;
        loop {
            self.follow()?;
            if self.manifest_version() >= min {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "minimum manifest version {min} not reached (serving {})",
                    self.manifest_version()
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Take the writer role. The successful claim swap fences the previous
    /// writer. While still read-only, a lost swap is retried until `bound`.
    pub fn promote(&mut self, bound: Duration) -> Result<(), String> {
        if self.writer.is_fenced() {
            return Err(format!(
                "{}: writer lost the manifest swap",
                crate::LOG_FENCED
            ));
        }
        if !self.writer.is_read_only() {
            return Ok(());
        }
        self.writer.probe_for_write()?;
        let deadline = Instant::now() + bound;
        loop {
            self.follow()?;
            if self.writer.claim_writer()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("promote did not win the manifest swap".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// `(committed_pages, written_pages, current page used)`. Cursors bind
    /// this so a write that reaches the log invalidates the snapshot.
    pub fn snapshot_stamp(&self) -> (u32, u32, u32) {
        (
            self.writer.committed_pages(),
            self.writer.written_pages(),
            self.writer.page_used() as u32,
        )
    }

    /// User-facing mutations require the current identity to be in the view.
    /// Row filters read the live projection, so a hidden or missing identity
    /// of a filtered kind is not in the view.
    fn require_visible_in_view(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
    ) -> Result<(), String> {
        let maps = self.joins_in(namespace)?;
        acl.require_visible(kind, key, |property| {
            if self.omits_current_property(namespace, kind, property) {
                None
            } else {
                maps.prop(kind, key, property)
            }
        })
        .map_err(|err| err.to_string())
    }

    /// Live object projected to `as_of` schema version, or the current
    /// version when `as_of` is `None` (ADR 0035).
    pub fn load_at_schema(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
        as_of: u32,
    ) -> Result<ObjectRecord, String> {
        let mut record = self.load_stored(namespace, kind, key, acl)?;
        self.apply_schema_view(&mut record, Some(as_of))?;
        if !acl.object_visible(kind, key, |property| {
            record.props.get(property).map(String::as_str)
        }) {
            return Err(format!("unknown identity {namespace}/{kind}/{key}"));
        }
        record.props = acl.omit_denied(&record.kind, record.props);
        Ok(record)
    }

    fn load_stored(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
    ) -> Result<ObjectRecord, String> {
        let namespace = self.require_known_namespace(namespace)?;
        let missing = || format!("unknown identity {namespace}/{kind}/{key}");
        if let Ok(tenant) = tenant_of_namespace(namespace) {
            if !self.writer.tenant_key_usable(tenant) {
                return Err(missing());
            }
        }
        let id = match self.find_live(namespace, kind, key) {
            Some(id) => id,
            None => {
                self.paged_fail_closed()?;
                return Err(missing());
            }
        };
        let meta = match self.live_meta(id) {
            Some(meta) => meta,
            None => {
                self.paged_fail_closed()?;
                return Err(missing());
            }
        };
        let maps = self.joins_in(namespace)?;
        let _ = acl;
        let props = if meta.hidden {
            self.hidden_owned(id)
                .map(|owned| Self::materialize_owned(maps, &owned))
                .unwrap_or_default()
        } else {
            maps.row_props(kind, key)
        };
        self.paged_fail_closed()?;
        Ok(ObjectRecord {
            gen: meta.gen,
            namespace: namespace.to_string(),
            kind: kind.to_string(),
            key: key.to_string(),
            hidden: meta.hidden,
            action_id: match meta.action_id {
                None => None,
                Some(intern) => Some(
                    maps.intern_get(intern)
                        .ok_or_else(|| "missing intern action".to_string())?
                        .to_string(),
                ),
            },
            props,
            schema_version: if meta.schema_version == 0 {
                1
            } else {
                meta.schema_version
            },
        })
    }

    fn ensure_schema_history(&mut self) -> Result<(), String> {
        self.rebuild_schema_history_from_log_if_empty()?;
        self.capture_schema_history_from_identity();
        Ok(())
    }

    fn rebuild_schema_history_from_log_if_empty(&mut self) -> Result<(), String> {
        if self.schema_history_from_checkpoint {
            return Ok(());
        }
        let has_schema = if self.paged.is_some() {
            self.live_ids()?
                .into_iter()
                .any(|id| self.live_kind(id) == Some(SCHEMA_KIND))
        } else {
            self.identity
                .keys()
                .any(|&id| self.live_kind(id) == Some(SCHEMA_KIND))
        };
        if !has_schema {
            return Ok(());
        }
        self.schema_history.clear();
        for record in read_records_with_keys(&self.log, self.writer.keys())? {
            if record.kind == SCHEMA_KIND && !record.hidden {
                if let Ok(desc) = SchemaDescriptor::from_record(&record) {
                    self.remember_schema(&record.namespace, desc);
                }
            }
        }
        self.schema_history_from_checkpoint = true;
        Ok(())
    }

    fn capture_schema_history_from_identity(&mut self) {
        let ids: Vec<_> = self
            .identity
            .iter()
            .filter(|(&id, meta)| self.live_kind(id) == Some(SCHEMA_KIND) && !meta.hidden)
            .map(|(&id, _)| id)
            .collect();
        for id in ids {
            let Ok((namespace, _, key)) = self.live_strings(id) else {
                continue;
            };
            let namespace = namespace.to_string();
            let key = key.to_string();
            if let Ok(Some(desc)) =
                self.load_reserved(&namespace, SCHEMA_KIND, &key, SchemaDescriptor::from_record)
            {
                self.remember_schema(&namespace, desc);
            }
        }
    }

    fn remember_schema(&mut self, namespace: &str, desc: SchemaDescriptor) {
        let key = (namespace.to_string(), desc.kind.clone());
        let rows = self.schema_history.entry(key).or_default();
        rows.retain(|row| row.effective_version() != desc.effective_version());
        rows.push(desc);
        rows.sort_by_key(|row| row.effective_version());
    }

    /// Apply `steps` in order to live overlays and instances. Stream ingest uses
    /// this so a buffered schema chain is checked against committed rows the
    /// way flush will apply them (ADR 0040). Overlay keys and instance keys
    /// already replaced in the buffer are skipped so superseded live bodies
    /// are not migrated.
    pub fn preflight_schema_chain(
        &self,
        namespace: &str,
        steps: &[SchemaDescriptor],
        skip_overlay_keys: &[String],
        skip_instance_keys: &[String],
    ) -> Result<(), String> {
        if steps.is_empty() {
            return Ok(());
        }
        let kind = &steps[0].kind;
        let prefix = format!("{kind}/");
        let ns_id = self.ns_ix.get(namespace).copied();
        let mut overlay_errors = Vec::new();
        let overlay_ids: Vec<_> = self
            .live_ids()?
            .into_iter()
            .filter(|&id| {
                Some(id.0) == ns_id
                    && self.live_kind(id) == Some(OVERLAY_KIND)
                    && self
                        .live_key(id)
                        .is_some_and(|key| key.starts_with(&prefix))
            })
            .collect();
        for id in overlay_ids {
            let Some(overlay_key) = self.live_key(id).map(str::to_string) else {
                continue;
            };
            if skip_overlay_keys.iter().any(|key| key == &overlay_key) {
                continue;
            }
            let Ok(live) = self.load_stored(
                namespace,
                OVERLAY_KIND,
                &overlay_key,
                &PropertyAcl::allow_all(),
            ) else {
                continue;
            };
            if live.hidden {
                continue;
            }
            let mut patch = OverlayPatch::from_record(&live)?;
            let mut encoded = live;
            let mut failed = false;
            for step in steps {
                if step.kind != *kind || step.migration.is_empty() {
                    continue;
                }
                if let Err(err) = Self::preflight_overlay_step(&mut patch, step) {
                    overlay_errors.push(err);
                    failed = true;
                    break;
                }
                encoded = match OverlayPatch::remapped_audit(
                    &encoded,
                    &step.migration.rename,
                    &step.migration.drop,
                ) {
                    Ok(Some(audit)) => match patch.to_audit_record(&audit) {
                        Ok(record) => record,
                        Err(err) => {
                            overlay_errors.push(err);
                            failed = true;
                            break;
                        }
                    },
                    Ok(None) => match patch.to_record() {
                        Ok(record) => record,
                        Err(err) => {
                            overlay_errors.push(err);
                            failed = true;
                            break;
                        }
                    },
                    Err(err) => {
                        overlay_errors.push(err);
                        failed = true;
                        break;
                    }
                };
            }
            if !failed {
                if let Err(err) = self.check_record_fits(&encoded) {
                    overlay_errors.push(err);
                }
            }
        }
        if !overlay_errors.is_empty() {
            return Err(format!(
                "cannot migrate overlays on {kind}: {}",
                overlay_errors.join("; ")
            ));
        }
        let mut instance_errors = Vec::new();
        let kind_id = self
            .joins
            .get(namespace)
            .and_then(|joins| joins.intern_existing(kind));
        let instance_ids: Vec<_> = self
            .live_ids()?
            .into_iter()
            .filter(|&id| Some(id.0) == ns_id && Some(id.1) == kind_id)
            .collect();
        for id in instance_ids {
            let Some(meta) = self.live_meta(id) else {
                continue;
            };
            if skip_instance_keys
                .iter()
                .any(|key| self.live_key(id) == Some(key.as_str()))
            {
                continue;
            }
            let ident = self
                .live_strings(id)
                .map(|(ns, kind, key)| format!("{ns}/{kind}/{key}"))
                .unwrap_or_else(|_| format!("{}/{}/{}", id.0, id.1, id.2));
            let mut props = if meta.hidden {
                self.hidden_owned(id)
                    .and_then(|owned| {
                        self.joins
                            .get(namespace)
                            .map(|joins| Self::materialize_owned(joins, &owned))
                    })
                    .unwrap_or_default()
            } else {
                self.joins
                    .get(namespace)
                    .map(|joins| {
                        let kind = joins.intern_get(id.1).unwrap_or("");
                        let key = joins.intern_get(id.2).unwrap_or("");
                        joins.row_props(kind, key)
                    })
                    .unwrap_or_default()
            };
            for step in steps {
                if step.kind != *kind {
                    continue;
                }
                if step.migration.cast.is_empty() && step.migration.rename.is_empty() {
                    continue;
                }
                if let Err(err) = step.migration.apply(&mut props, false) {
                    instance_errors.push(format!("{ident} {err}"));
                    break;
                }
            }
        }
        if !instance_errors.is_empty() {
            return Err(format!(
                "cannot migrate instances on {kind}: {}",
                instance_errors.join("; ")
            ));
        }
        Ok(())
    }

    fn preflight_overlay_step(
        patch: &mut OverlayPatch,
        next: &SchemaDescriptor,
    ) -> Result<(), String> {
        let ident = format!("{}/{}/{}", patch.namespace, next.kind, patch.key);
        for name in patch.props.keys().chain(patch.cleared.iter()) {
            if next.migration.drop.iter().any(|dropped| dropped == name) {
                return Err(format!("overlay {ident} cannot carry dropped {name}"));
            }
        }
        next.migration
            .apply(&mut patch.props, false)
            .map_err(|err| format!("overlay {ident} {err}"))?;
        let mut cleared = Vec::new();
        for name in &patch.cleared {
            let renamed = next
                .migration
                .rename
                .iter()
                .find(|(old, _)| old == name)
                .map(|(_, new)| new.clone())
                .unwrap_or_else(|| name.clone());
            if !cleared.contains(&renamed) {
                cleared.push(renamed);
            }
        }
        patch.cleared = cleared;
        for name in patch.props.keys().chain(patch.cleared.iter()) {
            if !next.properties.iter().any(|property| property == name) {
                return Err(format!("overlay {ident} cannot carry {name}"));
            }
        }
        Ok(())
    }

    fn refuse_conflicting_pending_schema(
        &self,
        record: &ObjectRecord,
        next: &SchemaDescriptor,
        apply: bool,
    ) -> Result<(), String> {
        if self.applying_admitted {
            return Ok(());
        }
        let mut last = None;
        let mut identical = false;
        for (index, pending) in self.pending_admitted.iter().enumerate() {
            if self.applied_pending.contains(&index)
                || pending.hidden
                || pending.namespace != record.namespace
                || pending.kind != SCHEMA_KIND
                || pending.key != record.key
            {
                continue;
            }
            identical = identical || Self::pending_schema_eq(pending, next);
            last = Some(pending);
        }
        let Some(last) = last else {
            return Ok(());
        };
        if apply {
            return Err(format!(
                "schema {} already admitted for {}/{}",
                record.key, record.namespace, record.key
            ));
        }
        if identical {
            return Ok(());
        }
        let previous = SchemaDescriptor::from_record(last)?;
        if previous.check_replacement(next).is_ok() {
            return Ok(());
        }
        Err(format!(
            "schema {} already admitted for {}/{}",
            record.key, record.namespace, record.key
        ))
    }

    fn last_pending_schema(
        &self,
        namespace: &str,
        kind: &str,
    ) -> Result<Option<SchemaDescriptor>, String> {
        let mut last = None;
        for (index, pending) in self.pending_admitted.iter().enumerate() {
            if self.applied_pending.contains(&index)
                || pending.namespace != namespace
                || pending.kind != SCHEMA_KIND
                || pending.key != kind
            {
                continue;
            }
            if pending.hidden {
                last = None;
                continue;
            }
            last = Some(SchemaDescriptor::from_record(pending)?);
        }
        Ok(last)
    }

    fn admitted_schema_eq(&self, namespace: &str, next: &SchemaDescriptor) -> bool {
        self.pending_admitted
            .iter()
            .enumerate()
            .any(|(index, pending)| {
                !self.applied_pending.contains(&index)
                    && pending.namespace == namespace
                    && pending.kind == SCHEMA_KIND
                    && pending.key == next.kind
                    && !pending.hidden
                    && Self::pending_schema_eq(pending, next)
            })
    }

    fn refuse_overlay_conflict_with_pending(&self, record: &ObjectRecord) -> Result<(), String> {
        let patch = OverlayPatch::from_record(record)?;
        let schema = self.schema(&patch.namespace, &patch.kind).ok().flatten();
        for (index, pending) in self.pending_admitted.iter().enumerate() {
            if self.applied_pending.contains(&index)
                || pending.hidden
                || pending.kind == SCHEMA_KIND
                || pending.kind == OVERLAY_KIND
                || pending.kind == SOURCE_KIND
                || pending.kind == CHANGE_SET_KIND
                || pending.namespace != patch.namespace
                || pending.kind != patch.kind
                || pending.key != patch.key
            {
                continue;
            }
            let source = self
                .pending_source_props
                .get(index)
                .cloned()
                .unwrap_or_else(|| pending.props.clone());
            let plan = schema.as_ref().map(SchemaDescriptor::conflict_plan);
            let (flush_props, audit) = if record.hidden {
                (source, None)
            } else {
                patch.apply_with(source, plan.as_ref())?
            };
            if let Some(audit) = audit {
                self.check_record_fits(&patch.to_audit_record(&audit)?)?;
            }
            if pending.action_id.is_some() && flush_props != pending.props {
                return Err(format!(
                    "action id {} body conflict",
                    pending.action_id.as_deref().unwrap_or("")
                ));
            }
            if let Some(schema) = schema.as_ref() {
                let mut probe = pending.clone();
                probe.props = flush_props;
                schema.validate(&probe)?;
            }
        }
        Ok(())
    }

    /// Fail closed when an intervening overlay cannot be remapped by a
    /// schema still only in an ingest buffer (ADR 0040).
    fn pending_schemas_admit_overlay(&self, record: &ObjectRecord) -> Result<(), String> {
        if record.hidden {
            return Ok(());
        }
        let mut patch = OverlayPatch::from_record(record)?;
        let mut encoded = record.clone();
        let mut saw = false;
        for (index, pending) in self.pending_admitted.iter().enumerate() {
            if self.applied_pending.contains(&index)
                || pending.hidden
                || pending.namespace != patch.namespace
                || pending.kind != SCHEMA_KIND
                || pending.key != patch.kind
            {
                continue;
            }
            let schema = SchemaDescriptor::from_record(pending)?;
            if !schema.migration.is_empty() {
                if let Err(err) = Self::preflight_overlay_step(&mut patch, &schema) {
                    return Err(format!("cannot migrate overlays on {}: {err}", schema.kind));
                }
                encoded = match OverlayPatch::remapped_audit(
                    &encoded,
                    &schema.migration.rename,
                    &schema.migration.drop,
                )? {
                    Some(audit) => patch.to_audit_record(&audit)?,
                    None => patch.to_record()?,
                };
            } else {
                let ident = format!("{}/{}/{}", patch.namespace, schema.kind, patch.key);
                for name in patch.props.keys().chain(patch.cleared.iter()) {
                    if !schema.properties.iter().any(|property| property == name) {
                        return Err(format!(
                            "cannot migrate overlays on {}: overlay {ident} cannot carry {name}",
                            schema.kind
                        ));
                    }
                }
            }
            saw = true;
        }
        if saw {
            self.check_record_fits(&encoded)?;
        }
        Ok(())
    }

    /// Fail closed when an intervening instance would be invalid under a
    /// schema still only in an ingest buffer (ADR 0040).
    fn pending_schemas_admit_instance(&self, record: &ObjectRecord) -> Result<(), String> {
        let mut props = record.props.clone();
        let mut last = None;
        for (index, pending) in self.pending_admitted.iter().enumerate() {
            if self.applied_pending.contains(&index)
                || pending.hidden
                || pending.namespace != record.namespace
                || pending.kind != SCHEMA_KIND
                || pending.key != record.kind
            {
                continue;
            }
            let schema = SchemaDescriptor::from_record(pending)?;
            if !schema.migration.is_empty() {
                if let Err(err) = schema.migration.apply(&mut props, false) {
                    return Err(format!(
                        "cannot migrate instances on {}: {}/{}/{} {err}",
                        schema.kind, record.namespace, record.kind, record.key
                    ));
                }
            }
            last = Some(schema);
        }
        if let Some(schema) = last {
            let mut probe = record.clone();
            probe.props = props;
            schema.validate(&probe)?;
        }
        Ok(())
    }

    fn pending_schema_eq(pending: &ObjectRecord, next: &SchemaDescriptor) -> bool {
        let Ok(mut stored) = SchemaDescriptor::from_record(pending) else {
            return false;
        };
        let mut incoming = next.clone();
        if incoming.version == 0 {
            incoming.version = stored.effective_version();
        }
        stored.version = stored.effective_version();
        incoming == stored
    }

    /// Fail closed when a later schema replacement would rewrite or reject a
    /// record still only in an ingest buffer (ADR 0040). Live maps are
    /// remapped by the replacement; buffered bodies are written as admitted.
    fn pending_records_survive_schema(
        &self,
        namespace: &str,
        next: &SchemaDescriptor,
    ) -> Result<(), String> {
        let mut instance_errors = Vec::new();
        let mut overlay_errors = Vec::new();
        let overlay_prefix = format!("{}/", next.kind);
        for (index, record) in self.pending_admitted.iter().enumerate() {
            if record.namespace != namespace
                || record.hidden
                || self.applied_pending.contains(&index)
                || (self.admit_owner != 0
                    && self.pending_owners.get(index) == Some(&self.admit_owner))
            {
                continue;
            }
            if record.kind == next.kind {
                if !next.migration.is_empty() {
                    if let Err(err) = Self::pending_body_survives_schema(&record.props, next) {
                        instance_errors.push(format!(
                            "{}/{}/{} {err}",
                            record.namespace, record.kind, record.key
                        ));
                    }
                }
                if let Err(err) = next.validate(record) {
                    instance_errors.push(format!(
                        "{}/{}/{} {err}",
                        record.namespace, record.kind, record.key
                    ));
                }
            } else if record.kind == OVERLAY_KIND && record.key.starts_with(&overlay_prefix) {
                match OverlayPatch::from_record(record) {
                    Ok(patch) => {
                        if let Err(err) = Self::pending_overlay_survives_schema(&patch, next) {
                            overlay_errors.push(err);
                        }
                    }
                    Err(err) => overlay_errors.push(err),
                }
            }
        }
        if !overlay_errors.is_empty() {
            return Err(format!(
                "cannot migrate overlays on {}: {}",
                next.kind,
                overlay_errors.join("; ")
            ));
        }
        if !instance_errors.is_empty() {
            return Err(format!(
                "cannot migrate instances on {}: {}",
                next.kind,
                instance_errors.join("; ")
            ));
        }
        Ok(())
    }

    fn pending_body_survives_schema(
        props: &HashMap<String, String>,
        next: &SchemaDescriptor,
    ) -> Result<(), String> {
        for name in props.keys() {
            if next.migration.drop.iter().any(|dropped| dropped == name) {
                return Err(format!("cannot carry dropped {name}"));
            }
        }
        let mut props = props.clone();
        let before = props.clone();
        next.migration.apply(&mut props, false)?;
        if props != before {
            return Err("would rewrite a buffered record".into());
        }
        Ok(())
    }

    fn pending_overlay_survives_schema(
        patch: &OverlayPatch,
        next: &SchemaDescriptor,
    ) -> Result<(), String> {
        let ident = format!("{}/{}/{}", patch.namespace, next.kind, patch.key);
        for name in patch.props.keys().chain(patch.cleared.iter()) {
            if next.migration.drop.iter().any(|dropped| dropped == name) {
                return Err(format!("overlay {ident} cannot carry dropped {name}"));
            }
            if !next.properties.iter().any(|property| property == name) {
                return Err(format!("overlay {ident} cannot carry {name}"));
            }
        }
        if next.migration.is_empty() {
            return Ok(());
        }
        let mut props = patch.props.clone();
        let before = props.clone();
        next.migration
            .apply(&mut props, false)
            .map_err(|err| format!("overlay {ident} {err}"))?;
        if props != before {
            return Err(format!("overlay {ident} would rewrite a buffered overlay"));
        }
        let mut cleared = patch.cleared.clone();
        cleared.sort();
        let mut renamed = Vec::new();
        for name in &patch.cleared {
            let mapped = next
                .migration
                .rename
                .iter()
                .find(|(old, _)| old == name)
                .map(|(_, new)| new.clone())
                .unwrap_or_else(|| name.clone());
            if !renamed.contains(&mapped) {
                renamed.push(mapped);
            }
        }
        renamed.sort();
        if renamed != cleared {
            return Err(format!("overlay {ident} would rewrite a buffered overlay"));
        }
        Ok(())
    }

    fn plan_overlay_migrations(
        &self,
        namespace: &str,
        next: &SchemaDescriptor,
    ) -> Result<Vec<ObjectRecord>, String> {
        if next.migration.is_empty() {
            return Ok(Vec::new());
        }
        let prefix = format!("{}/", next.kind);
        let mut errors = Vec::new();
        let mut out = Vec::new();
        let ns_id = self.ns_ix.get(namespace).copied();
        let ids: Vec<_> = self
            .identity
            .keys()
            .copied()
            .filter(|&id| {
                Some(id.0) == ns_id
                    && self.live_kind(id) == Some(OVERLAY_KIND)
                    && self
                        .live_key(id)
                        .is_some_and(|key| key.starts_with(&prefix))
            })
            .collect();
        for id in ids {
            let Some(overlay_key) = self.live_key(id).map(str::to_string) else {
                continue;
            };
            let Ok(live) = self.load_stored(
                namespace,
                OVERLAY_KIND,
                &overlay_key,
                &PropertyAcl::allow_all(),
            ) else {
                continue;
            };
            if live.hidden {
                continue;
            }
            let mut patch = OverlayPatch::from_record(&live)?;
            let ident = format!("{namespace}/{}/{}", next.kind, patch.key);
            let original = patch.clone();
            let live_overlay = live;
            let dropped: HashSet<&str> = next.migration.drop.iter().map(String::as_str).collect();
            let allowed: HashSet<&str> = next.properties.iter().map(String::as_str).collect();
            let mut local = Vec::new();
            for name in patch.props.keys().chain(patch.cleared.iter()) {
                if dropped.contains(name.as_str()) {
                    local.push(format!("overlay {ident} cannot carry dropped {name}"));
                }
            }
            if local.is_empty() {
                if let Err(err) = next.migration.apply(&mut patch.props, false) {
                    local.push(format!("overlay {ident} {err}"));
                }
            }
            if local.is_empty() {
                let mut cleared = Vec::new();
                for name in &patch.cleared {
                    let renamed = next
                        .migration
                        .rename
                        .iter()
                        .find(|(old, _)| old == name)
                        .map(|(_, new)| new.clone())
                        .unwrap_or_else(|| name.clone());
                    if !cleared.contains(&renamed) {
                        cleared.push(renamed);
                    }
                }
                patch.cleared = cleared;
                for name in patch.props.keys().chain(patch.cleared.iter()) {
                    if !allowed.contains(name.as_str()) {
                        local.push(format!("overlay {ident} cannot carry {name}"));
                    }
                }
            }
            if !local.is_empty() {
                errors.extend(local);
                continue;
            }
            if patch == original {
                continue;
            }
            match (|| {
                let audit = OverlayPatch::remapped_audit(
                    &live_overlay,
                    &next.migration.rename,
                    &next.migration.drop,
                )?;
                match audit {
                    Some(audit) => patch.to_audit_record(&audit),
                    None => patch.to_record(),
                }
            })() {
                Ok(record) => out.push(record),
                Err(err) => errors.push(format!("overlay {ident} {err}")),
            }
        }
        if !errors.is_empty() {
            return Err(format!(
                "cannot migrate overlays on {}: {}",
                next.kind,
                errors.join("; ")
            ));
        }
        Ok(out)
    }

    fn plan_instance_migrations(
        &self,
        namespace: &str,
        next: &SchemaDescriptor,
    ) -> Result<(), String> {
        if next.migration.cast.is_empty() && next.migration.rename.is_empty() {
            return Ok(());
        }
        let mut errors = Vec::new();
        let ns_id = self.ns_ix.get(namespace).copied();
        let kind_id = self
            .joins
            .get(namespace)
            .and_then(|joins| joins.intern_existing(&next.kind));
        let ids: Vec<_> = self
            .identity
            .keys()
            .copied()
            .filter(|&id| Some(id.0) == ns_id && Some(id.1) == kind_id)
            .collect();
        for id in ids {
            let Some(meta) = self.identity.get(&id) else {
                continue;
            };
            let ident = self
                .live_strings(id)
                .map(|(ns, kind, key)| format!("{ns}/{kind}/{key}"))
                .unwrap_or_else(|_| format!("{}/{}/{}", id.0, id.1, id.2));
            let mut props = if meta.hidden {
                self.hidden_props
                    .get(&id)
                    .and_then(|owned| {
                        self.joins
                            .get(namespace)
                            .map(|joins| Self::materialize_owned(joins, owned))
                    })
                    .unwrap_or_default()
            } else {
                self.joins
                    .get(namespace)
                    .map(|joins| {
                        let kind = joins.intern_get(id.1).unwrap_or("");
                        let key = joins.intern_get(id.2).unwrap_or("");
                        joins.row_props(kind, key)
                    })
                    .unwrap_or_default()
            };
            if let Some(current) = self.current_kind_properties(namespace, &next.kind) {
                let current: HashSet<&str> = current.iter().map(String::as_str).collect();
                let mut keep = current.clone();
                for (from, to) in &next.migration.rename {
                    keep.insert(from.as_str());
                    if from != to && !current.contains(to.as_str()) && props.contains_key(to) {
                        errors.push(format!(
                            "{ident} rename destination {to} collides with a dropped property"
                        ));
                    }
                }
                props.retain(|name, _| keep.contains(name.as_str()));
            }
            if let Err(err) = next.migration.apply(&mut props, false) {
                errors.push(format!("{ident} {err}"));
            }
        }
        if !errors.is_empty() {
            return Err(format!(
                "cannot migrate instances on {}: {}",
                next.kind,
                errors.join("; ")
            ));
        }
        Ok(())
    }

    fn remap_kind(&mut self, namespace: &str, desc: &SchemaDescriptor) -> Result<(), String> {
        let ns_id = self.ns_ix.get(namespace).copied();
        let kind_id = self
            .joins
            .get(namespace)
            .and_then(|joins| joins.intern_existing(&desc.kind));
        let ids: Vec<_> = self
            .identity
            .keys()
            .copied()
            .filter(|&id| Some(id.0) == ns_id && Some(id.1) == kind_id)
            .collect();
        for id in ids {
            let Some(meta) = self.identity.get(&id).cloned() else {
                continue;
            };
            let mut props = if meta.hidden {
                self.hidden_props
                    .get(&id)
                    .and_then(|owned| {
                        self.joins
                            .get(namespace)
                            .map(|joins| Self::materialize_owned(joins, owned))
                    })
                    .unwrap_or_default()
            } else {
                self.joins
                    .get(namespace)
                    .map(|joins| {
                        let kind = joins.intern_get(id.1).unwrap_or("");
                        let key = joins.intern_get(id.2).unwrap_or("");
                        joins.row_props(kind, key)
                    })
                    .unwrap_or_default()
            };
            let previous = self
                .schema_history
                .get(&(namespace.to_string(), desc.kind.clone()))
                .and_then(|rows| {
                    rows.iter()
                        .rev()
                        .find(|row| row.effective_version() < desc.effective_version())
                });
            let mut extras = HashMap::new();
            if let Some(previous) = previous {
                let allowed: HashSet<&str> =
                    previous.properties.iter().map(String::as_str).collect();
                let mut keep = allowed.clone();
                for (from, _) in &desc.migration.rename {
                    keep.insert(from.as_str());
                }
                extras.extend(
                    props
                        .iter()
                        .filter(|(name, _)| !keep.contains(name.as_str()))
                        .map(|(name, value)| (name.clone(), value.clone())),
                );
                props.retain(|name, _| keep.contains(name.as_str()));
            }
            // Rename/cast update the live query projection. Dropped keys stay
            // stored when their names are free so named historical reads work.
            if desc.migration.apply(&mut props, false).is_ok() {
                for (name, value) in extras {
                    props.entry(name).or_insert(value);
                }
                self.install_live(id, meta, props)?;
                self.dirty.insert(id);
            }
        }
        Ok(())
    }

    fn apply_schema_view(
        &self,
        record: &mut ObjectRecord,
        as_of: Option<u32>,
    ) -> Result<(), String> {
        if record.kind == SCHEMA_KIND
            || record.kind == OVERLAY_KIND
            || record.kind == SOURCE_KIND
            || record.kind == CHANGE_SET_KIND
        {
            return Ok(());
        }
        if as_of.is_none() && self.live_schema_hidden(&record.namespace, &record.kind) {
            return unschematized_schema_view(record, as_of);
        }
        let Some(history) = self
            .schema_history
            .get(&(record.namespace.clone(), record.kind.clone()))
        else {
            return unschematized_schema_view(record, as_of);
        };
        if history.is_empty() {
            return unschematized_schema_view(record, as_of);
        }
        let current = history
            .last()
            .map(|row| row.effective_version())
            .unwrap_or(1);
        let target = as_of.unwrap_or(current);
        if target == 0 {
            return Err(format!(
                "unknown schema version {target} for {}/{}",
                record.namespace, record.kind
            ));
        }
        if target > current {
            return Err(format!(
                "schema version {target} is past current {current} for {}/{}",
                record.namespace, record.kind
            ));
        }
        let known = history.iter().any(|row| row.effective_version() == target);
        if !known {
            return Err(format!(
                "unknown schema version {target} for {}/{}",
                record.namespace, record.kind
            ));
        }
        if target < current {
            for row in history.iter().rev() {
                if row.effective_version() <= target {
                    break;
                }
                row.migration.reverse(&mut record.props);
            }
        }
        if let Some(desc) = history.iter().find(|row| row.effective_version() == target) {
            let allowed: HashSet<&str> = desc.properties.iter().map(String::as_str).collect();
            if target < current {
                record
                    .props
                    .retain(|name, _| allowed.contains(name.as_str()));
            } else {
                for row in history
                    .iter()
                    .filter(|row| row.effective_version() <= target)
                {
                    for name in &row.migration.drop {
                        if !allowed.contains(name.as_str()) {
                            record.props.remove(name);
                        }
                    }
                }
            }
        }
        record.schema_version = target;
        Ok(())
    }

    /// Current evaluate and ACL row filters omit declared drops; the join
    /// maps still store those keys so `load_at_schema` can name an older
    /// version (ADR 0035).
    pub(crate) fn omits_current_property(
        &self,
        namespace: &str,
        kind: &str,
        property: &str,
    ) -> bool {
        if self.live_schema_hidden(namespace, kind) {
            return false;
        }
        let Some(history) = self
            .schema_history
            .get(&(namespace.to_string(), kind.to_string()))
        else {
            return false;
        };
        let Some(current) = history.last() else {
            return false;
        };
        if current.properties.iter().any(|name| name == property) {
            return false;
        }
        history
            .iter()
            .any(|row| row.migration.drop.iter().any(|name| name == property))
    }

    fn live_schema_hidden(&self, namespace: &str, kind: &str) -> bool {
        self.find_live(namespace, SCHEMA_KIND, kind)
            .and_then(|id| self.live_meta(id))
            .is_some_and(|meta| meta.hidden)
    }

    fn current_kind_properties(&self, namespace: &str, kind: &str) -> Option<&[String]> {
        if self.live_schema_hidden(namespace, kind) {
            return None;
        }
        self.schema_history
            .get(&(namespace.to_string(), kind.to_string()))
            .and_then(|rows| rows.last())
            .map(|desc| desc.properties.as_slice())
    }

    fn hidden_props_omitting(
        &self,
        maps: &JoinMaps,
        kind: &str,
        id: &LiveId,
        denied: &HashSet<(u32, u32)>,
    ) -> HashMap<String, String> {
        let Some(owned) = self.hidden_owned(*id) else {
            return HashMap::new();
        };
        let mut props = HashMap::with_capacity(owned.len());
        let kind_id = maps.intern_existing(kind).unwrap_or(0);
        for (prop_id, value_id) in owned {
            if !denied.is_empty() && denied.contains(&(kind_id, prop_id)) {
                continue;
            }
            let Some(prop) = maps.intern_get(prop_id) else {
                continue;
            };
            let Some(value) = maps.intern_get(value_id) else {
                continue;
            };
            props.insert(prop.to_string(), value.to_string());
        }
        props
    }

    fn replay_from_log(&mut self) -> Result<(), String> {
        self.identity.clear();
        self.hidden_props.clear();
        self.joins.clear();
        self.ns.clear();
        self.ns_ix.clear();
        self.dirty.clear();
        self.action_commits.clear();
        self.schema_history.clear();
        self.schema_history_from_checkpoint = false;
        self.paged = None;
        self.replay_tail(0)?;
        self.schema_history_from_checkpoint = true;
        self.action_commits_from_log = true;
        Ok(())
    }

    fn replay_tail(&mut self, after_page: u32) -> Result<(), String> {
        for record in read_records_after_with_keys(&self.log, after_page, self.writer.keys())? {
            self.apply_logged(record)?;
        }
        self.dirty.clear();
        Ok(())
    }

    /// Apply one record that is already on the log to the live projection.
    /// Recovery replay and the split commit path share this step, so a
    /// commit applied after its fsync equals the same commit replayed after
    /// a crash (#261).
    fn apply_logged(&mut self, record: ObjectRecord) -> Result<(), String> {
        let schema_namespace = record.namespace.clone();
        let schema_kind = record.kind.clone();
        let schema_key = record.key.clone();
        self.apply_record(record)?;
        self.maybe_refresh_schema(&schema_namespace, &schema_kind, &schema_key)
    }

    fn maybe_refresh_schema(
        &mut self,
        namespace: &str,
        kind: &str,
        key: &str,
    ) -> Result<(), String> {
        if kind == SCHEMA_KIND {
            self.refresh_leaf_measures(namespace, key)?;
        }
        Ok(())
    }

    fn refresh_leaf_measures(&mut self, namespace: &str, kind: &str) -> Result<(), String> {
        let schema = self.schema(namespace, kind)?;
        match schema {
            Some(schema) => self
                .joins_for_write(namespace)?
                .set_declared_sums(&schema.kind, &schema.sums),
            None => {
                if let Some(joins) = self.joins.get_mut(namespace) {
                    joins.clear_declared_sums(kind);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn adopt_declared_measures(&mut self) -> Result<(), String> {
        let mut rows: Vec<(String, String)> = self
            .identity
            .iter()
            .filter(|(&id, meta)| self.live_kind(id) == Some(SCHEMA_KIND) && !meta.hidden)
            .filter_map(|(&id, _)| {
                let (namespace, _, key) = self.live_strings(id).ok()?;
                Some((namespace.to_string(), key.to_string()))
            })
            .collect();
        rows.sort();
        for (namespace, kind) in rows {
            let schema = self.schema(&namespace, &kind)?;
            match schema {
                Some(schema) => self
                    .joins_for_write(&namespace)?
                    .adopt_declared_sums(&schema.kind, &schema.sums),
                None => {
                    if let Some(joins) = self.joins.get_mut(&namespace) {
                        joins.clear_declared_sums(&kind);
                    }
                }
            }
        }
        for joins in self.joins.values_mut() {
            joins.recover_cached_sums();
        }
        Ok(())
    }

    /// Build last-hop rollups for an undeclared measure on first evaluate.
    pub(crate) fn ensure_lazy_measure(
        &self,
        namespace: &str,
        leaf_kind: &str,
        sum_property: &str,
    ) -> Result<(), String> {
        let Some(joins) = self.joins.get(namespace) else {
            return Ok(());
        };
        joins.ensure_cached_sum(leaf_kind, sum_property)?;
        Ok(())
    }

    /// Build one-property breakdown groups on first matching evaluate.
    pub(crate) fn ensure_lazy_groups(
        &self,
        namespace: &str,
        kind: &str,
        group_property: &str,
        sum_property: &str,
    ) -> Result<(), String> {
        let Some(joins) = self.joins.get(namespace) else {
            return Ok(());
        };
        joins.ensure_group_table(kind, group_property, sum_property)?;
        Ok(())
    }

    fn joins_for_write(&mut self, namespace: &str) -> Result<&mut JoinMaps, String> {
        self.ensure_writable()?;
        Ok(self.joins.entry(namespace.to_string()).or_default())
    }

    fn joins_in(&self, namespace: &str) -> Result<&JoinMaps, String> {
        self.joins
            .get(namespace)
            .ok_or_else(|| format!("unknown namespace {namespace}"))
    }

    fn live_ids(&self) -> Result<Vec<LiveId>, String> {
        if let Some(paged) = &self.paged {
            return Ok(paged
                .identities()?
                .into_iter()
                .map(|(id, _, _)| id)
                .collect());
        }
        Ok(self.identity.keys().copied().collect())
    }

    pub(crate) fn paged_fail_closed(&self) -> Result<(), String> {
        match &self.paged {
            Some(paged) => paged.fail_closed(),
            None => Ok(()),
        }
    }

    fn require_known_namespace<'a>(&self, namespace: &'a str) -> Result<&'a str, String> {
        let namespace = require_namespace(namespace)?;
        if let Ok(tenant) = tenant_of_namespace(namespace) {
            if !self.writer.tenant_key_usable(tenant) {
                return Err(format!("unknown namespace {namespace}"));
            }
        }
        if self.joins.contains_key(namespace) {
            Ok(namespace)
        } else {
            Err(format!("unknown namespace {namespace}"))
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if !std::thread::panicking() && !self.writer.is_failed() && self.ready {
            let _ = self.maybe_persist_projection(true);
        }
        self.release_paged();
    }
}

fn unschematized_schema_view(record: &mut ObjectRecord, as_of: Option<u32>) -> Result<(), String> {
    let target = as_of.unwrap_or(1);
    if target != 1 {
        return Err(format!(
            "unknown schema version {target} for {}/{}",
            record.namespace, record.kind
        ));
    }
    record.schema_version = 1;
    Ok(())
}

/// Store namespace for `namespace` owned by `tenant_id`, so storage keys
/// carry `(tenant_id, namespace)` (ADR 0030). The tenant may not contain
/// `/`, which keeps the composition one-to-one. Either part empty fails
/// closed. This is a key shape only: mikura does not bind or authorize
/// tenants.
pub fn tenant_namespace(tenant_id: &str, namespace: &str) -> Result<String, String> {
    if tenant_id.is_empty() || tenant_id.contains('/') {
        return Err("tenant_id must be non-empty and must not contain '/'".into());
    }
    require_namespace(namespace)?;
    Ok(format!("{tenant_id}/{namespace}"))
}

fn require_namespace(namespace: &str) -> Result<&str, String> {
    if namespace.is_empty() {
        Err("namespace required".into())
    } else {
        Ok(namespace)
    }
}

/// Change-set marker body: `{i}.op` (`put` or `hide`), `{i}.kind`, and
/// `{i}.key` for each edit in order (ADR 0033).
fn marker_props(edits: &[Edit]) -> HashMap<String, String> {
    edits
        .iter()
        .enumerate()
        .flat_map(|(i, edit)| {
            let op = if edit.is_hide() { "hide" } else { "put" };
            [
                (format!("{i}.op"), op.to_string()),
                (format!("{i}.kind"), edit.kind().to_string()),
                (format!("{i}.key"), edit.key().to_string()),
            ]
        })
        .collect()
}

fn marker_members(props: &HashMap<String, String>) -> Vec<SetMember> {
    (0..)
        .map_while(|i| {
            Some(SetMember {
                hide: props.get(&format!("{i}.op"))? == "hide",
                kind: props.get(&format!("{i}.kind"))?.clone(),
                key: props.get(&format!("{i}.key"))?.clone(),
                props: None,
            })
        })
        .collect()
}

fn require_source_cursor(record: &ObjectRecord) -> Result<u64, String> {
    if record.hidden {
        return Err("source cursor cannot be hidden".into());
    }
    if record.key.is_empty() || record.key.contains('/') {
        return Err("source cursor key must be non-empty and must not contain '/'".into());
    }
    if record.props.len() != 1 {
        return Err("source cursor must have only offset".into());
    }
    let raw = record
        .props
        .get(SOURCE_OFFSET)
        .ok_or_else(|| format!("source cursor missing {SOURCE_OFFSET}"))?;
    raw.parse::<u64>()
        .map_err(|_| format!("source cursor offset {raw} is invalid"))
}
