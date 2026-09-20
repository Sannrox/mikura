use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclError {
    Denied { kind: String, property: String },
}

#[derive(Clone, Debug, Default)]
pub struct PropertyAcl {
    denied: HashSet<(String, String)>,
}

impl PropertyAcl {
    pub fn allow_all() -> Self {
        Self {
            denied: HashSet::new(),
        }
    }

    pub fn deny_property(kind: &str, property: &str) -> Self {
        let mut acl = Self::allow_all();
        acl.denied.insert((kind.into(), property.into()));
        acl
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

    pub(crate) fn denied_sorted(&self) -> Vec<(&str, &str)> {
        let mut denied: Vec<(&str, &str)> = self
            .denied
            .iter()
            .map(|(kind, property)| (kind.as_str(), property.as_str()))
            .collect();
        denied.sort_unstable();
        denied
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
}
