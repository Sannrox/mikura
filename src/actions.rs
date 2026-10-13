use std::collections::HashMap;

use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct Action {
    /// Clerk-assigned opaque id. Empty fails closed on `Store::apply_action`.
    /// A repeated id with the same body is a replay (ADR 0011).
    pub id: String,
    pub namespace: String,
    pub kind: String,
    pub key: String,
    pub props: HashMap<String, String>,
}

/// Reserved kind of the marker that opens a committed [`ChangeSet`]. Its key
/// is the Action id; only `Store::apply_change_set` writes it (ADR 0033).
pub const CHANGE_SET_KIND: &str = "mikura.change_set";

/// Most edits one [`ChangeSet`] may carry.
pub const CHANGE_SET_BOUND: usize = 64;

/// Edits to several identities of one namespace under one clerk Action id,
/// committed all or nothing by `Store::apply_change_set` (ADR 0033).
#[derive(Clone, Debug)]
pub struct ChangeSet {
    pub id: String,
    pub namespace: String,
    pub edits: Vec<Edit>,
}

/// One member of a [`ChangeSet`]. `expected_gen` is checked per member on a
/// new Action id, as on `Store::apply_action`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "edit", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    /// Whole-record replace, as `Store::apply_action`.
    Put {
        kind: String,
        key: String,
        #[serde(default)]
        props: HashMap<String, String>,
        #[serde(default)]
        expected_gen: Option<u64>,
    },
    /// Hide a live identity, as `Store::hide`.
    Hide {
        kind: String,
        key: String,
        #[serde(default)]
        expected_gen: Option<u64>,
    },
}

impl Edit {
    pub fn kind(&self) -> &str {
        match self {
            Self::Put { kind, .. } | Self::Hide { kind, .. } => kind,
        }
    }

    pub fn key(&self) -> &str {
        match self {
            Self::Put { key, .. } | Self::Hide { key, .. } => key,
        }
    }

    pub fn expected_gen(&self) -> Option<u64> {
        match self {
            Self::Put { expected_gen, .. } | Self::Hide { expected_gen, .. } => *expected_gen,
        }
    }

    pub fn is_hide(&self) -> bool {
        matches!(self, Self::Hide { .. })
    }
}
