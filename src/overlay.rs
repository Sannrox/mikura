//! Clerk-admitted property overlay (ADR 0009, ADR 0032).
//!
//! The last accepted patch persists as [`OVERLAY_KIND`] with key `{kind}/{key}`.
//! Default merge is user-wins. A schema [`ConflictPlan`] may select
//! newest-timestamp-wins per kind or per property.

use std::collections::{HashMap, HashSet};

use crate::schema::{ConflictPlan, ConflictStrategy, SCHEMA_KIND};
use crate::store::{ObjectRecord, SOURCE_KIND};
use crate::value::PropertyType;

/// Reserved kind for the last accepted property overlay of another identity.
pub const OVERLAY_KIND: &str = "mikura.overlay";

/// Comma-separated property names to omit from a later source snapshot.
pub const OVERLAY_CLEARED: &str = "cleared";

/// Canonical timestamp of this overlay patch (`YYYY-MM-DDTHH:MM:SS.sssZ`).
/// `@` prefix cannot be a schema property token (ADR 0032).
pub const OVERLAY_AT: &str = "@at";

/// Recorded conflict strategies for the last merge that had a loser (`name:user|newest`).
pub const OVERLAY_CONFLICT: &str = "@conflict";

/// Prefix for losing values recorded on the overlay row (`@lost:<property>`).
pub const OVERLAY_LOST_PREFIX: &str = "@lost:";

/// Losing values plus the strategy that produced them, written on the overlay row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictAudit {
    pub strategies: Vec<(String, ConflictStrategy)>,
    pub lost: HashMap<String, String>,
}

/// Admitted named-property patch for one identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OverlayPatch {
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub props: HashMap<String, String>,
    pub cleared: Vec<String>,
    pub action_id: Option<String>,
    /// Clerk-supplied time of this patch. Required when a newest strategy applies.
    pub at: Option<String>,
}

impl OverlayPatch {
    pub fn identity_key(kind: &str, key: &str) -> String {
        format!("{kind}/{key}")
    }

    /// Decode a persisted `mikura.overlay` row.
    pub fn from_record(record: &ObjectRecord) -> Result<Self, String> {
        if record.kind != OVERLAY_KIND {
            return Err(format!(
                "overlay kind must be {OVERLAY_KIND}, got {}",
                record.kind
            ));
        }
        let (kind, key) = split_identity_key(&record.key)?;
        if kind == SCHEMA_KIND || kind == OVERLAY_KIND || kind == SOURCE_KIND {
            return Err(format!("overlay cannot describe {kind}"));
        }
        let mut props = HashMap::new();
        let mut cleared = Vec::new();
        let mut at = None;
        for (name, value) in &record.props {
            if name == OVERLAY_CLEARED {
                cleared = split_csv(value)?;
                continue;
            }
            if name == OVERLAY_AT {
                at = Some(canonicalize_overlay_at(value)?);
                continue;
            }
            if name == OVERLAY_CONFLICT || name.starts_with(OVERLAY_LOST_PREFIX) {
                // Audit fields live on the row; [`Self::from_record`] is the
                // patch body. [`Self::remapped_audit`] reads them from the
                // stored record when a schema migration rewrites the overlay.
                continue;
            }
            props.insert(name.clone(), value.clone());
        }
        let mut seen = HashSet::new();
        for name in &cleared {
            if !seen.insert(name.as_str()) {
                return Err(format!("duplicate cleared property {name}"));
            }
            if props.contains_key(name) {
                return Err(format!("cleared property {name} also has an override"));
            }
        }
        if record.namespace.is_empty() {
            return Err("namespace required".into());
        }
        Ok(Self {
            namespace: record.namespace.clone(),
            kind,
            key,
            props,
            cleared,
            action_id: record.action_id.clone(),
            at,
        })
    }

    /// Canonical UTC overlay timestamp, if the clerk supplied `at`.
    pub fn canonical_at(&self) -> Result<Option<String>, String> {
        match &self.at {
            Some(raw) => Ok(Some(canonicalize_overlay_at(raw)?)),
            None => Ok(None),
        }
    }

