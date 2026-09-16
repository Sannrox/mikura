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

    /// Drop denied keys. Remaining values are stored ones, not substitutes.
    pub fn omit_denied(
        &self,
        kind: &str,
        mut props: HashMap<String, String>,
    ) -> HashMap<String, String> {
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
}
