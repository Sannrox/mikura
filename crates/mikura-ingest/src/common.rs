use mikura::ObjectRecord;
use std::collections::HashMap;

pub(crate) type Identity = (String, String);
pub(crate) type Snapshot = HashMap<Identity, ObjectRecord>;

pub(crate) fn identity(record: &ObjectRecord) -> Identity {
    (record.kind.clone(), record.key.clone())
}

pub(crate) fn intern_token_budget(record: &ObjectRecord) -> usize {
    2 + record.props.len().saturating_mul(2)
}

pub(crate) fn fold_last_wins(
    records: impl IntoIterator<Item = ObjectRecord>,
) -> (Vec<Identity>, Snapshot) {
    let mut order = Vec::new();
    let mut chosen = HashMap::new();
    for record in records {
        let id = identity(&record);
        if !chosen.contains_key(&id) {
            order.push(id.clone());
        }
        chosen.insert(id, record);
    }
    (order, chosen)
}

pub(crate) fn fold_snapshot(records: Vec<ObjectRecord>) -> Snapshot {
    fold_last_wins(records).1
}

pub(crate) fn source_payload_eq(left: &ObjectRecord, right: &ObjectRecord) -> bool {
    left.hidden == right.hidden && left.action_id == right.action_id && left.props == right.props
}