    /// Encode as a `mikura.overlay` record. [`crate::Store::append`] assigns `gen`.
    pub fn to_record(&self) -> Result<ObjectRecord, String> {
        if self.kind == SCHEMA_KIND || self.kind == OVERLAY_KIND || self.kind == SOURCE_KIND {
            return Err(format!("overlay cannot describe {}", self.kind));
        }
        if self.namespace.is_empty() {
            return Err("namespace required".into());
        }
        if self.kind.is_empty() || self.key.is_empty() {
            return Err("overlay identity must not be empty".into());
        }
        if self.key.contains('/') {
            return Err("overlay target key must not contain '/'".into());
        }
        let mut props = self.props.clone();
        for reserved in [OVERLAY_CLEARED, OVERLAY_AT, OVERLAY_CONFLICT] {
            if props.contains_key(reserved) {
                return Err(format!("{reserved} is reserved on overlay objects"));
            }
        }
        if props
            .keys()
            .any(|name| name.starts_with(OVERLAY_LOST_PREFIX))
        {
            return Err(format!(
                "{OVERLAY_LOST_PREFIX} is reserved on overlay objects"
            ));
        }
        if !self.cleared.is_empty() {
            props.insert(OVERLAY_CLEARED.to_string(), self.cleared.join(","));
        }
        if let Some(at) = &self.at {
            props.insert(OVERLAY_AT.to_string(), canonicalize_overlay_at(at)?);
        }
        Ok(ObjectRecord {
            gen: 0,
            namespace: self.namespace.clone(),
            kind: OVERLAY_KIND.to_string(),
            key: Self::identity_key(&self.kind, &self.key),
            hidden: false,
            action_id: self.action_id.clone(),
            props,
            schema_version: 1,
        })
    }

    /// Source props, minus `cleared`, then overlay values (user-wins).
    pub fn apply(&self, mut source: HashMap<String, String>) -> HashMap<String, String> {
        for name in &self.cleared {
            source.remove(name);
        }
        for (name, value) in &self.props {
            source.insert(name.clone(), value.clone());
        }
        source
    }

    /// Merge source with this overlay under `plan`.
    ///
    /// Absent plan is user-wins with no audit row. Explicit `user` records
    /// losing source values. `newest` compares [`OVERLAY_AT`] to the source
    /// `conflict_time` property; equal timestamps keep the overlay.
    pub fn apply_with(
        &self,
        source: HashMap<String, String>,
        plan: Option<&ConflictPlan>,
    ) -> Result<(HashMap<String, String>, Option<ConflictAudit>), String> {
        let Some(plan) = plan.filter(|plan| plan.records_audit() || plan.uses_newest()) else {
            return Ok((self.apply(source), None));
        };
        let overlay_newer = if plan.uses_newest_on(self.props.keys().chain(self.cleared.iter())) {
            Some(self.overlay_is_newer(source.get(plan.time_property()?))?)
        } else {
            None
        };
        let mut merged = source.clone();
        let mut strategies = Vec::new();
        let mut lost = HashMap::new();
        for name in &self.cleared {
            let strategy = plan.strategy_for(name);
            let source_value = source.get(name).cloned();
            let overlay_wins = match strategy {
                ConflictStrategy::User => true,
                ConflictStrategy::Newest => overlay_newer
                    .ok_or_else(|| "newest conflict requires timestamps".to_string())?,
            };
            if overlay_wins {
                merged.remove(name);
                if let Some(value) = source_value {
                    strategies.push((name.clone(), strategy));
                    lost.insert(name.clone(), value);
                }
            } else if source_value.is_some() {
                strategies.push((name.clone(), strategy));
            }
        }
        for (name, overlay_value) in &self.props {
            let strategy = plan.strategy_for(name);
            let source_value = source.get(name);
            let overlay_wins = match strategy {
                ConflictStrategy::User => true,
                ConflictStrategy::Newest => overlay_newer
                    .ok_or_else(|| "newest conflict requires timestamps".to_string())?,
            };
            if overlay_wins {
                if let Some(value) = source_value {
                    if value != overlay_value {
                        strategies.push((name.clone(), strategy));
                        lost.insert(name.clone(), value.clone());
                    }
                }
                merged.insert(name.clone(), overlay_value.clone());
            } else if source_value.map(String::as_str) != Some(overlay_value.as_str()) {
                strategies.push((name.clone(), strategy));
                lost.insert(name.clone(), overlay_value.clone());
            }
        }
        strategies.sort_by(|a, b| a.0.cmp(&b.0));
        let audit = if plan.records_audit() && !strategies.is_empty() {
            Some(ConflictAudit { strategies, lost })
        } else {
            None
        };
        Ok((merged, audit))
    }

    /// Encode an audit onto a copy of this overlay record.
    pub fn to_audit_record(&self, audit: &ConflictAudit) -> Result<ObjectRecord, String> {
        let mut record = self.to_record()?;
        record.props.insert(
            OVERLAY_CONFLICT.to_string(),
            encode_conflict_strategies(&audit.strategies),
        );
        for (name, value) in &audit.lost {
            record
                .props
                .insert(format!("{OVERLAY_LOST_PREFIX}{name}"), value.clone());
        }
        Ok(record)
    }

