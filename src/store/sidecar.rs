use std::collections::HashMap;
use std::path::Path;

use crate::codec::{read_str, read_u32, read_u64, take, write_str};
use crate::joins::{
    append_checksummed, check_join_crc, read_checksummed, write_checksummed, Checkpoint, JoinMaps,
    LiveMeta, ACTION_NONE, JOIN_DELTA_MAGIC, JOIN_DELTA_MAGIC_V5, JOIN_MAGIC,
};
use crate::log::read_records_after;

use super::{LiveId, Store};

pub(super) type ProjectionBody = (
    HashMap<String, JoinMaps>,
    HashMap<LiveId, LiveMeta>,
    HashMap<LiveId, HashMap<String, String>>,
);

impl Store {
    /// Last join-sidecar page stamp written or loaded by this process.
    /// Missing sidecar files count as stamp 0 without reading the projection.
    pub fn sidecar_stamp_pages(&self) -> u32 {
        let checkpoint = Self::join_map_path(&self.log).exists();
        let delta = Self::join_delta_path(&self.log).exists();
        match (checkpoint, delta) {
            (false, _) => 0,
            (true, false) => self.sidecar_checkpoint_stamp,
            (true, true) => self
                .sidecar_delta_stamp
                .unwrap_or(self.sidecar_checkpoint_stamp),
        }
    }

    pub(super) fn persist_projection(&mut self) -> Result<(), String> {
        let pages = self.writer.committed_pages();
        let compact =
            !self.has_checkpoint || self.dirty.len().saturating_mul(4) > self.identity.len().max(1);
        if compact {
            return self.rewrite_checkpoint(pages);
        }
        self.persist_delta(pages)?;
        self.dirty.clear();
        if self.delta_bytes > self.delta_compact_bytes {
            return self.rewrite_checkpoint(pages);
        }
        Ok(())
    }

    fn rewrite_checkpoint(&mut self, pages: u32) -> Result<(), String> {
        self.persist_checkpoint(pages)?;
        self.sidecar_checkpoint_stamp = pages;
        self.sidecar_delta_stamp = None;
        let _ = std::fs::remove_file(Self::join_delta_path(&self.log));
        self.dirty.clear();
        self.has_checkpoint = true;
        self.delta_bytes = 0;
        self.ensure_schema_history()?;
        self.persist_log_checkpoint()
    }

    pub(super) fn encode_projection_after_magic(&self, body: &mut Vec<u8>) -> Result<(), String> {
        let mut namespaces: Vec<_> = self.joins.keys().cloned().collect();
        namespaces.sort();
        let ns_n =
            u32::try_from(namespaces.len()).map_err(|_| "too many namespaces".to_string())?;
        body.extend_from_slice(&ns_n.to_le_bytes());
        for namespace in namespaces {
            write_str(body, &namespace)?;
            let joins = self
                .joins
                .get(&namespace)
                .ok_or_else(|| "missing namespace joins".to_string())?;
            let n = u32::try_from(joins.intern.len())
                .map_err(|_| "too many interned strings".to_string())?;
            body.extend_from_slice(&n.to_le_bytes());
            for value in &joins.intern {
                write_str(body, value)?;
            }
            let mut ids: Vec<_> = self
                .identity
                .keys()
                .filter(|(ns, _, _)| ns == &namespace)
                .cloned()
                .collect();
            ids.sort();
            let kn = u32::try_from(ids.len()).map_err(|_| "too many identities".to_string())?;
            body.extend_from_slice(&kn.to_le_bytes());
            for id in ids {
                let meta = self
                    .identity
                    .get(&id)
                    .ok_or_else(|| "missing identity".to_string())?;
                let kind_id = joins
                    .intern_existing(&id.1)
                    .ok_or_else(|| "missing intern kind".to_string())?;
                let key_id = joins
                    .intern_existing(&id.2)
                    .ok_or_else(|| "missing intern key".to_string())?;
                body.extend_from_slice(&kind_id.to_le_bytes());
                body.extend_from_slice(&key_id.to_le_bytes());
                body.extend_from_slice(&meta.gen.to_le_bytes());
                body.push(u8::from(meta.hidden));
                let owned = if meta.hidden {
                    interned_prop_ids(joins, self.hidden_props.get(&id))?
                } else {
                    joins
                        .owned
                        .get(&(kind_id, key_id))
                        .cloned()
                        .unwrap_or_default()
                };
                let cn =
                    u32::try_from(owned.len()).map_err(|_| "too many properties".to_string())?;
                body.extend_from_slice(&cn.to_le_bytes());
                for (prop_id, value_id) in owned {
                    body.extend_from_slice(&prop_id.to_le_bytes());
                    body.extend_from_slice(&value_id.to_le_bytes());
                }
                let action_intern = meta.action_id.unwrap_or(ACTION_NONE);
                body.extend_from_slice(&action_intern.to_le_bytes());
            }
            joins.write_measures(body)?;
        }
        Ok(())
    }

