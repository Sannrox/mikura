use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::acl::PropertyAcl;
use crate::codec::{read_str, read_u32, read_u64, take, write_str};
use crate::joins::{crc32, read_checksummed, Checkpoint};
use crate::log::{read_records_after, LogWriter, SyncPolicy};

use super::{ActionCommit, SetMember, Store};

type ActionCatalog = HashMap<(String, String), ActionCommit>;
type LoadedLogCheckpoint = (Checkpoint, ActionCatalog);

pub(super) const CHECKPOINT_MAGIC: &[u8; 8] = b"MKCKPT01";

impl Store {
    /// Snapshot live identity, join maps, and the Action-id catalog at the
    /// current commit point (ADR 0034).
    pub fn checkpoint(&mut self) -> Result<(), String> {
        self.require_committed()?;
        self.ensure_action_commits()?;
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
        let mut ids: Vec<_> = self.identity.keys().cloned().collect();
        ids.sort();
        for id in ids {
            records.push(self.load(&id.0, &id.1, &id.2, &PropertyAcl::allow_all())?);
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
                Ok((loaded, actions)) if loaded.pages <= pages => {
                    self.action_commits = actions;
                    self.action_commits_from_log = true;
                    if loaded.pages < pages {
                        for record in read_records_after(&self.log, loaded.pages)? {
                            self.remember_action(&record);
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

    fn write_log_checkpoint_at(&self, log: &Path, pages: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(CHECKPOINT_MAGIC);
        body.extend_from_slice(&pages.to_le_bytes());
        self.encode_projection_after_magic(&mut body)?;
        self.encode_action_commits(&mut body)?;
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
                Ok((loaded, actions)) if loaded.pages <= pages => {
                    let from = loaded.pages;
                    self.adopt_checkpoint(loaded)?;
                    self.action_commits = actions;
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
    let (joins, identity, hidden_props) = Store::decode_projection_after_magic(&mut cur)?;
    let actions = decode_action_commits(&mut cur)?;
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
    ))
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
