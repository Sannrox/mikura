use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

/// Bound on `hide_identities`. Exceeding it fails closed (no truncation).
pub const HIDE_IDENTITIES_BOUND: usize = 4096;
/// Bound on `hide_kinds`. Exceeding it fails closed (no truncation).
pub const HIDE_KINDS_BOUND: usize = 256;
/// Bound on kinds carrying a row filter. Exceeding it fails closed.
pub const ROW_FILTER_KINDS_BOUND: usize = 256;
/// Bound on conjunctive terms per kind. Exceeding it fails closed.
pub const ROW_FILTER_TERMS_BOUND: usize = 16;
/// Bound on allowed values per term. Exceeding it fails closed.
pub const ROW_FILTER_VALUES_BOUND: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclError {
    Denied {
        kind: String,
        property: String,
    },
    /// User-facing mutation of an identity that is not in this view.
    Invisible {
        kind: String,
        key: String,
    },
    Malformed(String),
}

impl fmt::Display for AclError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied { kind, property } => {
                write!(f, "Denied {{ kind: \"{kind}\", property: \"{property}\" }}")
            }
            Self::Invisible { kind, key } => write!(f, "not in this view {kind}/{key}"),
            Self::Malformed(msg) => f.write_str(msg),
        }
    }
}

/// Request-scoped restriction document (ADR 0014).
///
/// Property deny and object hide are separate axes. Never written to the
/// object log. Empty document is today's open clerk path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyAcl {
    denied: HashSet<(String, String)>,
    hide_kinds: HashSet<String>,
    hide_identities: HashSet<(String, String)>,
    /// `kind -> property -> allowed values` (ADR 0029). Terms of one kind
    /// are a conjunction; a row is visible only when every term's stored
    /// value is in its allowed set.
    row_filters: HashMap<String, BTreeMap<String, BTreeSet<String>>>,
}

impl PropertyAcl {
    pub fn allow_all() -> Self {
        Self::default()
    }

    pub fn deny_property(kind: &str, property: &str) -> Self {
        let mut acl = Self::allow_all();
        acl.denied.insert((kind.into(), property.into()));
        acl
    }

    pub fn insert_deny(&mut self, kind: &str, property: &str) -> Result<(), AclError> {
        require_token(kind, "deny kind")?;
        require_token(property, "deny property")?;
        if self.filters_on(kind, property) {
            return Err(row_filter_on_denied(kind, property));
        }
        let pair = (kind.to_string(), property.to_string());
        if !self.denied.insert(pair) {
            return Err(AclError::Malformed(format!(
                "duplicate deny {kind}.{property}"
            )));
        }
        Ok(())
    }

    pub fn insert_hide_kind(&mut self, kind: &str) -> Result<(), AclError> {
        require_token(kind, "hide kind")?;
        if self.hide_kinds.len() >= HIDE_KINDS_BOUND && !self.hide_kinds.contains(kind) {
            return Err(AclError::Malformed(format!(
                "hide_kinds exceeds {HIDE_KINDS_BOUND}"
            )));
        }
        if !self.hide_kinds.insert(kind.to_string()) {
            return Err(AclError::Malformed(format!("duplicate hide kind {kind}")));
        }
        Ok(())
    }

    pub fn insert_hide_identity(&mut self, kind: &str, key: &str) -> Result<(), AclError> {
        require_token(kind, "hide identity kind")?;
        require_token(key, "hide identity key")?;
        let pair = (kind.to_string(), key.to_string());
        if self.hide_identities.len() >= HIDE_IDENTITIES_BOUND
            && !self.hide_identities.contains(&pair)
        {
            return Err(AclError::Malformed(format!(
                "hide_identities exceeds {HIDE_IDENTITIES_BOUND}"
            )));
        }
        if !self.hide_identities.insert(pair) {
            return Err(AclError::Malformed(format!(
                "duplicate hide identity {kind}/{key}"
            )));
        }
        Ok(())
    }