    /// Recorded conflict audit with property names mapped through `rename`
    /// and entries for `drop` omitted.
    pub fn remapped_audit(
        record: &ObjectRecord,
        rename: &[(String, String)],
        drop: &[String],
    ) -> Result<Option<ConflictAudit>, String> {
        let dropped: HashSet<&str> = drop.iter().map(String::as_str).collect();
        let map_name = |name: &str| -> Option<String> {
            if dropped.contains(name) {
                return None;
            }
            Some(
                rename
                    .iter()
                    .find(|(old, _)| old == name)
                    .map(|(_, new)| new.clone())
                    .unwrap_or_else(|| name.to_string()),
            )
        };
        let mut strategies = Vec::new();
        if let Some(raw) = Self::recorded_conflict(record) {
            for (name, strategy) in decode_conflict_strategies(raw)? {
                if let Some(name) = map_name(&name) {
                    strategies.push((name, strategy));
                }
            }
        }
        let mut lost = HashMap::new();
        for (name, value) in Self::recorded_lost(record) {
            if let Some(name) = map_name(&name) {
                lost.insert(name, value);
            }
        }
        if strategies.is_empty() && lost.is_empty() {
            return Ok(None);
        }
        strategies.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Some(ConflictAudit { strategies, lost }))
    }

    /// Strategy tokens recorded on an overlay row, if present.
    pub fn recorded_conflict(record: &ObjectRecord) -> Option<&str> {
        record.props.get(OVERLAY_CONFLICT).map(String::as_str)
    }

    /// Losing values recorded on an overlay row.
    pub fn recorded_lost(record: &ObjectRecord) -> HashMap<String, String> {
        record
            .props
            .iter()
            .filter_map(|(name, value)| {
                name.strip_prefix(OVERLAY_LOST_PREFIX)
                    .map(|property| (property.to_string(), value.clone()))
            })
            .collect()
    }

    fn overlay_is_newer(&self, source_at: Option<&String>) -> Result<bool, String> {
        let overlay_at = self
            .at
            .as_deref()
            .ok_or_else(|| "newest conflict requires overlay at".to_string())?;
        let source_at =
            source_at.ok_or_else(|| "newest conflict requires source timestamp".to_string())?;
        let overlay_ts = PropertyType::Timestamp.canonicalize_write(overlay_at)?;
        let source_ts = PropertyType::Timestamp.canonicalize_write(source_at)?;
        Ok(overlay_ts >= source_ts)
    }
}

fn split_identity_key(raw: &str) -> Result<(String, String), String> {
    let Some((kind, key)) = raw.split_once('/') else {
        return Err(format!("overlay key {raw} must be kind/key"));
    };
    if kind.is_empty() || key.is_empty() || key.contains('/') {
        return Err(format!("overlay key {raw} must be kind/key"));
    }
    Ok((kind.to_string(), key.to_string()))
}

fn split_csv(raw: &str) -> Result<Vec<String>, String> {
    crate::codec::split_unique_csv(raw, "empty overlay cleared entry", |token| {
        format!("duplicate overlay cleared entry {token}")
    })
}

fn canonicalize_overlay_at(raw: &str) -> Result<String, String> {
    PropertyType::Timestamp
        .canonicalize_write(raw)
        .map_err(|err| format!("overlay at: {err}"))
}

