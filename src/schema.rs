//! Clerk-supplied kind, property, and link rules (ADR 0008).
//!
//! Descriptors persist as ordinary objects of kind [`SCHEMA_KIND`] with key
//! equal to the described kind. Values stay UTF-8 strings. Optional [`SCHEMA_SUMS`]
//! names last-hop measure properties. A store with no visible schema row for
//! a kind accepts unvalidated string records.

use std::collections::{HashMap, HashSet};

use crate::store::ObjectRecord;
use crate::value::PropertyType;

/// Reserved kind for the last accepted descriptor of another kind.
pub const SCHEMA_KIND: &str = "mikura.schema";

/// Comma-separated allowed property names on the described kind.
pub const SCHEMA_PROPERTIES: &str = "properties";

/// Comma-separated required property names. Absent means none required.
pub const SCHEMA_REQUIRED: &str = "required";

/// Comma-separated `name:far_kind:out|in:0..1` link rules. Absent means none.
pub const SCHEMA_LINKS: &str = "links";

/// Comma-separated property names that are last-hop sum measures. Absent means none.
pub const SCHEMA_SUMS: &str = "sums";

/// Comma-separated `name:type` or `name:decimal:<scale>` logical types.
/// Absent means every property is a string (ADR 0012).
pub const SCHEMA_TYPES: &str = "types";

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
    pub sums: Vec<String>,
    /// Declared non-default types. Omitted names are [`PropertyType::String`].
    pub types: Vec<(String, PropertyType)>,
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
            sums: optional_csv(record, SCHEMA_SUMS)?,
            types: parse_types(optional_csv(record, SCHEMA_TYPES)?)?,
        };
        descriptor.check()?;
        for name in record.props.keys() {
            if name != SCHEMA_PROPERTIES
                && name != SCHEMA_REQUIRED
                && name != SCHEMA_LINKS
                && name != SCHEMA_SUMS
                && name != SCHEMA_TYPES
            {
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
        if !self.sums.is_empty() {
            props.insert(SCHEMA_SUMS.to_string(), join_csv(&sorted(&self.sums)));
        }
        if !self.types.is_empty() {
            let mut encoded: Vec<String> = self
                .types
                .iter()
                .map(|(name, ty)| format!("{name}:{}", ty.token()))
                .collect();
            encoded.sort();
            props.insert(SCHEMA_TYPES.to_string(), encoded.join(","));
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
        for (name, value) in &record.props {
            if let Some(ty) = self.property_type(name) {
                ty.parse_canonical(value)
                    .map_err(|err| format!("{err} for {name} on {}", self.kind))?;
            }
        }
        Ok(())
    }

    /// Normalize timestamp offsets in `record.props`. Other types must already
    /// be canonical (ADR 0012).
    pub fn canonicalize_instance(&self, record: &mut ObjectRecord) -> Result<(), String> {
        self.check()?;
        if record.kind != self.kind {
            return Err(format!(
                "schema {} does not apply to {}",
                self.kind, record.kind
            ));
        }
        for (name, value) in record.props.iter_mut() {
            if let Some(ty) = self.property_type(name) {
                *value = ty
                    .canonicalize_write(value)
                    .map_err(|err| format!("{err} for {name} on {}", self.kind))?;
            }
        }
        Ok(())
    }

    /// Declared type, or [`PropertyType::String`] when the name is declared
    /// without `types`.
    pub fn property_type(&self, name: &str) -> Option<PropertyType> {
        if !self.properties.iter().any(|prop| prop == name) {
            return None;
        }
        Some(
            self.types
                .iter()
                .find(|(prop, _)| prop == name)
                .map(|(_, ty)| *ty)
                .unwrap_or(PropertyType::String),
        )
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
        let mut sums = HashSet::new();
        for name in &self.sums {
            token(name, "sum")?;
            if !properties.contains(name.as_str()) {
                return Err(format!("sum property {name} is not declared"));
            }
            if !sums.insert(name.as_str()) {
                return Err(format!("duplicate sum property {name}"));
            }
        }
        let mut types = HashSet::new();
        for (name, ty) in &self.types {
            token(name, "type")?;
            if !properties.contains(name.as_str()) {
                return Err(format!("typed property {name} is not declared"));
            }
            if !types.insert(name.as_str()) {
                return Err(format!("duplicate typed property {name}"));
            }
            if let PropertyType::Decimal { scale } = ty {
                if *scale > 18 {
                    return Err(format!("decimal scale {scale} on {name} exceeds 18"));
                }
            }
        }
        for link in self.links.iter().filter(|link| link.outgoing) {
            if let Some(ty) = self
                .types
                .iter()
                .find(|(name, _)| name == &link.name)
                .map(|(_, ty)| *ty)
            {
                if ty != PropertyType::String {
                    return Err(format!("outgoing link {} must be a string type", link.name));
                }
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
    crate::codec::split_unique_csv(raw, "empty schema list entry", |token| {
        format!("duplicate schema list entry {token}")
    })
}

fn join_csv(parts: &[String]) -> String {
    parts.join(",")
}

fn sorted(parts: &[String]) -> Vec<String> {
    let mut out = parts.to_vec();
    out.sort();
    out
}

fn parse_types(entries: Vec<String>) -> Result<Vec<(String, PropertyType)>, String> {
    let mut types = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        let (name, token) = split_type_entry(&entry)?;
        if !seen.insert(name.clone()) {
            return Err(format!("duplicate typed property {name}"));
        }
        types.push((name, PropertyType::from_token(token)?));
    }
    types.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(types)
}

fn split_type_entry(entry: &str) -> Result<(String, &str), String> {
    let Some((name, token)) = entry.split_once(':') else {
        return Err(format!(
            "type {entry} must be name:string|boolean|integer|timestamp or name:decimal:<scale>"
        ));
    };
    if name.is_empty() || token.is_empty() {
        return Err(format!("type {entry} is empty"));
    }
    Ok((name.to_string(), token))
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
            sums: Vec::new(),
            types: Vec::new(),
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

    #[test]
    fn sums_roundtrip_and_historical_rows_load() {
        let mut schema = descriptor();
        schema.properties = vec!["affects".into(), "amount".into(), "name".into()];
        schema.sums = vec!["amount".into()];
        let record = schema.to_record().unwrap();
        assert_eq!(
            record.props.get(SCHEMA_SUMS).map(String::as_str),
            Some("amount")
        );
        assert_eq!(SchemaDescriptor::from_record(&record).unwrap(), schema);

        let mut historical = descriptor().to_record().unwrap();
        historical.props.remove(SCHEMA_SUMS);
        let loaded = SchemaDescriptor::from_record(&historical).unwrap();
        assert!(loaded.sums.is_empty());

        schema.sums = vec!["missing".into()];
        let err = schema.to_record().unwrap_err();
        assert!(
            err.contains("sum property missing is not declared"),
            "{err}"
        );
    }

    #[test]
    fn types_roundtrip_and_historical_rows_load() {
        let mut schema = descriptor();
        schema.properties = vec![
            "affects".into(),
            "cost".into(),
            "name".into(),
            "open".into(),
        ];
        schema.types = vec![
            ("cost".into(), PropertyType::Decimal { scale: 2 }),
            ("open".into(), PropertyType::Boolean),
        ];
        let record = schema.to_record().unwrap();
        assert_eq!(
            record.props.get(SCHEMA_TYPES).map(String::as_str),
            Some("cost:decimal:2,open:boolean")
        );
        assert_eq!(SchemaDescriptor::from_record(&record).unwrap(), schema);

        let mut historical = descriptor().to_record().unwrap();
        historical.props.remove(SCHEMA_TYPES);
        let loaded = SchemaDescriptor::from_record(&historical).unwrap();
        assert!(loaded.types.is_empty());

        schema.types = vec![("missing".into(), PropertyType::Integer)];
        let err = schema.to_record().unwrap_err();
        assert!(
            err.contains("typed property missing is not declared"),
            "{err}"
        );
    }

    #[test]
    fn validate_rejects_non_canonical_scalars() {
        let mut schema = descriptor();
        schema.properties = vec!["affects".into(), "name".into(), "open".into()];
        schema.types = vec![("open".into(), PropertyType::Boolean)];
        schema
            .validate(&instance(&[
                ("name", "n"),
                ("affects", "svc-api"),
                ("open", "true"),
            ]))
            .unwrap();
        let err = schema
            .validate(&instance(&[
                ("name", "n"),
                ("affects", "svc-api"),
                ("open", "TRUE"),
            ]))
            .unwrap_err();
        assert!(err.contains("invalid boolean"), "{err}");
    }
}