    pub fn check(&self, kind: &str, property: &str) -> Result<(), AclError> {
        if self.denied.contains(&(kind.into(), property.into())) {
            return Err(AclError::Denied {
                kind: kind.into(),
                property: property.into(),
            });
        }
        Ok(())
    }

    /// Add one row-filter term on `kind`: the stored `property` must equal
    /// one of `values` exactly (ADR 0029). Terms on one kind are a
    /// conjunction. Empty tokens, an empty or duplicate value, a repeated
    /// `(kind, property)`, a denied property, and overflow fail closed.
    pub fn insert_row_filter<V: AsRef<str>>(
        &mut self,
        kind: &str,
        property: &str,
        values: impl IntoIterator<Item = V>,
    ) -> Result<(), AclError> {
        require_token(kind, "row filter kind")?;
        require_token(property, "row filter property")?;
        if self.check(kind, property).is_err() {
            return Err(row_filter_on_denied(kind, property));
        }
        let mut allowed = BTreeSet::new();
        for value in values {
            let value = value.as_ref();
            require_token(value, "row filter value")?;
            if allowed.len() >= ROW_FILTER_VALUES_BOUND {
                return Err(AclError::Malformed(format!(
                    "row filter {kind}.{property} exceeds {ROW_FILTER_VALUES_BOUND} values"
                )));
            }
            if !allowed.insert(value.to_string()) {
                return Err(AclError::Malformed(format!(
                    "duplicate row filter value {kind}.{property}={value}"
                )));
            }
        }
        if allowed.is_empty() {
            return Err(AclError::Malformed(format!(
                "row filter {kind}.{property} requires at least one value"
            )));
        }
        if !self.row_filters.contains_key(kind) && self.row_filters.len() >= ROW_FILTER_KINDS_BOUND
        {
            return Err(AclError::Malformed(format!(
                "row filters exceed {ROW_FILTER_KINDS_BOUND} kinds"
            )));
        }
        let terms = self.row_filters.entry(kind.to_string()).or_default();
        if terms.contains_key(property) {
            return Err(AclError::Malformed(format!(
                "duplicate row filter {kind}.{property}"
            )));
        }
        if terms.len() >= ROW_FILTER_TERMS_BOUND {
            return Err(AclError::Malformed(format!(
                "row filter on {kind} exceeds {ROW_FILTER_TERMS_BOUND} terms"
            )));
        }
        terms.insert(property.to_string(), allowed);
        Ok(())
    }

    fn filters_on(&self, kind: &str, property: &str) -> bool {
        self.row_filters
            .get(kind)
            .is_some_and(|terms| terms.contains_key(property))
    }

