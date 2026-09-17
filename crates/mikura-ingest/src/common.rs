use mikura::ObjectRecord;
use std::collections::HashMap;

pub(crate) fn identity(record: &ObjectRecord) -> (String, String) {
    (record.kind.clone(), record.key.clone())
}

pub(crate) fn fold_snapshot(records: Vec<ObjectRecord>) -> HashMap<(String, String), ObjectRecord> {
    let mut chosen = HashMap::new();
    for record in records {
        chosen.insert(identity(&record), record);
    }
    chosen
}

pub(crate) fn source_payload_eq(left: &ObjectRecord, right: &ObjectRecord) -> bool {
    left.hidden == right.hidden && left.action_id == right.action_id && left.props == right.props
}