    fn persist_checkpoint(&self, pages: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(JOIN_MAGIC);
        body.extend_from_slice(&pages.to_le_bytes());
        self.encode_projection_after_magic(&mut body)?;
        self.encode_instance_schema_versions(&mut body)?;
        write_checksummed(&Self::join_map_path(&self.log), &body)
    }

    fn persist_delta(&mut self, pages: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(JOIN_DELTA_MAGIC);
        body.extend_from_slice(&pages.to_le_bytes());
        let mut ids: Vec<_> = self.dirty.iter().cloned().collect();
        ids.sort();
        let n = u32::try_from(ids.len()).map_err(|_| "too many dirty rows".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        let mut versions = Vec::with_capacity(ids.len());
        for id in &ids {
            let meta = self
                .identity
                .get(id)
                .ok_or_else(|| "missing dirty identity".to_string())?;
            write_str(&mut body, &id.0)?;
            write_str(&mut body, &id.1)?;
            write_str(&mut body, &id.2)?;
            body.extend_from_slice(&meta.gen.to_le_bytes());
            body.push(u8::from(meta.hidden));
            versions.push(meta.schema_version.max(1));
            let joins = self
                .joins
                .get(&id.0)
                .ok_or_else(|| "missing namespace joins".to_string())?;
            let props = if meta.hidden {
                self.hidden_props.get(id).cloned().unwrap_or_default()
            } else {
                joins.row_props(&id.1, &id.2)
            };
            let cn = u32::try_from(props.len()).map_err(|_| "too many properties".to_string())?;
            body.extend_from_slice(&cn.to_le_bytes());
            let mut names: Vec<_> = props.keys().cloned().collect();
            names.sort();
            for name in names {
                write_str(&mut body, &name)?;
                write_str(
                    &mut body,
                    props
                        .get(&name)
                        .ok_or_else(|| "missing dirty prop".to_string())?,
                )?;
            }
            write_str(
                &mut body,
                meta.action_id
                    .map(|action| {
                        joins
                            .intern_get(action)
                            .ok_or_else(|| "missing intern action".to_string())
                    })
                    .transpose()?
                    .unwrap_or(""),
            )?;
        }
        for version in versions {
            body.extend_from_slice(&version.to_le_bytes());
        }
        let path = Self::join_delta_path(&self.log);
        if path.exists() {
            let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            if len > self.delta_bytes {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(|e| e.to_string())?;
                file.set_len(self.delta_bytes).map_err(|e| e.to_string())?;
                file.sync_data().map_err(|e| e.to_string())?;
            }
        } else {
            self.delta_bytes = 0;
        }
        append_checksummed(&path, &body)?;
        self.delta_bytes = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        self.sidecar_delta_stamp = Some(pages);
        Ok(())
    }

    pub(super) fn decode_projection_after_magic(cur: &mut &[u8]) -> Result<ProjectionBody, String> {
        let ns_n = read_u32(cur)? as usize;
        let mut all_joins = HashMap::with_capacity(ns_n);
        let mut identity = HashMap::new();
        let mut hidden_props = HashMap::new();
        for _ in 0..ns_n {
            let namespace = read_str(cur)?;
            if namespace.is_empty() {
                return Err("namespace required".into());
            }
            if all_joins.contains_key(&namespace) {
                return Err(format!("duplicate namespace {namespace}"));
            }
            let intern_n = read_u32(cur)? as usize;
            let mut joins = JoinMaps::default();
            joins.intern.reserve(intern_n);
            for i in 0..intern_n {
                let value = read_str(cur)?;
                joins.intern_ix.insert(value.clone(), i as u32);
                joins.intern.push(value);
            }
            let id_n = read_u32(cur)? as usize;
            for _ in 0..id_n {
                let kind_id = read_u32(cur)?;
                let key_id = read_u32(cur)?;
                let gen = read_u64(cur)?;
                let hidden = take::<1>(cur)?[0] != 0;
                let cn = read_u32(cur)? as usize;
                let mut owned = Vec::with_capacity(cn);
                let kind = joins
                    .intern
                    .get(kind_id as usize)
                    .ok_or_else(|| "bad intern kind".to_string())?
                    .clone();
                let key = joins
                    .intern
                    .get(key_id as usize)
                    .ok_or_else(|| "bad intern key".to_string())?
                    .clone();
                for _ in 0..cn {
                    let prop_id = read_u32(cur)?;
                    let value_id = read_u32(cur)?;
                    owned.push((prop_id, value_id));
                }
                let action_intern = read_u32(cur)?;
                let action_id = if action_intern == ACTION_NONE {
                    None
                } else {
                    if joins.intern_get(action_intern).is_none() {
                        return Err("bad intern action".into());
                    }
                    Some(action_intern)
                };
                identity.insert(
                    (namespace.clone(), kind.clone(), key.clone()),
                    LiveMeta {
                        gen,
                        hidden,
                        action_id,
                        schema_version: 1,
                    },
                );
                if hidden {
                    hidden_props.insert(
                        (namespace.clone(), kind, key),
                        props_from_owned(&joins, &owned)?,
                    );
                } else {
                    joins.insert_visible_ids(kind_id, key_id, owned)?;
                }
            }
            joins.read_measures(cur)?;
            all_joins.insert(namespace, joins);
        }
        Ok((all_joins, identity, hidden_props))
    }

    fn load_checkpoint(path: &Path) -> Result<Checkpoint, String> {
        let body = read_checksummed(path)?;
        if body.len() < 12 || &body[..8] != JOIN_MAGIC {
            return Err("bad join magic".into());
        }
        let mut cur = &body[8..];
        let pages = read_u32(&mut cur)?;
        let (joins, mut identity, hidden_props) = Self::decode_projection_after_magic(&mut cur)?;
        if !cur.is_empty() {
            Store::apply_instance_schema_versions(
                &mut identity,
                Store::decode_instance_schema_versions(&mut cur)?,
            );
        }
        if !cur.is_empty() {
            return Err("trailing join bytes".into());
        }
        Ok(Checkpoint {
            pages,
            joins,
            identity,
            hidden_props,
        })
    }

    fn apply_delta(&mut self, path: &Path) -> Result<u32, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if bytes.len() < 12 {
            return Err("join sidecar too short".into());
        }
        let mut cur = bytes.as_slice();
        let mut pages = None;
        let mut good = 0usize;
        while !cur.is_empty() {
            let before = cur.len();
            match take_delta_frame(&mut cur) {
                Ok(body) => {
                    pages = Some(self.apply_delta_body(body)?);
                    good += before - cur.len();
                }
                Err(err) if pages.is_some() && is_torn_tail(&err) => break,
                Err(err) => return Err(err),
            }
        }
        self.delta_bytes = good as u64;
        let pages = pages.ok_or_else(|| "join sidecar too short".to_string())?;
        self.sidecar_delta_stamp = Some(pages);
        Ok(pages)
    }

