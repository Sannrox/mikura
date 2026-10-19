//! Clerk-supplied kind, property, and link rules (ADR 0008, ADR 0013, ADR 0032, ADR 0035).
//!
//! Descriptors persist as ordinary objects of kind [`SCHEMA_KIND`] with key
//! equal to the described kind. Values stay UTF-8 strings. Optional [`SCHEMA_SUMS`]
//! names last-hop measure properties. Optional [`SCHEMA_INDEXES`] names
//! properties evaluate may look up instead of scanning. Optional [`SCHEMA_CONFLICT`] selects
//! overlay merge strategy. A store with no visible schema row for
//! a kind accepts unvalidated string records. A later visible descriptor is
//! a new schema version: additive changes commit as before; recast, rename,
//! retarget, and drop need a declared migration (ADR 0035).

use std::collections::{HashMap, HashSet};

use crate::store::ObjectRecord;
use crate::value::{PropertyType, VECTOR_DIM_MAX};

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

/// Comma-separated property names evaluate may look up as secondary indexes.
/// Absent means scan. Names must already be in [`SCHEMA_PROPERTIES`].
pub const SCHEMA_INDEXES: &str = "indexes";

/// Comma-separated `name:type` logical types (`boolean`, `integer`,
/// `timestamp`, `string`, `decimal:<scale>`, `vector:<dim>`, `tsref`,
/// `geo:point`, `geo:shape`, `media`). Absent means every property is a
/// string (ADR 0012, ADR 0043).
pub const SCHEMA_TYPES: &str = "types";

/// Kind-wide overlay conflict strategy: `user` or `newest`. Absent means user-wins.
pub const SCHEMA_CONFLICT: &str = "conflict";

/// Comma-separated `name:user|newest` per-property overlay conflict overrides.
pub const SCHEMA_CONFLICT_PROPERTIES: &str = "conflict_properties";

/// Instance timestamp property compared to overlay `at` for newest-wins.
pub const SCHEMA_CONFLICT_TIME: &str = "conflict_time";

/// Monotonic schema version for the described kind. Absent means 1 on a first
/// row and previous+1 on a replacement (ADR 0035).
pub const SCHEMA_VERSION: &str = "version";

/// Comma-separated `old:new` property renames for a breaking replacement.
pub const SCHEMA_MIGRATE_RENAME: &str = "migrate_rename";

/// Comma-separated `name:type` recasts (name after rename) for a breaking replacement.
pub const SCHEMA_MIGRATE_CAST: &str = "migrate_cast";

/// Comma-separated property names dropped by a breaking replacement.
pub const SCHEMA_MIGRATE_DROP: &str = "migrate_drop";

/// Comma-separated `name:far_kind:out|in` outgoing-link retargets.
pub const SCHEMA_MIGRATE_RETARGET: &str = "migrate_retarget";

/// How overlay and source values combine on a later source write (ADR 0032).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictStrategy {
    /// Overlay values replace source (today's default).
    User,
    /// The side with the newer timestamp wins; equal timestamps keep the overlay.
    Newest,
}

impl ConflictStrategy {
    pub fn token(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Newest => "newest",
        }
    }

    pub fn from_token(token: &str) -> Result<Self, String> {
        match token {
            "user" => Ok(Self::User),
            "newest" => Ok(Self::Newest),
            other => Err(format!(
                "conflict strategy must be user or newest, got {other}"
            )),
        }
    }
}

/// Resolved overlay merge plan from a descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictPlan {
    pub default: ConflictStrategy,
    pub properties: HashMap<String, ConflictStrategy>,
    pub time_property: Option<String>,
    pub record_audit: bool,
}

impl ConflictPlan {
    pub fn strategy_for(&self, name: &str) -> ConflictStrategy {
        self.properties.get(name).copied().unwrap_or(self.default)
    }

    pub fn records_audit(&self) -> bool {
        self.record_audit
    }

    pub fn uses_newest(&self) -> bool {
        self.default == ConflictStrategy::Newest
            || self
                .properties
                .values()
                .any(|strategy| *strategy == ConflictStrategy::Newest)
    }

