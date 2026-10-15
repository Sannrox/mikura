use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::acl::PropertyAcl;
use crate::codec::{read_str, read_u32, read_u64, take, write_str};
use crate::joins::{check_join_crc, crc32, read_checksummed, Checkpoint, LiveMeta};
use crate::log::{read_records_after, LogWriter, SyncPolicy};

use crate::schema::{SchemaDescriptor, SCHEMA_KIND};

use super::{ActionCommit, LiveId, SetMember, Store};

type ActionCatalog = HashMap<(String, String), ActionCommit>;
type SchemaHistory = HashMap<(String, String), Vec<SchemaDescriptor>>;
type InstanceSchemaVersions = HashMap<LiveId, u32>;
type LoadedLogCheckpoint = (
    Checkpoint,
    ActionCatalog,
    Option<SchemaHistory>,
    Option<InstanceSchemaVersions>,
);

pub(super) const CHECKPOINT_MAGIC: &[u8; 8] = b"MKCKPT01";

/// The commit point a log checkpoint was taken at, from the file's bytes
/// (body then CRC32). Takes bytes so a caller copies exactly what it checked.
pub(crate) fn checkpoint_stamp(bytes: &[u8]) -> Result<u32, String> {
    if bytes.len() < 16 {
        return Err("log checkpoint: too short".into());
    }
    let (body, crc) = bytes.split_at(bytes.len() - 4);
    check_join_crc(body, u32::from_le_bytes(crc.try_into().unwrap()))
        .map_err(|err| format!("log checkpoint: {err}"))?;
    if &body[..8] != CHECKPOINT_MAGIC {
        return Err("bad checkpoint magic".into());
    }
    let mut cur = &body[8..];
    read_u32(&mut cur)
}

impl Store {
    /// Snapshot live identity, join maps, and the Action-id catalog at the
    /// current commit point (ADR 0034).
    pub fn checkpoint(&mut self) -> Result<(), String> {
        self.require_committed()?;
        self.ensure_action_commits()?;
        self.ensure_schema_history()?;
        if self.dirty.is_empty() && self.has_checkpoint {
            return self.persist_log_checkpoint();
        }
        self.persist_projection()
    }