    /// Whether this identity exists in the request view.
    ///
    /// `prop` returns the identity's stored property value. It is consulted
    /// only for kinds with a row filter; a missing value is invisible.
    /// Restriction-invisible is not a persisted tombstone.
    pub fn object_visible<'a>(
        &self,
        kind: &str,
        key: &str,
        prop: impl Fn(&str) -> Option<&'a str>,
    ) -> bool {
        if self.hide_kinds.contains(kind)
            || self.hide_identities.contains(&(kind.into(), key.into()))
        {
            return false;
        }
        self.row_filters.get(kind).is_none_or(|terms| {
            terms
                .iter()
                .all(|(property, allowed)| prop(property).is_some_and(|v| allowed.contains(v)))
        })
    }

    pub fn require_visible<'a>(
        &self,
        kind: &str,
        key: &str,
        prop: impl Fn(&str) -> Option<&'a str>,
    ) -> Result<(), AclError> {
        if self.object_visible(kind, key, prop) {
            Ok(())
        } else {
            Err(AclError::Invisible {
                kind: kind.into(),
                key: key.into(),
            })
        }
    }

    pub fn hides_objects(&self) -> bool {
        !self.hide_kinds.is_empty()
            || !self.hide_identities.is_empty()
            || !self.row_filters.is_empty()
    }

    /// Whether any identity of `kind` can be invisible in this view.
    pub(crate) fn hides_in_kind(&self, kind: &str) -> bool {
        self.hide_kinds.contains(kind)
            || self.row_filters.contains_key(kind)
            || self.hide_identities.iter().any(|(k, _)| k == kind)
    }

    pub(crate) fn denied_sorted(&self) -> Vec<(&str, &str)> {
        let mut denied: Vec<(&str, &str)> = self
            .denied
            .iter()
            .map(|(kind, property)| (kind.as_str(), property.as_str()))
            .collect();
        denied.sort_unstable();
        denied
    }

    pub(crate) fn hide_kinds_sorted(&self) -> Vec<&str> {
        let mut kinds: Vec<&str> = self.hide_kinds.iter().map(String::as_str).collect();
        kinds.sort_unstable();
        kinds
    }

    pub(crate) fn hide_identities_sorted(&self) -> Vec<(&str, &str)> {
        let mut ids: Vec<(&str, &str)> = self
            .hide_identities
            .iter()
            .map(|(kind, key)| (kind.as_str(), key.as_str()))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// `(kind, property, sorted values)` in a stable order for cursor binding.
    pub(crate) fn row_filters_sorted(&self) -> Vec<(&str, &str, &BTreeSet<String>)> {
        let mut terms: Vec<_> = self
            .row_filters
            .iter()
            .flat_map(|(kind, terms)| {
                terms
                    .iter()
                    .map(move |(property, values)| (kind.as_str(), property.as_str(), values))
            })
            .collect();
        terms.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        terms
    }

    /// Resolve named denies to intern ids. Unknown names cannot appear on a row.
    pub(crate) fn interned_denies(
        &self,
        intern_lookup: impl Fn(&str) -> Option<u32>,
    ) -> HashSet<(u32, u32)> {
        if self.denied.is_empty() {
            return HashSet::new();
        }
        self.denied
            .iter()
            .filter_map(|(kind, property)| Some((intern_lookup(kind)?, intern_lookup(property)?)))
            .collect()
    }

    /// Borrow interned `(property, value)` pairs, skipping denied
    /// `(kind_id, prop_id)`. Callers that need an owned map collect
    /// [`Self::omit_owned`].
    pub(crate) fn iter_owned<'a>(
        intern: &'a [String],
        kind_id: u32,
        owned: &'a [(u32, u32)],
        denied: &'a HashSet<(u32, u32)>,
    ) -> impl Iterator<Item = (&'a str, &'a str)> + 'a {
        let filter = !denied.is_empty();
        owned.iter().filter_map(move |&(prop_id, value_id)| {
            if filter && denied.contains(&(kind_id, prop_id)) {
                return None;
            }
            Some((
                intern.get(prop_id as usize)?.as_str(),
                intern.get(value_id as usize)?.as_str(),
            ))
        })
    }

    /// Materialize interned property pairs, cloning only allowed keys.
    pub(crate) fn omit_owned(
        intern: &[String],
        kind_id: u32,
        owned: &[(u32, u32)],
        denied: &HashSet<(u32, u32)>,
    ) -> HashMap<String, String> {
        let mut props = HashMap::with_capacity(owned.len());
        for (prop, value) in Self::iter_owned(intern, kind_id, owned, denied) {
            props.insert(prop.to_string(), value.to_string());
        }
        props
    }

    /// Drop denied keys. Remaining values are stored ones, not substitutes.
    pub fn omit_denied(
        &self,
        kind: &str,
        mut props: HashMap<String, String>,
    ) -> HashMap<String, String> {
        if self.denied.is_empty() {
            return props;
        }
        props.retain(|property, _| self.check(kind, property).is_ok());
        props
    }
}

fn row_filter_on_denied(kind: &str, property: &str) -> AclError {
    AclError::Malformed(format!("row filter on denied property {kind}.{property}"))
}