fn decode_conflict_strategies(raw: &str) -> Result<Vec<(String, ConflictStrategy)>, String> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in raw.split(',') {
        let Some((name, token)) = entry.split_once(':') else {
            return Err(format!(
                "overlay conflict {entry} must be name:user or name:newest"
            ));
        };
        if name.is_empty() || token.is_empty() {
            return Err(format!("overlay conflict {entry} is empty"));
        }
        if !seen.insert(name) {
            return Err(format!("duplicate overlay conflict {name}"));
        }
        out.push((name.to_string(), ConflictStrategy::from_token(token)?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn encode_conflict_strategies(strategies: &[(String, ConflictStrategy)]) -> String {
    let mut encoded: Vec<String> = strategies
        .iter()
        .map(|(name, strategy)| format!("{name}:{}", strategy.token()))
        .collect();
    encoded.sort();
    encoded.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_overrides_and_clears() {
        let patch = OverlayPatch {
            namespace: "ns".into(),
            kind: "incident".into(),
            key: "inc-1".into(),
            props: HashMap::from([("note".into(), "acked".into())]),
            cleared: vec!["tmp".into()],
            action_id: Some("act-inc-1-note".into()),
            at: None,
        };
        let merged = patch.apply(HashMap::from([
            ("name".into(), "elevated latency".into()),
            ("tmp".into(), "x".into()),
        ]));
        assert_eq!(merged.get("note").map(String::as_str), Some("acked"));
        assert_eq!(
            merged.get("name").map(String::as_str),
            Some("elevated latency")
        );
        assert!(!merged.contains_key("tmp"));
        let record = patch.to_record().unwrap();
        assert_eq!(record.key, "incident/inc-1");
        assert_eq!(OverlayPatch::from_record(&record).unwrap(), patch);
    }

    #[test]
    fn newest_keeps_overlay_on_tie_and_source_when_newer() {
        let patch = OverlayPatch {
            namespace: "ns".into(),
            kind: "incident".into(),
            key: "inc-1".into(),
            props: HashMap::from([("status".into(), "acked".into())]),
            cleared: Vec::new(),
            action_id: None,
            at: Some("2026-10-11T12:00:00.000Z".into()),
        };
        let plan = ConflictPlan {
            default: ConflictStrategy::Newest,
            properties: HashMap::new(),
            time_property: Some("updated_at".into()),
            record_audit: true,
        };
        let source = HashMap::from([
            ("status".into(), "open".into()),
            ("updated_at".into(), "2026-10-11T12:00:00.000Z".into()),
        ]);
        let (merged, audit) = patch.apply_with(source, Some(&plan)).unwrap();
        assert_eq!(merged.get("status").map(String::as_str), Some("acked"));
        assert_eq!(
            audit.unwrap().lost.get("status").map(String::as_str),
            Some("open")
        );

        let source = HashMap::from([
            ("status".into(), "closed".into()),
            ("updated_at".into(), "2026-10-11T13:00:00.000Z".into()),
        ]);
        let (merged, audit) = patch.apply_with(source, Some(&plan)).unwrap();
        assert_eq!(merged.get("status").map(String::as_str), Some("closed"));
        assert_eq!(
            audit.unwrap().lost.get("status").map(String::as_str),
            Some("acked")
        );

        let patch = OverlayPatch {
            namespace: "ns".into(),
            kind: "incident".into(),
            key: "inc-1".into(),
            props: HashMap::new(),
            cleared: vec!["note".into()],
            action_id: None,
            at: Some("2026-10-11T11:00:00.000Z".into()),
        };
        let source = HashMap::from([
            ("note".into(), "source-note".into()),
            ("updated_at".into(), "2026-10-11T12:00:00.000Z".into()),
        ]);
        let (merged, audit) = patch.apply_with(source, Some(&plan)).unwrap();
        assert_eq!(merged.get("note").map(String::as_str), Some("source-note"));
        let audit = audit.unwrap();
        assert_eq!(
            audit.strategies,
            vec![("note".into(), ConflictStrategy::Newest)]
        );
        assert!(audit.lost.is_empty());
    }

    #[test]
    fn overlay_metadata_keys_do_not_consume_instance_names() {
        let patch = OverlayPatch {
            namespace: "ns".into(),
            kind: "incident".into(),
            key: "inc-1".into(),
            props: HashMap::from([
                ("at".into(), "not-a-timestamp".into()),
                ("conflict".into(), "mine".into()),
                ("lost:note".into(), "keep".into()),
            ]),
            cleared: Vec::new(),
            action_id: None,
            at: Some("2026-10-11T12:00:00+02:00".into()),
        };
        let record = patch.to_record().unwrap();
        assert_eq!(
            record.props.get("at").map(String::as_str),
            Some("not-a-timestamp")
        );
        assert_eq!(
            record.props.get(OVERLAY_AT).map(String::as_str),
            Some("2026-10-11T10:00:00.000Z")
        );
        assert_eq!(
            record.props.get("conflict").map(String::as_str),
            Some("mine")
        );
        assert_eq!(
            record.props.get("lost:note").map(String::as_str),
            Some("keep")
        );
        let decoded = OverlayPatch::from_record(&record).unwrap();
        assert_eq!(
            decoded.props.get("at").map(String::as_str),
            Some("not-a-timestamp")
        );
        assert_eq!(decoded.at.as_deref(), Some("2026-10-11T10:00:00.000Z"));
        assert_eq!(
            decoded.props.get("conflict").map(String::as_str),
            Some("mine")
        );
        assert_eq!(
            decoded.props.get("lost:note").map(String::as_str),
            Some("keep")
        );
        assert_eq!(
            patch.canonical_at().unwrap().as_deref(),
            Some("2026-10-11T10:00:00.000Z")
        );
    }
}