    fn apply_delta_body(&mut self, body: &[u8]) -> Result<u32, String> {
        let versioned = delta_frame_versioned(body)?;
        let mut cur = &body[8..];
        let (pages, rows) = read_delta_rows(&mut cur, versioned)?;
        if !cur.is_empty() {
            return Err("trailing join bytes".into());
        }
        for row in rows {
            let action_id = if row.action.is_empty() {
                None
            } else {
                Some(self.joins_for_write(&row.namespace).intern(&row.action))
            };
            let schema_namespace = row.namespace.clone();
            let schema_kind = row.kind.clone();
            let schema_key = row.key.clone();
            self.install_live(
                super::live_id(&row.namespace, &row.kind, &row.key),
                LiveMeta {
                    gen: row.gen,
                    hidden: row.hidden,
                    action_id,
                    schema_version: row.schema_version,
                },
                row.props,
            );
            self.maybe_refresh_schema(&schema_namespace, &schema_kind, &schema_key)?;
        }
        Ok(pages)
    }

    pub(super) fn install_projection(&mut self) -> Result<(), String> {
        let sidecar = Self::join_map_path(&self.log);
        let delta = Self::join_delta_path(&self.log);
        let pages = self.writer.committed_pages();
        // Superblock `committed_pages` is not proof the file still holds those
        // pages. A matching sidecar or checkpoint must not skip this check.
        let _ = read_records_after(&self.log, pages)?;
        if sidecar.exists() {
            let loaded = Self::load_checkpoint(&sidecar)?;
            if loaded.pages == pages && !delta.exists() {
                self.adopt_checkpoint(loaded)?;
                self.restore_action_catalog(pages)?;
                return Ok(());
            }
            if loaded.pages <= pages {
                let loaded_pages = loaded.pages;
                self.adopt_checkpoint(loaded)?;
                let mut from = loaded_pages;
                if delta.exists() {
                    let delta_pages = self.apply_delta(&delta)?;
                    if delta_pages == pages {
                        self.restore_action_catalog(pages)?;
                        return Ok(());
                    }
                    if delta_pages > loaded_pages && delta_pages < pages {
                        from = delta_pages;
                    } else {
                        self.replay_from_log()?;
                        self.restore_action_catalog(pages)?;
                        self.has_checkpoint = false;
                        return self.persist_projection();
                    }
                } else if loaded_pages == pages {
                    self.restore_action_catalog(pages)?;
                    return Ok(());
                }
                self.restore_action_catalog(pages)?;
                self.replay_tail(from)?;
                self.has_checkpoint = false;
                return self.persist_projection();
            }
        } else if delta.exists() {
            let _ = std::fs::remove_file(&delta);
        }
        if self.adopt_log_checkpoint(pages)? {
            return Ok(());
        }
        self.replay_from_log()?;
        self.restore_action_catalog(pages)?;
        self.has_checkpoint = false;
        self.persist_projection()
    }