    /// True when this overlay patch has a property or clear that uses newest.
    pub fn uses_newest_on(&self, names: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
        names
            .into_iter()
            .any(|name| self.strategy_for(name.as_ref()) == ConflictStrategy::Newest)
    }

    pub fn time_property(&self) -> Result<&str, String> {
        self.time_property
            .as_deref()
            .ok_or_else(|| "newest conflict requires conflict_time".to_string())
    }
}

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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaDescriptor {
    pub kind: String,
    pub properties: Vec<String>,
    pub required: Vec<String>,
    pub links: Vec<SchemaLink>,
    pub sums: Vec<String>,
    /// Declared secondary-index properties. Evaluate may look these up
    /// instead of scanning. Empty means scan every identity.
    pub indexes: Vec<String>,
    /// Declared non-default types. Omitted names are [`PropertyType::String`].
    pub types: Vec<(String, PropertyType)>,
    /// Kind-wide overlay merge strategy. `None` is implicit user-wins with no audit row.
    pub conflict: Option<ConflictStrategy>,
    /// Per-property overlay merge overrides.
    pub conflict_properties: Vec<(String, ConflictStrategy)>,
    /// Source timestamp property compared to overlay `at` when any strategy is newest.
    pub conflict_time: Option<String>,
    /// 0 means unspecified: 1 on a first row, previous+1 on a replacement.
    pub version: u32,
    /// Declared breaking mapping from the previous visible descriptor.
    pub migration: SchemaMigration,
}

/// Clerk-declared mapping for one breaking descriptor replacement (ADR 0035).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaMigration {
    pub rename: Vec<(String, String)>,
    pub cast: Vec<(String, PropertyType)>,
    pub drop: Vec<String>,
    pub retarget: Vec<SchemaLink>,
}

impl SchemaMigration {
    pub fn is_empty(&self) -> bool {
        self.rename.is_empty()
            && self.cast.is_empty()
            && self.drop.is_empty()
            && self.retarget.is_empty()
    }

    /// Rename keys in place. Cast and drop are left to [`Self::apply`].
    pub fn apply_renames(&self, props: &mut HashMap<String, String>) -> Result<(), String> {
        for (old, new) in &self.rename {
            if old == new {
                continue;
            }
            if let Some(value) = props.remove(old) {
                if props.contains_key(new) {
                    return Err(format!("rename {old} to {new} collides"));
                }
                props.insert(new.clone(), value);
            }
        }
        Ok(())
    }

