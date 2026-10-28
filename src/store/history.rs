//! Read-only object history over the committed log (ADR 0036, #241).
//!
//! Every generation stays on the log, so history is a scan of committed
//! pages. Each version is presented the way [`Store::load`] would have
//! presented it: current property names, declared drops omitted, the
//! request restriction decided on that version's own values, and denied
//! properties omitted.

use super::{ObjectRecord, Store};
use crate::acl::PropertyAcl;

/// How one generation came to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionOrigin {
    /// A source snapshot or refresh. It keeps the previous generation's
    /// Action id when an admitted overlay merges into it.
    Source,
    /// A governed write: the generation names an Action id the previous
    /// generation did not (ADR 0006). Replays write no generation.
    Edit,
    /// A hide (delete) of the identity (ADR 0008).
    Hide,
}

/// One committed generation of an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectVersion {
    /// The generation as `load` would have shown it. `schema_version` is the
    /// descriptor version it was written under.
    pub record: ObjectRecord,
    /// The log page that holds this generation. Every commit ends on a page
    /// boundary, so the version is in effect at every commit point at or
    /// after this page.
    pub page: u32,
    pub origin: VersionOrigin,
}

/// A point in an object's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AsOf {
    /// The latest generation at or below this one.
    Generation(u64),
    /// The state after the commit that ended at this page. Pass a value
    /// [`Store::committed_pages`] returned after a commit: a page inside a
    /// batch would show part of that batch.
    CommitPoint(u32),
}

impl Store {
    /// Every committed generation of `(namespace, kind, key)`, oldest first.
    /// A version the restriction hides is left out; denied properties are
    /// omitted from every version. Uncommitted writes are not history.
    pub fn history(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
    ) -> Result<Vec<ObjectVersion>, String> {
        let namespace = self.require_known_namespace(namespace)?;
        let stored = self.stored_versions(namespace, kind, key)?;
        stored
            .into_iter()
            .filter_map(|(page, record, origin)| {
                self.present_version(page, record, origin, acl).transpose()
            })
            .collect()
    }

    /// The generation of `(namespace, kind, key)` in effect at `at`, as
    /// `load` would have answered then. Missing, not yet written, or hidden
    /// by the restriction at that point fails closed like `load`.
    pub fn load_as_of(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
        acl: &PropertyAcl,
        at: AsOf,
    ) -> Result<ObjectVersion, String> {
        let namespace = self.require_known_namespace(namespace)?;
        let missing = || format!("unknown identity {namespace}/{kind}/{key}");
        let (page, record, origin) = self
            .stored_versions(namespace, kind, key)?
            .into_iter()
            .rev()
            .find(|(page, record, _)| match at {
                AsOf::Generation(gen) => record.gen <= gen,
                AsOf::CommitPoint(point) => *page <= point,
            })
            .ok_or_else(missing)?;
        self.present_version(page, record, origin, acl)?
            .ok_or_else(missing)
    }

    /// Committed generations of one identity with their pages and origins,
    /// in log order.
    fn stored_versions(
        &self,
        namespace: &str,
        kind: &str,
        key: &str,
    ) -> Result<Vec<(u32, ObjectRecord, VersionOrigin)>, String> {
        let stored = self.writer.read_applied_pages(&self.log, |record| {
            record.namespace == namespace && record.kind == kind && record.key == key
        })?;
        if stored.is_empty() {
            return Err(format!("unknown identity {namespace}/{kind}/{key}"));
        }
        let mut previous_action: Option<String> = None;
        Ok(stored
            .into_iter()
            .map(|(page, record)| {
                let origin = if record.hidden {
                    VersionOrigin::Hide
                } else if record.action_id.is_some() && record.action_id != previous_action {
                    VersionOrigin::Edit
                } else {
                    VersionOrigin::Source
                };
                previous_action = record.action_id.clone();
                (page, record, origin)
            })
            .collect())
    }

    /// `None` when the restriction hides this version.
    fn present_version(
        &self,
        page: u32,
        mut record: ObjectRecord,
        origin: VersionOrigin,
        acl: &PropertyAcl,
    ) -> Result<Option<ObjectVersion>, String> {
        let written = record.schema_version.max(1);
        self.rename_forward(&mut record, written);
        self.apply_schema_view(&mut record, None)?;
        if !acl.object_visible(&record.kind, &record.key, |property| {
            record.props.get(property).map(String::as_str)
        }) {
            return Ok(None);
        }
        record.props = acl.omit_denied(&record.kind, record.props);
        record.schema_version = written;
        Ok(Some(ObjectVersion {
            record,
            page,
            origin,
        }))
    }

    /// Carry property names written under `written` to the current names.
    /// Values keep the bytes they were written with; a name the target
    /// already holds is left as stored.
    fn rename_forward(&self, record: &mut ObjectRecord, written: u32) {
        let Some(rows) = self
            .schema_history
            .get(&(record.namespace.clone(), record.kind.clone()))
        else {
            return;
        };
        let props = &mut record.props;
        for row in rows.iter().filter(|row| row.effective_version() > written) {
            for (old, new) in &row.migration.rename {
                if old != new && !props.contains_key(new) {
                    if let Some(value) = props.remove(old) {
                        props.insert(new.clone(), value);
                    }
                }
            }
        }
    }
}