    /// Write live generations (including hidden rows) to `dst` and a log
    /// checkpoint that keeps the Action-id catalog. The source log is not
    /// modified. Default open keeps full history; this is the opt-in compact
    /// (ADR 0034).
    pub fn compact_to(&mut self, dst: &Path) -> Result<usize, String> {
        self.require_committed()?;
        self.ensure_action_commits()?;
        self.ensure_schema_history()?;
        for path in [
            dst.to_path_buf(),
            Self::join_map_path(dst),
            Self::join_delta_path(dst),
            Self::checkpoint_path(dst),
            Self::checkpoint_prev_path(dst),
        ] {
            if path.exists() {
                return Err(format!("compact target {} already exists", path.display()));
            }
        }
        let mut records = Vec::with_capacity(self.identity.len());
        let mut schema_live = HashSet::new();
        let mut history_keys: Vec<_> = self.schema_history.keys().cloned().collect();
        history_keys.sort();
        for (namespace, kind) in history_keys {
            schema_live.insert((namespace.clone(), SCHEMA_KIND.to_string(), kind.clone()));
            let rows = self
                .schema_history
                .get(&(namespace.clone(), kind.clone()))
                .cloned()
                .unwrap_or_default();
            for (index, desc) in rows.iter().enumerate() {
                let mut record = desc.to_record(namespace.clone())?;
                if index + 1 == rows.len() {
                    if let Ok(live) =
                        self.load_stored(&namespace, SCHEMA_KIND, &kind, &PropertyAcl::allow_all())
                    {
                        if !live.hidden {
                            record.gen = live.gen;
                            record.action_id = live.action_id;
                        }
                    }
                }
                records.push(record);
            }
        }
        let mut ids: Vec<_> = self.identity.keys().cloned().collect();
        ids.sort();
        for id in ids {
            if schema_live.contains(&id) {
                let hidden = self.identity.get(&id).is_some_and(|meta| meta.hidden);
                if !hidden {
                    continue;
                }
            }
            records.push(self.load_stored(&id.0, &id.1, &id.2, &PropertyAcl::allow_all())?);
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let staging =
            Self::sidecar_path(dst, &format!(".compacting-{}-{nonce}", std::process::id()));
        let written = (|| {
            let mut writer = LogWriter::create(&staging, SyncPolicy::Group(32))?;
            for record in &records {
                writer.append_record(record).map_err(|error| {
                    format!(
                        "cannot compact {}/{}/{}: {error}",
                        record.namespace, record.kind, record.key
                    )
                })?;
            }
            writer.flush()?;
            let pages = writer.committed_pages();
            drop(writer);
            self.write_log_checkpoint_at(&staging, pages)?;
            let staged_ckpt = Self::checkpoint_path(&staging);
            let dst_ckpt = Self::checkpoint_path(dst);
            std::fs::hard_link(&staged_ckpt, &dst_ckpt)
                .map_err(|e| format!("publish {}: {e}", dst_ckpt.display()))?;
            if let Err(error) = std::fs::hard_link(&staging, dst) {
                let _ = std::fs::remove_file(&dst_ckpt);
                return Err(format!("publish {}: {error}", dst.display()));
            }
            let _ = std::fs::remove_file(&staged_ckpt);
            Ok(())
        })();
        let _ = std::fs::remove_file(&staging);
        let _ = std::fs::remove_file(Self::checkpoint_path(&staging));
        written.map(|()| records.len())
    }

    fn require_committed(&self) -> Result<(), String> {
        self.writer.ensure_usable()?;
        if self.writer.has_pending() {
            Err("uncommitted pages; commit first".into())
        } else {
            Ok(())
        }
    }

    /// Join-sidecar open clears the in-memory Action catalog. Reload it from
    /// `{log}.ckpt` so compact still refuses a superseded Action id (ADR 0034).
    pub(super) fn restore_action_catalog(&mut self, pages: u32) -> Result<(), String> {
        for path in [
            Self::checkpoint_path(&self.log),
            Self::checkpoint_prev_path(&self.log),
        ] {
            if !path.exists() {
                continue;
            }
            match load_log_checkpoint(&path) {
                Ok((loaded, actions, history, versions)) if loaded.pages <= pages => {
                    self.action_commits = actions;
                    if let Some(history) = history {
                        self.schema_history = history;
                        self.schema_history_from_checkpoint = true;
                    }
                    if let Some(versions) = versions {
                        raise_instance_schema_versions(&mut self.identity, versions);
                    }
                    self.action_commits_from_log = true;
                    if loaded.pages < pages {
                        for record in read_records_after(&self.log, loaded.pages)? {
                            self.remember_action(&record);
                            if record.kind == SCHEMA_KIND && !record.hidden {
                                if let Ok(desc) = SchemaDescriptor::from_record(&record) {
                                    self.remember_schema(&record.namespace, desc);
                                }
                            }
                        }
                    }
                    return Ok(());
                }
                Ok(_) | Err(_) => continue,
            }
        }
        Ok(())
    }

    pub(super) fn persist_log_checkpoint(&self) -> Result<(), String> {
        if !self.action_commits_from_log {
            return Ok(());
        }
        self.write_log_checkpoint_at(&self.log, self.writer.committed_pages())
    }

    /// Pre-0035 `MKCKPT01` body: projection and Action catalog, no schema
    /// history trailer. Crate tests use this to prove recovery rewrites a
    /// complete trailer before the next open trusts it.
    #[cfg(test)]
    pub(crate) fn persist_legacy_log_checkpoint_without_schema_history(
        &self,
    ) -> Result<(), String> {
        if !self.action_commits_from_log {
            return Err("action catalog is not from the log".into());
        }
        let pages = self.writer.committed_pages();
        let mut body = Vec::new();
        body.extend_from_slice(CHECKPOINT_MAGIC);
        body.extend_from_slice(&pages.to_le_bytes());
        self.encode_projection_after_magic(&mut body)?;
        self.encode_action_commits(&mut body)?;
        let path = Self::checkpoint_path(&self.log);
        let tmp = Self::checkpoint_tmp_path(&self.log);
        let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&body).map_err(|e| e.to_string())?;
        file.write_all(&crc32(&body).to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.sync_data().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    fn write_log_checkpoint_at(&self, log: &Path, pages: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(CHECKPOINT_MAGIC);
        body.extend_from_slice(&pages.to_le_bytes());
        self.encode_projection_after_magic(&mut body)?;
        self.encode_action_commits(&mut body)?;
        self.encode_schema_history(&mut body)?;
        self.encode_instance_schema_versions(&mut body)?;
        let path = Self::checkpoint_path(log);
        let tmp = Self::checkpoint_tmp_path(log);
        let prev = Self::checkpoint_prev_path(log);
        let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&body).map_err(|e| e.to_string())?;
        file.write_all(&crc32(&body).to_le_bytes())
            .map_err(|e| e.to_string())?;
        file.sync_data().map_err(|e| e.to_string())?;
        if path.exists() {
            let _ = std::fs::remove_file(&prev);
            std::fs::rename(&path, &prev).map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    pub(super) fn adopt_log_checkpoint(&mut self, pages: u32) -> Result<bool, String> {
        for path in [
            Self::checkpoint_path(&self.log),
            Self::checkpoint_prev_path(&self.log),
        ] {
            if !path.exists() {
                continue;
            }
            match load_log_checkpoint(&path) {
                Ok((loaded, actions, history, _)) if loaded.pages <= pages => {
                    let from = loaded.pages;
                    self.adopt_checkpoint(loaded)?;
                    self.action_commits = actions;
                    if let Some(history) = history {
                        self.schema_history = history;
                        self.schema_history_from_checkpoint = true;
                    }
                    self.action_commits_from_log = true;
                    // `replay_tail` checks the log still covers `from`
                    // (truncated files fail closed). It does not CRC pages
                    // `1..=from`: the checkpoint checksum binds that snapshot.
                    // Prefix CRC on every open would make restart proportional
                    // to history (ADR 0034). Delete the checkpoint to rebuild
                    // from the log. Then rewrite the join sidecar so a
                    // rejected or missing joins file cannot be adopted later.
                    self.replay_tail(from)?;
                    self.has_checkpoint = false;
                    self.persist_projection()?;
                    return Ok(true);
                }
                Ok(_) | Err(_) => continue,
            }
        }
        Ok(false)
    }

    fn encode_action_commits(&self, body: &mut Vec<u8>) -> Result<(), String> {
        let mut ids: Vec<_> = self.action_commits.keys().cloned().collect();
        ids.sort();
        let n = u32::try_from(ids.len()).map_err(|_| "too many action commits".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        for id in ids {
            let commit = self
                .action_commits
                .get(&id)
                .ok_or_else(|| "missing action commit".to_string())?;
            write_str(body, &id.0)?;
            write_str(body, &id.1)?;
            write_str(body, &commit.kind)?;
            write_str(body, &commit.key)?;
            body.extend_from_slice(&commit.gen.to_le_bytes());
            body.push(u8::from(commit.hidden));
            body.push(u8::from(commit.hide_applied));
            encode_props(body, &commit.props)?;
            let mn = u32::try_from(commit.members.len())
                .map_err(|_| "too many change-set members".to_string())?;
            body.extend_from_slice(&mn.to_le_bytes());
            for member in &commit.members {
                body.push(u8::from(member.hide));
                write_str(body, &member.kind)?;
                write_str(body, &member.key)?;
                match &member.props {
                    None => body.push(0),
                    Some(props) => {
                        body.push(1);
                        encode_props(body, props)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn encode_schema_history(&self, body: &mut Vec<u8>) -> Result<(), String> {
        let mut keys: Vec<_> = self.schema_history.keys().cloned().collect();
        keys.sort();
        let n = u32::try_from(keys.len()).map_err(|_| "too many schema kinds".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        for key in keys {
            let rows = self
                .schema_history
                .get(&key)
                .ok_or_else(|| "missing schema history".to_string())?;
            write_str(body, &key.0)?;
            write_str(body, &key.1)?;
            let rn =
                u32::try_from(rows.len()).map_err(|_| "too many schema versions".to_string())?;
            body.extend_from_slice(&rn.to_le_bytes());
            for desc in rows {
                encode_props(body, &desc.to_record(key.0.clone())?.props)?;
            }
        }
        Ok(())
    }

    pub(super) fn encode_instance_schema_versions(&self, body: &mut Vec<u8>) -> Result<(), String> {
        let mut rows: Vec<_> = self
            .identity
            .iter()
            .filter(|(_, meta)| meta.schema_version > 1)
            .map(|(id, meta)| (id.clone(), meta.schema_version))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let n = u32::try_from(rows.len()).map_err(|_| "too many identities".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        for ((namespace, kind, key), version) in rows {
            write_str(body, &namespace)?;
            write_str(body, &kind)?;
            write_str(body, &key)?;
            body.extend_from_slice(&version.to_le_bytes());
        }
        Ok(())
    }
}

fn encode_props(body: &mut Vec<u8>, props: &HashMap<String, String>) -> Result<(), String> {
    let cn = u32::try_from(props.len()).map_err(|_| "too many properties".to_string())?;
    body.extend_from_slice(&cn.to_le_bytes());
    let mut names: Vec<_> = props.keys().cloned().collect();
    names.sort();
    for name in names {
        write_str(body, &name)?;
        write_str(
            body,
            props
                .get(&name)
                .ok_or_else(|| "missing property".to_string())?,
        )?;
    }
    Ok(())
}

fn decode_props(cur: &mut &[u8]) -> Result<HashMap<String, String>, String> {
    let cn = read_u32(cur)? as usize;
    let mut props = HashMap::with_capacity(cn);
    for _ in 0..cn {
        let name = read_str(cur)?;
        let value = read_str(cur)?;
        props.insert(name, value);
    }
    Ok(props)
}

fn load_log_checkpoint(path: &Path) -> Result<LoadedLogCheckpoint, String> {
    let body = read_checksummed(path).map_err(|err| format!("log checkpoint: {err}"))?;
    if body.len() < 12 || &body[..8] != CHECKPOINT_MAGIC {
        return Err("bad checkpoint magic".into());
    }
    let mut cur = &body[8..];
    let pages = read_u32(&mut cur)?;
    let (joins, mut identity, hidden_props) = Store::decode_projection_after_magic(&mut cur)?;
    let actions = decode_action_commits(&mut cur)?;
    let history = if cur.is_empty() {
        None
    } else {
        Some(decode_schema_history(&mut cur)?)
    };
    let versions = if cur.is_empty() {
        None
    } else {
        Some(Store::decode_instance_schema_versions(&mut cur)?)
    };
    if let Some(versions) = versions.as_ref() {
        set_instance_schema_versions(&mut identity, versions.clone());
    }
    if !cur.is_empty() {
        return Err("trailing checkpoint bytes".into());
    }
    Ok((
        Checkpoint {
            pages,
            joins,
            identity,
            hidden_props,
        },
        actions,
        history,
        versions,
    ))
}

impl Store {
    pub(super) fn decode_instance_schema_versions(
        cur: &mut &[u8],
    ) -> Result<InstanceSchemaVersions, String> {
        read_instance_schema_versions(cur)
    }

    pub(super) fn apply_instance_schema_versions(
        identity: &mut HashMap<LiveId, LiveMeta>,
        versions: InstanceSchemaVersions,
    ) {
        set_instance_schema_versions(identity, versions);
    }
}

fn read_instance_schema_versions(cur: &mut &[u8]) -> Result<InstanceSchemaVersions, String> {
    let n = read_u32(cur)? as usize;
    let mut out = HashMap::with_capacity(n);
    for _ in 0..n {
        let namespace = read_str(cur)?;
        let kind = read_str(cur)?;
        let key = read_str(cur)?;
        let version = read_u32(cur)?;
        out.insert(
            (namespace, kind, key),
            if version == 0 { 1 } else { version },
        );
    }
    Ok(out)
}

fn set_instance_schema_versions(
    identity: &mut HashMap<LiveId, LiveMeta>,
    versions: InstanceSchemaVersions,
) {
    for (id, version) in versions {
        if let Some(meta) = identity.get_mut(&id) {
            meta.schema_version = version;
        }
    }
}

/// Applies an older checkpoint's versions without lowering newer ones. The
/// projection is already loaded and may hold later generations; an
/// instance's schema version only grows, so the higher value is current.
fn raise_instance_schema_versions(
    identity: &mut HashMap<LiveId, LiveMeta>,
    versions: InstanceSchemaVersions,
) {
    for (id, version) in versions {
        if let Some(meta) = identity.get_mut(&id) {
            meta.schema_version = meta.schema_version.max(version);
        }
    }
}

fn decode_schema_history(cur: &mut &[u8]) -> Result<SchemaHistory, String> {
    let n = read_u32(cur)? as usize;
    let mut out = HashMap::with_capacity(n);
    for _ in 0..n {
        let namespace = read_str(cur)?;
        let kind = read_str(cur)?;
        let rn = read_u32(cur)? as usize;
        let mut rows = Vec::with_capacity(rn);
        for _ in 0..rn {
            let props = decode_props(cur)?;
            let desc = SchemaDescriptor::from_record(&super::ObjectRecord {
                gen: 0,
                namespace: namespace.clone(),
                kind: SCHEMA_KIND.to_string(),
                key: kind.clone(),
                hidden: false,
                action_id: None,
                props,
                schema_version: 1,
            })?;
            rows.push(desc);
        }
        out.insert((namespace, kind), rows);
    }
    Ok(out)
}

fn decode_action_commits(cur: &mut &[u8]) -> Result<ActionCatalog, String> {
    let n = read_u32(cur)? as usize;
    let mut out = HashMap::with_capacity(n);
    for _ in 0..n {
        let namespace = read_str(cur)?;
        let id = read_str(cur)?;
        let kind = read_str(cur)?;
        let key = read_str(cur)?;
        let gen = read_u64(cur)?;
        let hidden = take::<1>(cur)?[0] != 0;
        let hide_applied = take::<1>(cur)?[0] != 0;
        let props = decode_props(cur)?;
        let mn = read_u32(cur)? as usize;
        let mut members = Vec::with_capacity(mn);
        for _ in 0..mn {
            let hide = take::<1>(cur)?[0] != 0;
            let member_kind = read_str(cur)?;
            let member_key = read_str(cur)?;
            let has_props = take::<1>(cur)?[0] != 0;
            let member_props = if has_props {
                Some(decode_props(cur)?)
            } else {
                None
            };
            members.push(SetMember {
                hide,
                kind: member_kind,
                key: member_key,
                props: member_props,
            });
        }
        let key_id = (namespace.clone(), id);
        if out.contains_key(&key_id) {
            return Err("duplicate action commit".into());
        }
        out.insert(
            key_id,
            ActionCommit {
                namespace,
                kind,
                key,
                gen,
                hidden,
                hide_applied,
                props,
                members,
            },
        );
    }
    Ok(out)
}
