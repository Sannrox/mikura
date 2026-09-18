//! Clerk-supplied kind, property, and link rules (ADR 0008).
//!
//! Descriptors persist as ordinary objects of kind [`SCHEMA_KIND`] with key
//! equal to the described kind. Values stay UTF-8 strings. A store with no
//! visible schema row for a kind accepts unvalidated string records.

use std::collections::{HashMap, HashSet};

use crate::store::ObjectRecord;

/// Reserved kind for the last accepted descriptor of another kind.
pub const SCHEMA_KIND: &str = "mikura.schema";

/// Comma-separated allowed property names on the described kind.
pub const SCHEMA_PROPERTIES: &str = "properties";

/// Comma-separated required property names. Absent means none required.
pub const SCHEMA_REQUIRED: &str = "required";

/// Comma-separated `name:far_kind:out|in:0..1` link rules. Absent means none.
pub const SCHEMA_LINKS: &str = "links";

const LINK_CARDINALITY: &str = "0..1";

/// One named relation on a descriptor.
///
/// Outgoing links are stored on the described kind as `props[name]`. Incoming
/// links name a property on `far_kind` that points here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaLink {
    pub name: String,
    pub far_kind: String,
    pub outgoing: bool,
}

/// Clerk-authored rules for one kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaDescriptor {
    pub kind: String,
    pub properties: Vec<String>,
    pub required: Vec<String>,
    pub links: Vec<SchemaLink>,
}

impl SchemaDescriptor {
    /// Decode a persisted `mikura.schema` row.
    pub fn from_record(record: &ObjectRecord) -> Result<Self, String> {
        if record.kind != SCHEMA_KIND {
            return Err(format!(
                "schema kind must be {SCHEMA_KIND}, got {}",
                record.kind
            ));
        }
        let descriptor = Self {
            kind: record.key.clone(),
            properties: required_csv(record, SCHEMA_PROPERTIES)?,
            required: optional_csv(record, SCHEMA_REQUIRED)?,
            links: parse_links(optional_csv(record, SCHEMA_LINKS)?)?,
        };
        descriptor.check()?;
        for name in record.props.keys() {
            if name != SCHEMA_PROPERTIES && name != SCHEMA_REQUIRED && name != SCHEMA_LINKS {
                return Err(format!("unknown schema property {name}"));
            }
        }
        Ok(descriptor)
    }

    /// Encode as a `mikura.schema` record. [`crate::Store::append`] assigns `gen`.
    pub fn to_record(&self) -> Result<ObjectRecord, String> {
        self.check()?;
        let mut props = HashMap::new();
        props.insert(
            SCHEMA_PROPERTIES.to_string(),
            join_csv(&sorted(&self.properties)),
        );
        if !self.required.is_empty() {
            props.insert(
                SCHEMA_REQUIRED.to_string(),
                join_csv(&sorted(&self.required)),
            );
        }
        if !self.links.is_empty() {
            let mut encoded: Vec<String> = self.links.iter().map(encode_link).collect();
            encoded.sort();
            props.insert(SCHEMA_LINKS.to_string(), encoded.join(","));
        }
        Ok(ObjectRecord {
            gen: 0,
            kind: SCHEMA_KIND.to_string(),
            key: self.kind.clone(),
            hidden: false,
            action_id: None,
            props,
        })
    }