    /// Apply rename, then recast, then drop. Cast failures collect into `Err`.
    pub fn apply(
        &self,
        props: &mut HashMap<String, String>,
        drop_keys: bool,
    ) -> Result<(), String> {
        self.apply_renames(props)?;
        let mut errors = Vec::new();
        for (name, ty) in &self.cast {
            if let Some(value) = props.get(name) {
                match ty.canonicalize_write(value) {
                    Ok(canonical) => {
                        props.insert(name.clone(), canonical);
                    }
                    Err(err) => errors.push(format!("{name}: {err}")),
                }
            }
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        if drop_keys {
            for name in &self.drop {
                props.remove(name);
            }
        }
        Ok(())
    }

    /// Inverse of [`Self::apply`] for rollback preview. Dropped keys that are
    /// still stored reappear. Recast stays in canonical live form so load and
    /// evaluate share one projection; `Store::history` reads the original
    /// generation bytes (ADR 0036).
    pub fn reverse(&self, props: &mut HashMap<String, String>) {
        for (old, new) in self.rename.iter().rev() {
            if old == new {
                continue;
            }
            if let Some(value) = props.remove(new) {
                props.insert(old.clone(), value);
            }
        }
    }
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
            indexes: optional_csv(record, SCHEMA_INDEXES)?,
            types: parse_types(optional_csv(record, SCHEMA_TYPES)?)?,
            conflict: optional_conflict(record)?,
            conflict_properties: parse_conflict_properties(optional_csv(
                record,
                SCHEMA_CONFLICT_PROPERTIES,
            )?)?,
            conflict_time: optional_token(record, SCHEMA_CONFLICT_TIME)?,
            version: optional_version(record)?,
            migration: SchemaMigration {
                rename: parse_rename(optional_csv(record, SCHEMA_MIGRATE_RENAME)?)?,
                cast: parse_types(optional_csv(record, SCHEMA_MIGRATE_CAST)?)?,
                drop: optional_csv(record, SCHEMA_MIGRATE_DROP)?,
                retarget: parse_retarget(optional_csv(record, SCHEMA_MIGRATE_RETARGET)?)?,
            },
        };
        descriptor.check()?;
        for name in record.props.keys() {
            if name != SCHEMA_PROPERTIES
                && name != SCHEMA_REQUIRED
                && name != SCHEMA_LINKS
                && name != SCHEMA_SUMS
                && name != SCHEMA_INDEXES
                && name != SCHEMA_TYPES
                && name != SCHEMA_CONFLICT
                && name != SCHEMA_CONFLICT_PROPERTIES
                && name != SCHEMA_CONFLICT_TIME
                && name != SCHEMA_VERSION
                && name != SCHEMA_MIGRATE_RENAME
                && name != SCHEMA_MIGRATE_CAST
                && name != SCHEMA_MIGRATE_DROP
                && name != SCHEMA_MIGRATE_RETARGET
            {
                return Err(format!("unknown schema property {name}"));
            }
        }
        Ok(descriptor)
    }

    /// Encode as a `mikura.schema` record. [`crate::Store::append`] assigns `gen`.
    pub fn to_record(&self, namespace: impl Into<String>) -> Result<ObjectRecord, String> {
        self.check()?;
        let namespace = namespace.into();
        if namespace.is_empty() {
            return Err("namespace required".into());
        }
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
        if !self.indexes.is_empty() {
            props.insert(SCHEMA_INDEXES.to_string(), join_csv(&sorted(&self.indexes)));
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
        if let Some(conflict) = self.conflict {
            props.insert(SCHEMA_CONFLICT.to_string(), conflict.token().to_string());
        }
        if !self.conflict_properties.is_empty() {
            let mut encoded: Vec<String> = self
                .conflict_properties
                .iter()
                .map(|(name, strategy)| format!("{name}:{}", strategy.token()))
                .collect();
            encoded.sort();
            props.insert(SCHEMA_CONFLICT_PROPERTIES.to_string(), encoded.join(","));
        }
        if let Some(time) = &self.conflict_time {
            props.insert(SCHEMA_CONFLICT_TIME.to_string(), time.clone());
        }
        if self.version > 0 {
            props.insert(SCHEMA_VERSION.to_string(), self.version.to_string());
        }
        if !self.migration.rename.is_empty() {
            let mut encoded: Vec<String> = self
                .migration
                .rename
                .iter()
                .map(|(old, new)| format!("{old}:{new}"))
                .collect();
            encoded.sort();
            props.insert(SCHEMA_MIGRATE_RENAME.to_string(), encoded.join(","));
        }
        if !self.migration.cast.is_empty() {
            let mut encoded: Vec<String> = self
                .migration
                .cast
                .iter()
                .map(|(name, ty)| format!("{name}:{}", ty.token()))
                .collect();
            encoded.sort();
            props.insert(SCHEMA_MIGRATE_CAST.to_string(), encoded.join(","));
        }
        if !self.migration.drop.is_empty() {
            props.insert(
                SCHEMA_MIGRATE_DROP.to_string(),
                join_csv(&sorted(&self.migration.drop)),
            );
        }
        if !self.migration.retarget.is_empty() {
            let mut encoded: Vec<String> = self
                .migration
                .retarget
                .iter()
                .map(|link| {
                    format!(
                        "{}:{}:{}",
                        link.name,
                        link.far_kind,
                        if link.outgoing { "out" } else { "in" }
                    )
                })
                .collect();
            encoded.sort();
            props.insert(SCHEMA_MIGRATE_RETARGET.to_string(), encoded.join(","));
        }
        Ok(ObjectRecord {
            gen: 0,
            namespace,
            kind: SCHEMA_KIND.to_string(),
            key: self.kind.clone(),
            hidden: false,
            action_id: None,
            props,
            schema_version: 1,
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

    /// Version this row represents. Unspecified (0) is 1.
    pub fn effective_version(&self) -> u32 {
        self.version.max(1)
    }

    /// Fail closed when `next` recasts, renames, retargets, or drops without a
    /// covering migration, or when the version is not previous+1 (ADR 0035).
    ///
    /// Additive optional/required properties, shrinking `required`, new typed
    /// names, incoming-link, `sums`, and `indexes` edits pass without migrate keys.
    pub fn check_replacement(&self, next: &Self) -> Result<Self, String> {
        if self.kind != next.kind {
            return Err(format!("schema {} cannot replace {}", next.kind, self.kind));
        }
        let prev_v = self.effective_version();
        let next_v = if next.version == 0 {
            prev_v + 1
        } else {
            next.version
        };
        if next_v != prev_v + 1 {
            return Err(format!(
                "schema version {next_v} must be {} on {}",
                prev_v + 1,
                self.kind
            ));
        }
        let rename: HashMap<&str, &str> = next
            .migration
            .rename
            .iter()
            .map(|(old, new)| (old.as_str(), new.as_str()))
            .collect();
        if rename.len() != next.migration.rename.len() {
            return Err(format!("duplicate migrate_rename on {}", self.kind));
        }
        let mut seen_new = HashSet::new();
        for (_, new) in &next.migration.rename {
            if !seen_new.insert(new.as_str()) {
                return Err(format!(
                    "duplicate migrate_rename target {new} on {}",
                    self.kind
                ));
            }
        }
        let mut expected_drop = Vec::new();
        let mut expected_cast: Vec<(String, PropertyType)> = Vec::new();
        for name in &self.properties {
            let mapped = rename.get(name.as_str()).copied().unwrap_or(name.as_str());
            if !next.properties.iter().any(|prop| prop == mapped) {
                if rename.contains_key(name.as_str()) {
                    return Err(format!(
                        "rename {name} to {mapped} is not on {} properties",
                        self.kind
                    ));
                }
                expected_drop.push(name.clone());
                continue;
            }
            let Some(previous) = self.property_type(name) else {
                continue;
            };
            let Some(replacement) = next.property_type(mapped) else {
                continue;
            };
            if previous != replacement {
                expected_cast.push((mapped.to_string(), replacement));
            }
        }
        for (old, new) in &next.migration.rename {
            if old == new {
                return Err(format!("rename {old} is a no-op on {}", self.kind));
            }
            if !self.properties.iter().any(|prop| prop == old) {
                return Err(format!("rename source {old} is not on {}", self.kind));
            }
            if !next.properties.iter().any(|prop| prop == new) {
                return Err(format!("rename target {new} is not on {}", self.kind));
            }
            // Reading the previous version reverses the rename; a target that
            // version already declared could not be told apart from its own value.
            if self.properties.iter().any(|prop| prop == new) {
                return Err(format!(
                    "rename target {new} is already declared on {}",
                    self.kind
                ));
            }
            if next.properties.iter().any(|prop| prop == old) {
                return Err(format!(
                    "rename source {old} still declared on {}",
                    self.kind
                ));
            }
        }
        expected_drop.sort();
        let mut declared_drop = next.migration.drop.clone();
        declared_drop.sort();
        if declared_drop != expected_drop {
            if expected_drop.is_empty() {
                return Err(format!(
                    "migrate_drop is not a breaking change on {}",
                    self.kind
                ));
            }
            return Err(format!(
                "cannot drop {} on {} without migrate_drop",
                expected_drop.join(","),
                self.kind
            ));
        }
        expected_cast.sort_by(|a, b| a.0.cmp(&b.0));
        let mut declared_cast = next.migration.cast.clone();
        declared_cast.sort_by(|a, b| a.0.cmp(&b.0));
        if declared_cast != expected_cast {
            if expected_cast.is_empty() {
                return Err(format!(
                    "migrate_cast is not a breaking change on {}",
                    self.kind
                ));
            }
            let (name, ty) = &expected_cast[0];
            return Err(format!(
                "cannot recast {name} to {} on {}",
                ty.token(),
                self.kind
            ));
        }
        let mut expected_retarget = Vec::new();
        for link in self.links.iter().filter(|link| link.outgoing) {
            let mapped = rename
                .get(link.name.as_str())
                .copied()
                .unwrap_or(link.name.as_str());
            let Some(next_link) = next.links.iter().find(|candidate| candidate.name == mapped)
            else {
                continue;
            };
            if next_link.far_kind != link.far_kind || next_link.outgoing != link.outgoing {
                expected_retarget.push(next_link.clone());
            }
        }
        expected_retarget.sort_by(|a, b| a.name.cmp(&b.name));
        let mut declared_retarget = next.migration.retarget.clone();
        declared_retarget.sort_by(|a, b| a.name.cmp(&b.name));
        if declared_retarget != expected_retarget {
            if expected_retarget.is_empty() {
                return Err(format!(
                    "migrate_retarget is not a breaking change on {}",
                    self.kind
                ));
            }
            return Err(format!(
                "cannot retarget outgoing link {} on {}",
                expected_retarget[0].name, self.kind
            ));
        }
        let mut stamped = next.clone();
        stamped.version = next_v;
        stamped.check()?;
        Ok(stamped)
    }

    /// Overlay merge plan for this descriptor. Absent `conflict` is implicit
    /// user-wins and does not record an audit row.
    pub fn conflict_plan(&self) -> ConflictPlan {
        ConflictPlan {
            default: self.conflict.unwrap_or(ConflictStrategy::User),
            properties: self.conflict_properties.iter().cloned().collect(),
            time_property: self.conflict_time.clone(),
            record_audit: self.conflict.is_some() || !self.conflict_properties.is_empty(),
        }
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
        let mut indexes = HashSet::new();
        for name in &self.indexes {
            token(name, "index")?;
            if !properties.contains(name.as_str()) {
                return Err(format!("index property {name} is not declared"));
            }
            if !indexes.insert(name.as_str()) {
                return Err(format!("duplicate index property {name}"));
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
            if let PropertyType::Vector { dim } = ty {
                if !(1..=VECTOR_DIM_MAX).contains(dim) {
                    return Err(format!(
                        "vector dim {dim} on {name} is not in 1..={VECTOR_DIM_MAX}"
                    ));
                }
            }
        }
        for name in &self.sums {
            if let Some(ty) = self.property_type(name) {
                if !matches!(
                    ty,
                    PropertyType::String | PropertyType::Integer | PropertyType::Decimal { .. }
                ) {
                    return Err(format!("sum property {name} cannot be {}", ty.token()));
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
        let mut conflict_properties = HashSet::new();
        for (name, _) in &self.conflict_properties {
            token(name, "conflict property")?;
            if !properties.contains(name.as_str()) {
                return Err(format!("conflict property {name} is not declared"));
            }
            if !conflict_properties.insert(name.as_str()) {
                return Err(format!("duplicate conflict property {name}"));
            }
        }
        if let Some(name) = &self.conflict_time {
            token(name, "conflict_time")?;
            if !properties.contains(name.as_str()) {
                return Err(format!("conflict_time {name} is not declared"));
            }
            if self.property_type(name) != Some(PropertyType::Timestamp) {
                return Err(format!("conflict_time {name} must be a timestamp"));
            }
        }
        if self.conflict_plan().uses_newest() && self.conflict_time.is_none() {
            return Err("newest conflict requires conflict_time".into());
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

fn optional_token(record: &ObjectRecord, name: &str) -> Result<Option<String>, String> {
    match record.props.get(name) {
        None => Ok(None),
        Some(raw) if raw.is_empty() => Err(format!("empty schema {name}")),
        Some(raw) => Ok(Some(raw.clone())),
    }
}

fn optional_version(record: &ObjectRecord) -> Result<u32, String> {
    match record.props.get(SCHEMA_VERSION) {
        None => Ok(0),
        Some(raw) => {
            if raw.len() > 1 && raw.starts_with('0') {
                return Err(format!("schema version {raw} is not canonical"));
            }
            let version: u32 = raw
                .parse()
                .map_err(|_| format!("schema version {raw} is not an integer"))?;
            if version == 0 {
                return Err("schema version must be at least 1".into());
            }
            Ok(version)
        }
    }
}

fn parse_rename(entries: Vec<String>) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut seen_new = HashSet::new();
    for entry in entries {
        let Some((old, new)) = entry.split_once(':') else {
            return Err(format!("migrate_rename {entry} must be old:new"));
        };
        if old.is_empty() || new.is_empty() {
            return Err(format!("migrate_rename {entry} is empty"));
        }
        if !seen.insert(old.to_string()) {
            return Err(format!("duplicate migrate_rename {old}"));
        }
        if !seen_new.insert(new.to_string()) {
            return Err(format!("duplicate migrate_rename target {new}"));
        }
        out.push((old.to_string(), new.to_string()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn parse_retarget(entries: Vec<String>) -> Result<Vec<SchemaLink>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        let parts: Vec<&str> = entry.split(':').collect();
        if parts.len() != 3 {
            return Err(format!(
                "migrate_retarget {entry} must be name:far_kind:out or name:far_kind:in"
            ));
        }
        let (name, far_kind, dir) = (parts[0], parts[1], parts[2]);
        if name.is_empty() || far_kind.is_empty() {
            return Err(format!("migrate_retarget {entry} is empty"));
        }
        if !seen.insert(name.to_string()) {
            return Err(format!("duplicate migrate_retarget {name}"));
        }
        let outgoing = match dir {
            "out" => true,
            "in" => false,
            _ => {
                return Err(format!(
                    "migrate_retarget {entry} direction must be out or in"
                ))
            }
        };
        out.push(SchemaLink {
            name: name.to_string(),
            far_kind: far_kind.to_string(),
            outgoing,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn optional_conflict(record: &ObjectRecord) -> Result<Option<ConflictStrategy>, String> {
    match record.props.get(SCHEMA_CONFLICT) {
        None => Ok(None),
        Some(raw) => Ok(Some(ConflictStrategy::from_token(raw)?)),
    }
}

fn parse_conflict_properties(
    entries: Vec<String>,
) -> Result<Vec<(String, ConflictStrategy)>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        let Some((name, token)) = entry.split_once(':') else {
            return Err(format!(
                "conflict property {entry} must be name:user or name:newest"
            ));
        };
        if name.is_empty() || token.is_empty() {
            return Err(format!("conflict property {entry} is empty"));
        }
        if !seen.insert(name.to_string()) {
            return Err(format!("duplicate conflict property {name}"));
        }
        out.push((name.to_string(), ConflictStrategy::from_token(token)?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
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
            "type {entry} must be name:token (boolean|integer|timestamp|string|decimal:<scale>|vector:<dim>|tsref|geo:point|geo:shape|media)"
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
            ..Default::default()
        }
    }

    fn instance(props: &[(&str, &str)]) -> ObjectRecord {
        ObjectRecord {
            gen: 1,
            namespace: "ns".into(),
            kind: "incident".into(),
            key: "inc-1".into(),
            hidden: false,
            action_id: None,
            schema_version: 1,
            props: props
                .iter()
                .map(|(name, value)| ((*name).into(), (*value).into()))
                .collect(),
        }
    }

    #[test]
    fn record_roundtrip_preserves_rules() {
        let schema = descriptor();
        let record = schema.to_record("ns").unwrap();
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
        assert!(reserved.to_record("ns").unwrap_err().contains("reserved"));
        let mut record = descriptor().to_record("ns").unwrap();
        record.props.insert("note".into(), "x".into());
        let err = SchemaDescriptor::from_record(&record).unwrap_err();
        assert!(err.contains("unknown schema property"), "{err}");
    }

    #[test]
    fn sums_roundtrip_and_historical_rows_load() {
        let mut schema = descriptor();
        schema.properties = vec!["affects".into(), "amount".into(), "name".into()];
        schema.sums = vec!["amount".into()];
        let record = schema.to_record("ns").unwrap();
        assert_eq!(
            record.props.get(SCHEMA_SUMS).map(String::as_str),
            Some("amount")
        );
        assert_eq!(SchemaDescriptor::from_record(&record).unwrap(), schema);

        let mut historical = descriptor().to_record("ns").unwrap();
        historical.props.remove(SCHEMA_SUMS);
        let loaded = SchemaDescriptor::from_record(&historical).unwrap();
        assert!(loaded.sums.is_empty());

        schema.sums = vec!["missing".into()];
        let err = schema.to_record("ns").unwrap_err();
        assert!(
            err.contains("sum property missing is not declared"),
            "{err}"
        );
    }

    #[test]
    fn indexes_roundtrip_and_historical_rows_load() {
        let mut schema = descriptor();
        schema.indexes = vec!["name".into()];
        let record = schema.to_record("ns").unwrap();
        assert_eq!(
            record.props.get(SCHEMA_INDEXES).map(String::as_str),
            Some("name")
        );
        assert_eq!(SchemaDescriptor::from_record(&record).unwrap(), schema);

        let mut historical = descriptor().to_record("ns").unwrap();
        historical.props.remove(SCHEMA_INDEXES);
        let loaded = SchemaDescriptor::from_record(&historical).unwrap();
        assert!(loaded.indexes.is_empty());

        schema.indexes = vec!["missing".into()];
        let err = schema.to_record("ns").unwrap_err();
        assert!(
            err.contains("index property missing is not declared"),
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
        let record = schema.to_record("ns").unwrap();
        assert_eq!(
            record.props.get(SCHEMA_TYPES).map(String::as_str),
            Some("cost:decimal:2,open:boolean")
        );
        assert_eq!(SchemaDescriptor::from_record(&record).unwrap(), schema);

        let mut historical = descriptor().to_record("ns").unwrap();
        historical.props.remove(SCHEMA_TYPES);
        let loaded = SchemaDescriptor::from_record(&historical).unwrap();
        assert!(loaded.types.is_empty());

        schema.types = vec![("missing".into(), PropertyType::Integer)];
        let err = schema.to_record("ns").unwrap_err();
        assert!(
            err.contains("typed property missing is not declared"),
            "{err}"
        );
    }

    #[test]
    fn reference_types_roundtrip_and_cannot_be_sums_or_links() {
        let mut schema = descriptor();
        schema.properties = vec![
            "affects".into(),
            "embedding".into(),
            "file".into(),
            "location".into(),
            "name".into(),
            "region".into(),
            "series".into(),
        ];
        schema.types = vec![
            ("embedding".into(), PropertyType::Vector { dim: 2 }),
            ("file".into(), PropertyType::Media),
            ("location".into(), PropertyType::GeoPoint),
            ("region".into(), PropertyType::GeoShape),
            ("series".into(), PropertyType::TimeSeries),
        ];
        let record = schema.to_record("ns").unwrap();
        assert_eq!(
            record.props.get(SCHEMA_TYPES).map(String::as_str),
            Some("embedding:vector:2,file:media,location:geo:point,region:geo:shape,series:tsref")
        );
        assert_eq!(SchemaDescriptor::from_record(&record).unwrap(), schema);

        schema.sums = vec!["embedding".into()];
        let sum_err = schema.to_record("ns").unwrap_err();
        assert!(sum_err.contains("sum property embedding"), "{sum_err}");
        schema.sums.clear();
        schema.types.push(("affects".into(), PropertyType::Media));
        let link_err = schema.to_record("ns").unwrap_err();
        assert!(
            link_err.contains("outgoing link affects must be a string type"),
            "{link_err}"
        );
    }

    #[test]
    fn check_replacement_allows_additive_and_rejects_recast() {
        let previous = descriptor();
        let mut added = previous.clone();
        added.properties.push("note".into());
        added.required.push("note".into());
        added.types.push(("note".into(), PropertyType::String));
        added.sums.push("name".into());
        added.links.push(SchemaLink {
            name: "opened_by".into(),
            far_kind: "component".into(),
            outgoing: false,
        });
        previous.check_replacement(&added).unwrap();

        let mut optional = added.clone();
        optional.required.retain(|name| name != "note");
        added.check_replacement(&optional).unwrap();

        let mut shrunk = optional.clone();
        shrunk.properties.retain(|name| name != "note");
        shrunk.types.clear();
        let drop_err = optional.check_replacement(&shrunk).unwrap_err();
        assert!(drop_err.contains("cannot drop note"), "{drop_err}");
        shrunk.migration.drop = vec!["note".into()];
        optional.check_replacement(&shrunk).unwrap();

        previous.check_replacement(&previous).unwrap();

        let mut recast = previous.clone();
        recast.types = vec![("name".into(), PropertyType::Integer)];
        let recast_err = previous.check_replacement(&recast).unwrap_err();
        assert!(recast_err.contains("cannot recast name"), "{recast_err}");

        let mut decimal = previous.clone();
        decimal.properties.push("cost".into());
        decimal.types = vec![("cost".into(), PropertyType::Decimal { scale: 2 })];
        previous.check_replacement(&decimal).unwrap();
        let mut scaled = decimal.clone();
        scaled.types = vec![("cost".into(), PropertyType::Decimal { scale: 3 })];
        let scale_err = decimal.check_replacement(&scaled).unwrap_err();
        assert!(scale_err.contains("cannot recast cost"), "{scale_err}");

        let mut retarget = previous.clone();
        retarget.links[0].far_kind = "service".into();
        let link_err = previous.check_replacement(&retarget).unwrap_err();
        assert!(
            link_err.contains("cannot retarget outgoing link affects"),
            "{link_err}"
        );
        retarget.migration.retarget = vec![SchemaLink {
            name: "affects".into(),
            far_kind: "service".into(),
            outgoing: true,
        }];
        let stamped = previous.check_replacement(&retarget).unwrap();
        assert_eq!(stamped.version, 2);

        let mut renamed = previous.clone();
        renamed.properties = vec!["name".into(), "title".into()];
        renamed.links.clear();
        let rename_err = previous.check_replacement(&renamed).unwrap_err();
        assert!(rename_err.contains("cannot drop affects"), "{rename_err}");
        renamed.migration.drop = vec!["affects".into()];
        previous.check_replacement(&renamed).unwrap();
        renamed.migration.drop.clear();
        renamed.properties = vec!["affects".into(), "title".into()];
        renamed.required = vec!["title".into()];
        renamed.links = previous.links.clone();
        renamed.migration.rename = vec![("name".into(), "title".into())];
        let renamed = previous.check_replacement(&renamed).unwrap();
        assert_eq!(renamed.version, 2);

        recast.migration.cast = vec![("name".into(), PropertyType::Integer)];
        previous.check_replacement(&recast).unwrap();

        let mut collide = previous.clone();
        collide.properties = vec!["title".into()];
        collide.required = vec!["title".into()];
        collide.links.clear();
        collide.migration.rename = vec![
            ("affects".into(), "title".into()),
            ("name".into(), "title".into()),
        ];
        let collide_err = previous.check_replacement(&collide).unwrap_err();
        assert!(
            collide_err.contains("duplicate migrate_rename target title"),
            "{collide_err}"
        );

        let mut inbound = previous.clone();
        inbound.links[0].outgoing = false;
        let dir_err = previous.check_replacement(&inbound).unwrap_err();
        assert!(dir_err.contains("cannot retarget"), "{dir_err}");
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

    #[test]
    fn conflict_roundtrip_and_newest_requires_timestamp() {
        let mut schema = descriptor();
        schema.properties.push("status".into());
        schema.properties.push("updated_at".into());
        schema
            .types
            .push(("updated_at".into(), PropertyType::Timestamp));
        schema.conflict = Some(ConflictStrategy::User);
        schema.conflict_properties = vec![("status".into(), ConflictStrategy::Newest)];
        schema.conflict_time = Some("updated_at".into());
        let record = schema.to_record("ns").unwrap();
        assert_eq!(
            record.props.get(SCHEMA_CONFLICT).map(String::as_str),
            Some("user")
        );
        assert_eq!(
            record
                .props
                .get(SCHEMA_CONFLICT_PROPERTIES)
                .map(String::as_str),
            Some("status:newest")
        );
        assert_eq!(SchemaDescriptor::from_record(&record).unwrap(), schema);

        let mut missing_time = schema.clone();
        missing_time.conflict_time = None;
        let err = missing_time.to_record("ns").unwrap_err();
        assert!(err.contains("conflict_time"), "{err}");

        let mut not_ts = schema.clone();
        not_ts.conflict_time = Some("status".into());
        let err = not_ts.to_record("ns").unwrap_err();
        assert!(err.contains("must be a timestamp"), "{err}");

        schema.conflict = Some(ConflictStrategy::Newest);
        descriptor().check_replacement(&schema).unwrap();
    }
}