fn require_token(token: &str, what: &str) -> Result<(), AclError> {
    if token.is_empty() {
        return Err(AclError::Malformed(format!("{what} must be non-empty")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omit_denied_drops_only_the_denied_key() {
        let mut props = HashMap::new();
        props.insert("order_id".into(), "o1".into());
        props.insert("amount".into(), "10".into());
        let open = PropertyAcl::allow_all().omit_denied("Shipment", props.clone());
        assert_eq!(open.get("amount").map(String::as_str), Some("10"));
        assert_eq!(open.get("order_id").map(String::as_str), Some("o1"));
        let redacted =
            PropertyAcl::deny_property("Shipment", "amount").omit_denied("Shipment", props);
        assert!(!redacted.contains_key("amount"));
        assert_eq!(redacted.get("order_id").map(String::as_str), Some("o1"));
        assert!(!redacted.contains_key("guessed"));
    }

    #[test]
    fn interned_denies_are_kind_and_property_ids() {
        let acl = PropertyAcl::deny_property("Shipment", "amount");
        let denied = acl.interned_denies(|token| match token {
            "Shipment" => Some(3),
            "amount" => Some(7),
            _ => None,
        });
        assert_eq!(denied, HashSet::from([(3, 7)]));
        assert!(PropertyAcl::allow_all()
            .interned_denies(|_| Some(1))
            .is_empty());
        assert!(acl.interned_denies(|_| None).is_empty());
    }

    #[test]
    fn omit_owned_clones_only_allowed_pairs() {
        let intern = [
            "Shipment".into(),
            "amount".into(),
            "10".into(),
            "order_id".into(),
            "o1".into(),
        ];
        let owned = [(1, 2), (3, 4)];
        let open = PropertyAcl::omit_owned(&intern, 0, &owned, &HashSet::new());
        assert_eq!(open.get("amount").map(String::as_str), Some("10"));
        assert_eq!(open.get("order_id").map(String::as_str), Some("o1"));
        let denied = HashSet::from([(0, 1)]);
        let redacted = PropertyAcl::omit_owned(&intern, 0, &owned, &denied);
        assert!(!redacted.contains_key("amount"));
        assert_eq!(redacted.get("order_id").map(String::as_str), Some("o1"));
    }

    #[test]
    fn iter_owned_borrows_intern_and_skips_denied() {
        let intern = [
            "Shipment".into(),
            "amount".into(),
            "10".into(),
            "order_id".into(),
            "o1".into(),
        ];
        let owned = [(1, 2), (3, 4)];
        let denied = HashSet::from([(0, 1)]);
        let pairs: Vec<_> = PropertyAcl::iter_owned(&intern, 0, &owned, &denied).collect();
        assert_eq!(pairs, vec![("order_id", "o1")]);
        assert!(std::ptr::eq(pairs[0].0, intern[3].as_str()));
        assert!(std::ptr::eq(pairs[0].1, intern[4].as_str()));
        let none = HashSet::new();
        let open: Vec<_> = PropertyAcl::iter_owned(&intern, 0, &owned, &none).collect();
        assert_eq!(open, vec![("amount", "10"), ("order_id", "o1")]);
        assert!(std::ptr::eq(open[0].0, intern[1].as_str()));
        assert!(std::ptr::eq(open[0].1, intern[2].as_str()));
    }

    #[test]
    fn hide_identity_matches_missing_and_mutations_are_invisible() {
        let mut acl = PropertyAcl::allow_all();
        acl.insert_hide_identity("incident", "inc-1").unwrap();
        assert!(!acl.object_visible("incident", "inc-1", |_| None));
        assert!(acl.object_visible("incident", "inc-2", |_| None));
        assert!(matches!(
            acl.require_visible("incident", "inc-1", |_| None),
            Err(AclError::Invisible { .. })
        ));
        let dup = acl.insert_hide_identity("incident", "inc-1").unwrap_err();
        assert!(dup.to_string().contains("duplicate"));
        let empty = PropertyAcl::allow_all().insert_hide_kind("").unwrap_err();
        assert!(empty.to_string().contains("non-empty"));
    }

    #[test]
    fn hide_identities_bound_fails_closed() {
        let mut acl = PropertyAcl::allow_all();
        for i in 0..HIDE_IDENTITIES_BOUND {
            acl.insert_hide_identity("k", &format!("id{i}")).unwrap();
        }
        let err = acl.insert_hide_identity("k", "overflow").unwrap_err();
        assert!(err.to_string().contains("exceeds"));
    }
}