    pub(super) fn adopt_checkpoint(&mut self, loaded: Checkpoint) -> Result<(), String> {
        self.joins = loaded.joins;
        self.identity = loaded.identity;
        self.hidden_props = loaded.hidden_props;
        self.has_checkpoint = true;
        self.sidecar_checkpoint_stamp = loaded.pages;
        self.sidecar_delta_stamp = None;
        self.action_commits.clear();
        self.action_commits_from_log = false;
        self.adopt_declared_measures()
    }
}

fn is_torn_tail(err: &str) -> bool {
    err.contains("too short") || err.contains("bad join magic")
}

fn take_delta_frame<'a>(cur: &mut &'a [u8]) -> Result<&'a [u8], String> {
    if cur.len() < 12 {
        return Err("join sidecar too short".into());
    }
    let body_len = delta_frame_body_len(cur)?;
    let total = body_len
        .checked_add(4)
        .ok_or_else(|| "join sidecar too short".to_string())?;
    if cur.len() < total {
        return Err("join sidecar too short".into());
    }
    let (frame, rest) = cur.split_at(total);
    let (body, crc_bytes) = frame.split_at(body_len);
    let expected = u32::from_le_bytes(crc_bytes.try_into().unwrap());
    check_join_crc(body, expected)?;
    *cur = rest;
    Ok(body)
}

/// Whether a delta frame carries the per-row schema version trailer.
fn delta_frame_versioned(bytes: &[u8]) -> Result<bool, String> {
    match bytes.get(..8) {
        Some(magic) if magic == JOIN_DELTA_MAGIC => Ok(true),
        Some(magic) if magic == JOIN_DELTA_MAGIC_V5 => Ok(false),
        _ => Err("bad join magic".into()),
    }
}

