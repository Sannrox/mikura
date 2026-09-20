use std::collections::{HashMap, HashSet};
use std::fmt;

/// Bound on `hide_identities`. Exceeding it fails closed (no truncation).
pub const HIDE_IDENTITIES_BOUND: usize = 4096;
/// Bound on `hide_kinds`. Exceeding it fails closed (no truncation).
pub const HIDE_KINDS_BOUND: usize = 256;

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

    /// Whether this identity exists in the request view.
    ///
    /// Restriction-invisible is not a persisted tombstone.
    pub fn object_visible(&self, kind: &str, key: &str) -> bool {
        if self.hide_kinds.contains(kind) {
            return false;
        }
        !self.hide_identities.contains(&(kind.into(), key.into()))
    }

    pub fn require_visible(&self, kind: &str, key: &str) -> Result<(), AclError> {
        if self.object_visible(kind, key) {
            Ok(())
        } else {
            Err(AclError::Invisible {
                kind: kind.into(),
                key: key.into(),
            })
        }
    }

    pub fn hides_objects(&self) -> bool {
        !self.hide_kinds.is_empty() || !self.hide_identities.is_empty()
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

    /// Materialize interned property pairs, cloning only allowed keys.
    pub(crate) fn omit_owned(
        intern: &[String],
        kind_id: u32,
        owned: &[(u32, u32)],
        denied: &HashSet<(u32, u32)>,
    ) -> HashMap<String, String> {
        let mut props = HashMap::with_capacity(owned.len());
        for &(prop_id, value_id) in owned {
            if !denied.is_empty() && denied.contains(&(kind_id, prop_id)) {
                continue;
            }
            let Some(prop) = intern.get(prop_id as usize) else {
                continue;
            };
            let Some(value) = intern.get(value_id as usize) else {
                continue;
            };
            props.insert(prop.clone(), value.clone());
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
    fn hide_identity_matches_missing_and_mutations_are_invisible() {
        let mut acl = PropertyAcl::allow_all();
        acl.insert_hide_identity("incident", "inc-1").unwrap();
        assert!(!acl.object_visible("incident", "inc-1"));
        assert!(acl.object_visible("incident", "inc-2"));
        assert!(matches!(
            acl.require_visible("incident", "inc-1"),
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
