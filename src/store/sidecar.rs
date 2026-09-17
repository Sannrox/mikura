use std::collections::HashMap;
use std::path::Path;

use crate::codec::{read_str, read_u32, read_u64, take, write_str};
use crate::joins::{
    read_checksummed, write_checksummed, Checkpoint, JoinMaps, LiveMeta, ACTION_NONE,
    JOIN_DELTA_MAGIC, JOIN_MAGIC,
};

use super::Store;

impl Store {
    pub(super) fn persist_projection(&mut self) -> Result<(), String> {
        let pages = self.writer.committed_pages();
        let compact =
            !self.has_checkpoint || self.dirty.len().saturating_mul(4) > self.identity.len().max(1);
        if compact {
            self.persist_checkpoint(pages)?;
            let _ = std::fs::remove_file(Self::join_delta_path(&self.log));
            self.dirty.clear();
            self.has_checkpoint = true;
            return Ok(());
        }
        self.persist_delta(pages)?;
        Ok(())
    }

    fn persist_checkpoint(&self, pages: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(JOIN_MAGIC);
        body.extend_from_slice(&pages.to_le_bytes());
        let n = u32::try_from(self.joins.intern.len())
            .map_err(|_| "too many interned strings".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        for value in &self.joins.intern {
            write_str(&mut body, value)?;
        }
        let mut ids: Vec<_> = self.identity.keys().cloned().collect();
        ids.sort();
        let kn = u32::try_from(ids.len()).map_err(|_| "too many identities".to_string())?;
        body.extend_from_slice(&kn.to_le_bytes());
        for id in ids {
            let meta = self
                .identity
                .get(&id)
                .ok_or_else(|| "missing identity".to_string())?;
            let kind_id = self
                .joins
                .intern_existing(&id.0)
                .ok_or_else(|| "missing intern kind".to_string())?;
            let key_id = self
                .joins
                .intern_existing(&id.1)
                .ok_or_else(|| "missing intern key".to_string())?;
            body.extend_from_slice(&kind_id.to_le_bytes());
            body.extend_from_slice(&key_id.to_le_bytes());
            body.extend_from_slice(&meta.gen.to_le_bytes());
            body.push(u8::from(meta.hidden));
            let owned = if meta.hidden {
                interned_prop_ids(&self.joins, self.hidden_props.get(&id))?
            } else {
                self.joins
                    .owned
                    .get(&(kind_id, key_id))
                    .cloned()
                    .unwrap_or_default()
            };
            let cn = u32::try_from(owned.len()).map_err(|_| "too many properties".to_string())?;
            body.extend_from_slice(&cn.to_le_bytes());
            for (prop_id, value_id) in owned {
                body.extend_from_slice(&prop_id.to_le_bytes());
                body.extend_from_slice(&value_id.to_le_bytes());
            }
            let action_intern = meta.action_id.unwrap_or(ACTION_NONE);
            body.extend_from_slice(&action_intern.to_le_bytes());
        }
        write_checksummed(&Self::join_map_path(&self.log), &body)
    }

    fn persist_delta(&self, pages: u32) -> Result<(), String> {
        let mut body = Vec::new();
        body.extend_from_slice(JOIN_DELTA_MAGIC);
        body.extend_from_slice(&pages.to_le_bytes());
        let mut ids: Vec<_> = self.dirty.iter().cloned().collect();
        ids.sort();
        let n = u32::try_from(ids.len()).map_err(|_| "too many dirty rows".to_string())?;
        body.extend_from_slice(&n.to_le_bytes());
        for id in ids {
            let meta = self
                .identity
                .get(&id)
                .ok_or_else(|| "missing dirty identity".to_string())?;
            write_str(&mut body, &id.0)?;
            write_str(&mut body, &id.1)?;
            body.extend_from_slice(&meta.gen.to_le_bytes());
            body.push(u8::from(meta.hidden));
            let props = if meta.hidden {
                self.hidden_props.get(&id).cloned().unwrap_or_default()
            } else {
                self.joins.row_props(&id.0, &id.1)
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
                    .map(|id| {
                        self.joins
                            .intern_get(id)
                            .ok_or_else(|| "missing intern action".to_string())
                    })
                    .transpose()?
                    .unwrap_or(""),
            )?;
        }
        write_checksummed(&Self::join_delta_path(&self.log), &body)
    }

    fn load_checkpoint(path: &Path) -> Result<Checkpoint, String> {
        let body = read_checksummed(path)?;
        if body.len() < 12 || &body[..8] != JOIN_MAGIC {
            return Err("bad join magic".into());
        }
        let mut cur = &body[8..];
        let pages = read_u32(&mut cur)?;
        let intern_n = read_u32(&mut cur)? as usize;
        let mut joins = JoinMaps::default();
        joins.intern.reserve(intern_n);
        for i in 0..intern_n {
            let value = read_str(&mut cur)?;
            joins.intern_ix.insert(value.clone(), i as u32);
            joins.intern.push(value);
        }
        let id_n = read_u32(&mut cur)? as usize;
        let mut identity = HashMap::with_capacity(id_n);
        let mut hidden_props = HashMap::new();
        for _ in 0..id_n {
            let kind_id = read_u32(&mut cur)?;
            let key_id = read_u32(&mut cur)?;
            let gen = read_u64(&mut cur)?;
            let hidden = take::<1>(&mut cur)?[0] != 0;
            let cn = read_u32(&mut cur)? as usize;
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
                let prop_id = read_u32(&mut cur)?;
                let value_id = read_u32(&mut cur)?;
                owned.push((prop_id, value_id));
            }
            let action_intern = read_u32(&mut cur)?;
            let action_id = if action_intern == ACTION_NONE {
                None
            } else {
                if joins.intern_get(action_intern).is_none() {
                    return Err("bad intern action".into());
                }
                Some(action_intern)
            };
            identity.insert(
                (kind.clone(), key.clone()),
                LiveMeta {
                    gen,
                    hidden,
                    action_id,
                },
            );
            if hidden {
                hidden_props.insert((kind, key), props_from_owned(&joins, &owned)?);
            } else {
                joins.insert_visible_ids(kind_id, key_id, owned)?;
            }
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
        let body = read_checksummed(path)?;
        if body.len() < 12 || &body[..8] != JOIN_DELTA_MAGIC {
            return Err("bad join magic".into());
        }
        let mut cur = &body[8..];
        let pages = read_u32(&mut cur)?;
        let n = read_u32(&mut cur)? as usize;
        for _ in 0..n {
            let kind = read_str(&mut cur)?;
            let key = read_str(&mut cur)?;
            let gen = read_u64(&mut cur)?;
            let hidden = take::<1>(&mut cur)?[0] != 0;
            let cn = read_u32(&mut cur)? as usize;
            let mut props = HashMap::with_capacity(cn);
            for _ in 0..cn {
                let name = read_str(&mut cur)?;
                let value = read_str(&mut cur)?;
                props.insert(name, value);
            }
            let action_id = read_str(&mut cur)?;
            let action_id = if action_id.is_empty() {
                None
            } else {
                Some(self.joins.intern(&action_id))
            };
            self.joins.remove(&kind, &key);
            self.identity.insert(
                (kind.clone(), key.clone()),
                LiveMeta {
                    gen,
                    hidden,
                    action_id,
                },
            );
            if hidden {
                for (name, value) in &props {
                    self.joins.intern(name);
                    self.joins.intern(value);
                }
                self.joins.intern(&kind);
                self.joins.intern(&key);
                self.hidden_props.insert((kind, key), props);
            } else {
                self.hidden_props.remove(&(kind.clone(), key.clone()));
                self.joins.insert_visible(&kind, &key, props);
            }
        }
        if !cur.is_empty() {
            return Err("trailing join bytes".into());
        }
        Ok(pages)
    }

    pub(super) fn install_projection(&mut self) -> Result<(), String> {
        let sidecar = Self::join_map_path(&self.log);
        let delta = Self::join_delta_path(&self.log);
        let pages = self.writer.committed_pages();
        if sidecar.exists() {
            let loaded = Self::load_checkpoint(&sidecar)?;
            if loaded.pages == pages && !delta.exists() {
                self.joins = loaded.joins;
                self.identity = loaded.identity;
                self.hidden_props = loaded.hidden_props;
                self.has_checkpoint = true;
                return Ok(());
            }
            if loaded.pages <= pages {
                self.joins = loaded.joins;
                self.identity = loaded.identity;
                self.hidden_props = loaded.hidden_props;
                self.has_checkpoint = true;
                if delta.exists() {
                    let delta_pages = self.apply_delta(&delta)?;
                    if delta_pages == pages {
                        return Ok(());
                    }
                } else if loaded.pages == pages {
                    return Ok(());
                }
            }
        } else if delta.exists() {
            let _ = std::fs::remove_file(&delta);
        }
        self.replay_from_log()?;
        self.has_checkpoint = false;
        self.persist_projection()
    }
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