    /// Fail closed when `record` is a visible instance of this kind that
    /// violates the closed property set, required keys, or link cardinality.
    pub fn validate(&self, record: &ObjectRecord) -> Result<(), String> {
        self.check()?;
        if record.kind != self.kind {
            return Err(format!(
                "schema {} does not apply to {}",
                self.kind, record.kind
            ));
        }
        let allowed: HashSet<&str> = self.properties.iter().map(String::as_str).collect();
        for name in record.props.keys() {
            if !allowed.contains(name.as_str()) {
                return Err(format!("unknown property {name} on {}", self.kind));
            }
        }
        for name in &self.required {
            if !record.props.contains_key(name) {
                return Err(format!("missing required property {name} on {}", self.kind));
            }
        }
        for link in self.links.iter().filter(|link| link.outgoing) {
            match record.props.get(&link.name) {
                None => {}
                Some(value) if value.is_empty() => {
                    return Err(format!(
                        "link {} on {} must be a non-empty key",
                        link.name, self.kind
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    fn check(&self) -> Result<(), String> {
        token(&self.kind, "kind")?;
        if self.kind == SCHEMA_KIND {
            return Err(format!("{SCHEMA_KIND} is reserved"));
        }
        let mut properties = HashSet::new();
        for name in &self.properties {
            token(name, "property")?;
            if !properties.insert(name.as_str()) {
                return Err(format!("duplicate property {name}"));
            }
        }
        let mut required = HashSet::new();
        for name in &self.required {
            token(name, "required")?;
            if !properties.contains(name.as_str()) {
                return Err(format!("required property {name} is not declared"));
            }
            if !required.insert(name.as_str()) {
                return Err(format!("duplicate required property {name}"));
            }
        }
        let mut links = HashSet::new();
        for link in &self.links {
            token(&link.name, "link")?;
            token(&link.far_kind, "far kind")?;
            if link.far_kind == SCHEMA_KIND {
                return Err(format!("link {} cannot target {SCHEMA_KIND}", link.name));
            }
            if !links.insert(link.name.as_str()) {
                return Err(format!("duplicate link {}", link.name));
            }
            if link.outgoing && !properties.contains(link.name.as_str()) {
                return Err(format!(
                    "outgoing link {} is not a declared property",
                    link.name
                ));
            }
        }
        Ok(())
    }
}

fn required_csv(record: &ObjectRecord, name: &str) -> Result<Vec<String>, String> {
    let raw = record
        .props
        .get(name)
        .ok_or_else(|| format!("missing schema {name}"))?;
    split_csv(raw)
}

fn optional_csv(record: &ObjectRecord, name: &str) -> Result<Vec<String>, String> {
    match record.props.get(name) {
        None => Ok(Vec::new()),
        Some(raw) => split_csv(raw),
    }
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
            return Err("empty schema list entry".into());
        }
        if !seen.insert(token) {
            return Err(format!("duplicate schema list entry {token}"));
        }
        out.push(token.to_string());
    }
    Ok(out)
}

fn join_csv(parts: &[String]) -> String {
    parts.join(",")
}

fn sorted(parts: &[String]) -> Vec<String> {
    let mut out = parts.to_vec();
    out.sort();
    out
}

fn parse_links(entries: Vec<String>) -> Result<Vec<SchemaLink>, String> {
    entries
        .into_iter()
        .map(|entry| parse_link(&entry))
        .collect()
}

fn parse_link(entry: &str) -> Result<SchemaLink, String> {
    let parts: Vec<&str> = entry.split(':').collect();
    if parts.len() != 4 {
        return Err(format!(
            "link {entry} must be name:far_kind:out|in:{LINK_CARDINALITY}"
        ));
    }
    let outgoing = match parts[2] {
        "out" => true,
        "in" => false,
        other => return Err(format!("link direction must be out or in, got {other}")),
    };
    if parts[3] != LINK_CARDINALITY {
        return Err(format!(
            "link cardinality must be {LINK_CARDINALITY}, got {}",
            parts[3]
        ));
    }
    Ok(SchemaLink {
        name: parts[0].to_string(),
        far_kind: parts[1].to_string(),
        outgoing,
    })
}

fn encode_link(link: &SchemaLink) -> String {
    let direction = if link.outgoing { "out" } else { "in" };
    format!(
        "{}:{}:{}:{LINK_CARDINALITY}",
        link.name, link.far_kind, direction
    )
}

fn token(value: &str, label: &str) -> Result<(), String> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(format!("{label} must not be empty"));
    };
    if !first.is_ascii_alphabetic() {
        return Err(format!("{label} {value} must start with a letter"));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.') {
        return Err(format!("{label} {value} is not a schema token"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> SchemaDescriptor {
        SchemaDescriptor {
            kind: "incident".into(),
            properties: vec!["affects".into(), "name".into()],
            required: vec!["name".into()],
            links: vec![SchemaLink {
                name: "affects".into(),
                far_kind: "component".into(),
                outgoing: true,
            }],
        }
    }

    fn instance(props: &[(&str, &str)]) -> ObjectRecord {
        ObjectRecord {
            gen: 1,
            kind: "incident".into(),
            key: "inc-1".into(),
            hidden: false,
            action_id: None,
            props: props
                .iter()
                .map(|(name, value)| ((*name).into(), (*value).into()))
                .collect(),
        }
    }

    #[test]
    fn record_roundtrip_preserves_rules() {
        let schema = descriptor();
        let record = schema.to_record().unwrap();
        assert_eq!(record.kind, SCHEMA_KIND);
        assert_eq!(record.key, "incident");
        assert_eq!(
            record.props.get(SCHEMA_PROPERTIES).map(String::as_str),
            Some("affects,name")
        );
        assert_eq!(
            record.props.get(SCHEMA_LINKS).map(String::as_str),
            Some("affects:component:out:0..1")
        );
        assert_eq!(SchemaDescriptor::from_record(&record).unwrap(), schema);
    }

    #[test]
    fn validate_rejects_unknown_missing_and_empty_link() {
        let schema = descriptor();
        schema
            .validate(&instance(&[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
            ]))
            .unwrap();
        let unknown = schema
            .validate(&instance(&[("name", "n"), ("note", "acked")]))
            .unwrap_err();
        assert!(unknown.contains("unknown property"), "{unknown}");
        let missing = schema
            .validate(&instance(&[("affects", "svc-api")]))
            .unwrap_err();
        assert!(missing.contains("missing required"), "{missing}");
        let empty = schema
            .validate(&instance(&[("name", "n"), ("affects", "")]))
            .unwrap_err();
        assert!(empty.contains("non-empty key"), "{empty}");
    }

    #[test]
    fn reserved_kind_and_unknown_schema_keys_fail() {
        let mut reserved = descriptor();
        reserved.kind = SCHEMA_KIND.into();
        assert!(reserved.to_record().unwrap_err().contains("reserved"));
        let mut record = descriptor().to_record().unwrap();
        record.props.insert("note".into(), "x".into());
        let err = SchemaDescriptor::from_record(&record).unwrap_err();
        assert!(err.contains("unknown schema property"), "{err}");
    }
}
