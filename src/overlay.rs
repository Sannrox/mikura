//! Clerk-admitted property overlay (ADR 0009).
//!
//! The last accepted patch persists as [`OVERLAY_KIND`] with key `{kind}/{key}`.

use std::collections::{HashMap, HashSet};

use crate::schema::SCHEMA_KIND;
use crate::store::ObjectRecord;

/// Reserved kind for the last accepted property overlay of another identity.
pub const OVERLAY_KIND: &str = "mikura.overlay";

/// Comma-separated property names to omit from a later source snapshot.
pub const OVERLAY_CLEARED: &str = "cleared";

/// Admitted named-property patch for one identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayPatch {
    pub kind: String,
    pub key: String,
    pub props: HashMap<String, String>,
    pub cleared: Vec<String>,
    pub action_id: Option<String>,
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
        if kind == SCHEMA_KIND || kind == OVERLAY_KIND {
            return Err(format!("overlay cannot describe {kind}"));
        }
        let mut props = HashMap::new();
        let mut cleared = Vec::new();
        for (name, value) in &record.props {
            if name == OVERLAY_CLEARED {
                cleared = split_csv(value)?;
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
        Ok(Self {
            kind,
            key,
            props,
            cleared,
            action_id: record.action_id.clone(),
        })
    }

    /// Encode as a `mikura.overlay` record. [`crate::Store::append`] assigns `gen`.
    pub fn to_record(&self) -> Result<ObjectRecord, String> {
        if self.kind == SCHEMA_KIND || self.kind == OVERLAY_KIND {
            return Err(format!("overlay cannot describe {}", self.kind));
        }
        if self.kind.is_empty() || self.key.is_empty() {
            return Err("overlay identity must not be empty".into());
        }
        if self.key.contains('/') {
            return Err("overlay target key must not contain '/'".into());
        }
        let mut props = self.props.clone();
        if props.contains_key(OVERLAY_CLEARED) {
            return Err(format!("{OVERLAY_CLEARED} is reserved on overlay objects"));
        }
        if !self.cleared.is_empty() {
            props.insert(OVERLAY_CLEARED.to_string(), self.cleared.join(","));
        }
        Ok(ObjectRecord {
            gen: 0,
            kind: OVERLAY_KIND.to_string(),
            key: Self::identity_key(&self.kind, &self.key),
            hidden: false,
            action_id: self.action_id.clone(),
            props,
        })
    }

    /// Source props, minus `cleared`, then overlay values.
    pub fn apply(&self, mut source: HashMap<String, String>) -> HashMap<String, String> {
        for name in &self.cleared {
            source.remove(name);
        }
        for (name, value) in &self.props {
            source.insert(name.clone(), value.clone());
        }
        source
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
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for part in raw.split(',') {
        let token = part.trim();
        if token.is_empty() {
            return Err("empty overlay cleared entry".into());
        }
        if !seen.insert(token) {
            return Err(format!("duplicate overlay cleared entry {token}"));
        }
        out.push(token.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_overrides_and_clears() {
        let patch = OverlayPatch {
            kind: "incident".into(),
            key: "inc-1".into(),
            props: HashMap::from([("note".into(), "acked".into())]),
            cleared: vec!["tmp".into()],
            action_id: Some("act-inc-1-note".into()),
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
}