fn delta_frame_body_len(bytes: &[u8]) -> Result<usize, String> {
    if bytes.len() < 16 {
        return Err("bad join magic".into());
    }
    let versioned = delta_frame_versioned(bytes)?;
    let mut cur = &bytes[8..];
    let (_, n) = skip_delta_rows(&mut cur)?;
    if versioned {
        let trailer = n
            .checked_mul(4)
            .ok_or_else(|| "join sidecar too short".to_string())?;
        if cur.len() < trailer {
            return Err("join sidecar too short".into());
        }
        cur = &cur[trailer..];
    }
    Ok(bytes.len() - cur.len())
}

fn skip_delta_rows(cur: &mut &[u8]) -> Result<(u32, usize), String> {
    let pages = read_u32(cur)?;
    let n = read_u32(cur)? as usize;
    for _ in 0..n {
        let _namespace = read_str(cur)?;
        let _kind = read_str(cur)?;
        let _key = read_str(cur)?;
        let _gen = read_u64(cur)?;
        let _hidden = take::<1>(cur)?;
        let cn = read_u32(cur)? as usize;
        for _ in 0..cn {
            let _name = read_str(cur)?;
            let _value = read_str(cur)?;
        }
        let _action = read_str(cur)?;
    }
    Ok((pages, n))
}

struct DeltaRow {
    namespace: String,
    kind: String,
    key: String,
    gen: u64,
    hidden: bool,
    props: HashMap<String, String>,
    action: String,
    schema_version: u32,
}

fn read_delta_rows(cur: &mut &[u8], versioned: bool) -> Result<(u32, Vec<DeltaRow>), String> {
    let pages = read_u32(cur)?;
    let n = read_u32(cur)? as usize;
    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        let namespace = read_str(cur)?;
        let kind = read_str(cur)?;
        let key = read_str(cur)?;
        let gen = read_u64(cur)?;
        let hidden = take::<1>(cur)?[0] != 0;
        let cn = read_u32(cur)? as usize;
        let mut props = HashMap::with_capacity(cn);
        for _ in 0..cn {
            let name = read_str(cur)?;
            let value = read_str(cur)?;
            props.insert(name, value);
        }
        let action = read_str(cur)?;
        rows.push(DeltaRow {
            namespace,
            kind,
            key,
            gen,
            hidden,
            props,
            action,
            schema_version: 1,
        });
    }
    if versioned {
        for row in &mut rows {
            row.schema_version = read_u32(cur)?;
            if row.schema_version == 0 {
                row.schema_version = 1;
            }
        }
    }
    Ok((pages, rows))
}

fn interned_prop_ids(
    joins: &JoinMaps,
    props: Option<&HashMap<String, String>>,
) -> Result<Vec<(u32, u32)>, String> {
    let Some(props) = props else {
        return Ok(Vec::new());
    };
    let mut owned = Vec::with_capacity(props.len());
    let mut names: Vec<_> = props.keys().cloned().collect();
    names.sort();
    for name in names {
        let prop_id = joins
            .intern_existing(&name)
            .ok_or_else(|| "missing intern prop".to_string())?;
        let value = props
            .get(&name)
            .ok_or_else(|| "missing intern value".to_string())?;
        let value_id = joins
            .intern_existing(value)
            .ok_or_else(|| "missing intern value".to_string())?;
        owned.push((prop_id, value_id));
    }
    Ok(owned)
}

fn props_from_owned(
    joins: &JoinMaps,
    owned: &[(u32, u32)],
) -> Result<HashMap<String, String>, String> {
    let mut props = HashMap::with_capacity(owned.len());
    for &(prop_id, value_id) in owned {
        let prop = joins
            .intern
            .get(prop_id as usize)
            .ok_or_else(|| "bad intern prop".to_string())?
            .clone();
        let value = joins
            .intern
            .get(value_id as usize)
            .ok_or_else(|| "bad intern value".to_string())?
            .clone();
        props.insert(prop, value);
    }
    Ok(props)
}
