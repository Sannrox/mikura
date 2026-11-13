use super::*;
use crate::{BatchIngest, StreamIngest};
use mikura::{ComputeError, LocalCompute};

#[test]
fn stream_flush_appends_pending_records() {
    let (dir, log) = temp_log("stream");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        )
        .unwrap();
    let stamp = store.snapshot_stamp();
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.sum_amount, 10);
    assert_eq!(store.snapshot_stamp(), stamp);
    stream.flush(&mut store).unwrap();
    assert_ne!(store.snapshot_stamp(), stamp);
    let streamed = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(streamed.sum_amount, 17);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_bound_n_plus_one_fails_closed_without_drop() {
    let (dir, log) = temp_log("stream-bound");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(2).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "1")],
            ),
        )
        .unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s4",
                false,
                &[("order_id", "o1"), ("amount", "2")],
            ),
        )
        .unwrap();
    let overflow = rec(
        "Shipment",
        "s5",
        false,
        &[("order_id", "o1"), ("amount", "99")],
    );
    let err = stream.push(&mut store, overflow).unwrap_err();
    assert!(
        err.contains("bound 2 exceeded"),
        "expected fail-closed bound error, got {err}"
    );
    let oss = ObjectSet::new(LocalCompute);
    let live = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(live.sum_amount, 10);
    stream.flush(&mut store).unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    let committed = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(committed.sum_amount, 13);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_unflushed_records_are_not_on_rebuild() {
    let (dir, log) = temp_log("stream-unflushed");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(4).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        )
        .unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let from_log = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_log.sum_amount, 10);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_flush_survives_reopen() {
    let (dir, log) = temp_log("stream-flush-reopen");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(4).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s3",
                false,
                &[("order_id", "o1"), ("amount", "7")],
            ),
        )
        .unwrap();
    stream.flush(&mut store).unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let from_log = oss.evaluate(&reopened, &fixture_request()).unwrap();
    assert_eq!(from_log.sum_amount, 17);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_hidden_records_excluded_and_acl_fails_closed() {
    let (dir, log) = temp_log("stream-hidden-acl");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut stream = StreamIngest::new(4).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Shipment",
                "s_hidden",
                true,
                &[("order_id", "o1"), ("amount", "50")],
            ),
        )
        .unwrap();
    stream.flush(&mut store).unwrap();
    let oss = ObjectSet::new(LocalCompute);
    let visible = oss.evaluate(&store, &fixture_request()).unwrap();
    assert_eq!(visible.sum_amount, 10);
    let mut denied = fixture_request();
    denied.acl = PropertyAcl::deny_property("Shipment", "amount");
    let err = oss.evaluate(&store, &denied).unwrap_err();
    assert!(matches!(
        err,
        ComputeError::Acl(mikura::AclError::Denied { .. })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_rejects_invalid_instance() {
    let (dir, log) = temp_log("stream-pending-schema");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    let err = stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap_err();
    assert!(
        err.contains("missing required property name"),
        "expected pending schema to reject the instance, got {err}"
    );
    assert_eq!(stream.uncommitted(), 1);
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("name", "ok")]))
        .unwrap();
    assert!(store
        .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
        .is_err());
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("ok")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_replacement_admits_next_version() {
    let (dir, log) = temp_log("stream-pending-schema-v2");
    let mut store = Store::create(&log).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    BatchIngest::run(&mut store, vec![v1]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "title".into()],
        required: vec!["name".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream
        .push(
            &mut store,
            rec("Widget", "w1", false, &[("name", "ok"), ("title", "hi")]),
        )
        .unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("title")
            .map(String::as_str),
        Some("hi")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_unspecified_version_keeps_chain() {
    let (dir, log) = temp_log("stream-pending-schema-chain");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            required: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let unspecified = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "title".into()],
        required: vec!["name".into()],
        version: 0,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, unspecified).unwrap();
    let v3 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "title".into(), "color".into()],
        required: vec!["name".into()],
        version: 3,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v3).unwrap();
    stream
        .push(
            &mut store,
            rec(
                "Widget",
                "w1",
                false,
                &[("name", "ok"), ("title", "hi"), ("color", "red")],
            ),
        )
        .unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("color")
            .map(String::as_str),
        Some("red")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_overlay_uses_committed_newest_plan() {
    let (dir, log) = temp_log("stream-pending-newest");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into(), "updated_at".into()],
            required: vec!["name".into()],
            types: vec![("updated_at".into(), mikura::PropertyType::Timestamp)],
            conflict: Some(mikura::ConflictStrategy::Newest),
            conflict_time: Some("updated_at".into()),
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        at: Some("2026-10-11T10:00:00.000Z".into()),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, overlay).unwrap();
    let err = stream
        .push(
            &mut store,
            rec("Widget", "w1", false, &[("name", "source")]),
        )
        .unwrap_err();
    assert!(
        err.contains("newest") || err.contains("timestamp"),
        "expected pending overlay to use committed newest plan, got {err}"
    );
    assert_eq!(stream.uncommitted(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_overlay_hide_rejects_instance_missing_required() {
    let (dir, log) = temp_log("stream-pending-overlay-hide");
    let mut store = Store::create(&log).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    BatchIngest::run(&mut store, vec![schema, overlay.to_record().unwrap()]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let mut hide = overlay.to_record().unwrap();
    hide.hidden = true;
    stream.push(&mut store, hide).unwrap();
    let err = stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap_err();
    assert!(
        err.contains("missing required property name"),
        "expected overlay hide to drop the committed fill, got {err}"
    );
    assert_eq!(stream.uncommitted(), 1);
    stream.flush(&mut store).unwrap();
    assert!(store
        .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_overlay_fills_required_on_later_push() {
    let (dir, log) = temp_log("stream-pending-overlay");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    stream.push(&mut store, overlay).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap();
    assert!(store
        .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
        .is_err());
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("from-overlay")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_action_id_is_per_namespace() {
    let (dir, log) = temp_log("stream-action-ns");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let mut first = rec(
        "Shipment",
        "s3",
        false,
        &[("order_id", "o1"), ("amount", "1")],
    );
    first.namespace = "ns-a".into();
    first.action_id = Some("act-1".into());
    let mut second = rec(
        "Shipment",
        "s3",
        false,
        &[("order_id", "o1"), ("amount", "2")],
    );
    second.namespace = "ns-b".into();
    second.action_id = Some("act-1".into());
    stream.push(&mut store, first).unwrap();
    stream.push(&mut store, second).unwrap();
    assert_eq!(stream.uncommitted(), 2);
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns-a", "Shipment", "s3", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("amount")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        store
            .load("ns-b", "Shipment", "s3", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("amount")
            .map(String::as_str),
        Some("2")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_action_id_replay_is_noop() {
    let (dir, log) = temp_log("stream-schema-action-replay");
    let mut store = Store::create(&log).unwrap();
    let mut first = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    first.action_id = Some("act-schema-1".into());
    BatchIngest::run(&mut store, vec![first.clone()]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, first).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .effective_version(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_committed_overlay_fills_required_under_pending_schema() {
    let (dir, log) = temp_log("stream-committed-overlay-pending-schema");
    let mut store = Store::create(&log).unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    BatchIngest::run(&mut store, vec![overlay]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap();
    assert!(store
        .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
        .is_err());
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("from-overlay")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_recast_rejects_live_string() {
    let (dir, log) = temp_log("stream-pending-schema-recast");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![rec("Widget", "w1", false, &[("count", "abc")])],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v1).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = stream.push(&mut store, v2).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected buffered recast to preflight live data, got {err}"
    );
    assert_eq!(stream.uncommitted(), 1);
    stream.flush(&mut store).unwrap();
    assert!(store
        .schema("ns", "Widget")
        .unwrap()
        .unwrap()
        .types
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_recast_rejects_buffered_instance() {
    let (dir, log) = temp_log("stream-pending-schema-recast-buf");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v1).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("count", "abc")]))
        .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = stream.push(&mut store, v2).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected buffered recast to preflight pending data, got {err}"
    );
    assert_eq!(stream.uncommitted(), 2);
    stream.flush(&mut store).unwrap();
    assert!(store
        .schema("ns", "Widget")
        .unwrap()
        .unwrap()
        .types
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_hide_admits_untyped_instance() {
    let (dir, log) = temp_log("stream-pending-schema-hide");
    let mut store = Store::create(&log).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    BatchIngest::run(&mut store, vec![schema.clone()]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let mut hide = schema;
    hide.hidden = true;
    stream.push(&mut store, hide).unwrap();
    stream
        .push(
            &mut store,
            rec("Widget", "w1", false, &[("alias", "unvalidated")]),
        )
        .unwrap();
    assert!(store
        .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
        .is_err());
    stream.flush(&mut store).unwrap();
    assert!(store.schema("ns", "Widget").unwrap().is_none());
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("alias")
            .map(String::as_str),
        Some("unvalidated")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_duplicate_version_fails_closed() {
    let (dir, log) = temp_log("stream-pending-schema-dup-v");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v1.clone()).unwrap();
    let err = stream.push(&mut store, v1).unwrap_err();
    assert!(
        err.contains("must be 2"),
        "expected duplicate schema version to fail at push, got {err}"
    );
    assert_eq!(stream.uncommitted(), 1);
    stream.flush(&mut store).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_action_id_retry_keeps_unspecified_version() {
    let (dir, log) = temp_log("stream-schema-action-retry-v0");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let mut first = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 0,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    first.action_id = Some("act-schema-1".into());
    stream.push(&mut store, first.clone()).unwrap();
    stream.push(&mut store, first).unwrap();
    assert_eq!(stream.uncommitted(), 1);
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .effective_version(),
        1
    );
    let mut replace = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 0,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    replace.action_id = Some("act-schema-2".into());
    stream.push(&mut store, replace.clone()).unwrap();
    stream.push(&mut store, replace).unwrap();
    assert_eq!(stream.uncommitted(), 1);
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .effective_version(),
        2
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_overlay_follows_rename_schema() {
    let (dir, log) = temp_log("stream-pending-overlay-rename");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            required: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, overlay).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["title".into()],
        required: vec!["title".into()],
        version: 2,
        migration: mikura::SchemaMigration {
            rename: vec![("name".into(), "title".into())],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("title")
            .map(String::as_str),
        Some("from-overlay")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_overlay_cleared_rename_admits_schema() {
    let (dir, log) = temp_log("stream-pending-overlay-cleared-rename");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            required: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        cleared: vec!["name".into()],
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, overlay).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["title".into()],
        version: 2,
        migration: mikura::SchemaMigration {
            rename: vec![("name".into(), "title".into())],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store.schema("ns", "Widget").unwrap().unwrap().properties,
        vec!["title".to_string()]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_action_id_hide_copies_claimed_body() {
    let (dir, log) = temp_log("stream-pending-action-hide");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let mut visible = rec(
        "Shipment",
        "s3",
        false,
        &[("order_id", "o1"), ("amount", "1")],
    );
    visible.action_id = Some("act-1".into());
    let mut hide = visible.clone();
    hide.hidden = true;
    stream.push(&mut store, visible).unwrap();
    stream.push(&mut store, hide).unwrap();
    assert_eq!(stream.uncommitted(), 2);
    stream.flush(&mut store).unwrap();
    assert!(
        store
            .load("ns", "Shipment", "s3", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_replacement_after_hide_continues_version() {
    let (dir, log) = temp_log("stream-schema-hide-v2");
    let mut store = Store::create(&log).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    BatchIngest::run(&mut store, vec![v1.clone()]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let mut hide = v1;
    hide.hidden = true;
    stream.push(&mut store, hide).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "title".into()],
        required: vec!["name".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .effective_version(),
        2
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_schema_replacement_after_committed_hide_continues_version() {
    let (dir, log) = temp_log("stream-schema-committed-hide-v2");
    let mut store = Store::create(&log).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut hide = v1.clone();
    hide.hidden = true;
    BatchIngest::run(&mut store, vec![v1, hide]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "title".into()],
        required: vec!["name".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .effective_version(),
        2
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_overlay_chained_renames() {
    let (dir, log) = temp_log("stream-pending-overlay-chain-rename");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            required: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, overlay).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["title".into()],
        required: vec!["title".into()],
        version: 2,
        migration: mikura::SchemaMigration {
            rename: vec![("name".into(), "title".into())],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    let v3 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["label".into()],
        required: vec!["label".into()],
        version: 3,
        migration: mikura::SchemaMigration {
            rename: vec![("title".into(), "label".into())],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v3).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("label")
            .map(String::as_str),
        Some("from-overlay")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_committed_overlay_chained_renames() {
    let (dir, log) = temp_log("stream-committed-overlay-chain-rename");
    let mut store = Store::create(&log).unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into()],
                required: vec!["name".into()],
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            overlay,
        ],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["title".into()],
        required: vec!["title".into()],
        version: 2,
        migration: mikura::SchemaMigration {
            rename: vec![("name".into(), "title".into())],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    let v3 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["label".into()],
        required: vec!["label".into()],
        version: 3,
        migration: mikura::SchemaMigration {
            rename: vec![("title".into(), "label".into())],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v3).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("label")
            .map(String::as_str),
        Some("from-overlay")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_instance_hide_tombstones_pending_overlay() {
    let (dir, log) = temp_log("stream-instance-hide-overlay");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    stream.push(&mut store, overlay).unwrap();
    stream
        .push(
            &mut store,
            rec("Widget", "w1", true, &[("name", "from-overlay")]),
        )
        .unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("name", "ok")]))
        .unwrap();
    let err = stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap_err();
    assert!(
        err.contains("missing required property name"),
        "expected instance hide to drop the overlay fill, got {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_action_hide_uses_merged_claimed_body() {
    let (dir, log) = temp_log("stream-action-hide-merged");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    stream.push(&mut store, overlay).unwrap();
    let mut visible = rec("Widget", "w1", false, &[]);
    visible.action_id = Some("act-1".into());
    stream.push(&mut store, visible.clone()).unwrap();
    let mut hide = visible;
    hide.hidden = true;
    let err = stream.push(&mut store, hide.clone()).unwrap_err();
    assert!(
        err.contains("body conflict"),
        "expected hide to compare the merged claimed body, got {err}"
    );
    hide.props.insert("name".into(), "from-overlay".into());
    stream.push(&mut store, hide).unwrap();
    stream.flush(&mut store).unwrap();
    assert!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_instance_replacement_allows_recast() {
    let (dir, log) = temp_log("stream-pending-instance-replace-recast");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["count".into()],
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            rec("Widget", "w1", false, &[("count", "abc")]),
        ],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("count", "1")]))
        .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_overlay_hide_skips_drop_migration() {
    let (dir, log) = temp_log("stream-overlay-hide-drop");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    stream.push(&mut store, schema).unwrap();
    stream
        .push(&mut store, overlay.to_record().unwrap())
        .unwrap();
    let mut hide = overlay.to_record().unwrap();
    hide.hidden = true;
    stream.push(&mut store, hide).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![],
        version: 2,
        migration: mikura::SchemaMigration {
            drop: vec!["name".into()],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert!(
        store
            .load(
                "ns",
                mikura::OVERLAY_KIND,
                "Widget/w1",
                &PropertyAcl::allow_all()
            )
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_instance_hide_skips_overlay_drop_migration() {
    let (dir, log) = temp_log("stream-instance-hide-drop");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    stream.push(&mut store, overlay).unwrap();
    stream
        .push(
            &mut store,
            rec("Widget", "w1", true, &[("name", "from-overlay")]),
        )
        .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![],
        version: 2,
        migration: mikura::SchemaMigration {
            drop: vec!["name".into()],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert!(
        store
            .load(
                "ns",
                mikura::OVERLAY_KIND,
                "Widget/w1",
                &PropertyAcl::allow_all()
            )
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_committed_overlay_drop_fails_closed() {
    let (dir, log) = temp_log("stream-committed-overlay-drop");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into()],
                required: vec!["name".into()],
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            mikura::OverlayPatch {
                namespace: "ns".into(),
                kind: "Widget".into(),
                key: "w1".into(),
                props: [("name".into(), "from-overlay".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }
            .to_record()
            .unwrap(),
        ],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![],
        version: 2,
        migration: mikura::SchemaMigration {
            drop: vec!["name".into()],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = stream.push(&mut store, v2).unwrap_err();
    assert!(
        err.contains("cannot carry dropped name"),
        "expected committed overlay drop to fail at push, got {err}"
    );
    assert_eq!(stream.uncommitted(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_overlay_merged_instance_allows_recast() {
    let (dir, log) = temp_log("stream-overlay-merged-recast");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("count".into(), "1".into())].into_iter().collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, v1).unwrap();
    stream.push(&mut store, overlay).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("count", "abc")]))
        .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_action_hide_uses_claim_time_overlay() {
    let (dir, log) = temp_log("stream-action-hide-claim-time");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay_a = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "A".into())].into_iter().collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    let overlay_b = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "B".into())].into_iter().collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    stream.push(&mut store, overlay_a).unwrap();
    let mut visible = rec("Widget", "w1", false, &[]);
    visible.action_id = Some("act-1".into());
    stream.push(&mut store, visible.clone()).unwrap();
    stream.push(&mut store, overlay_b).unwrap();
    let mut hide_b = visible.clone();
    hide_b.hidden = true;
    hide_b.props.insert("name".into(), "B".into());
    let err = stream.push(&mut store, hide_b).unwrap_err();
    assert!(
        err.contains("body conflict"),
        "expected hide to compare the claim-time overlay, got {err}"
    );
    let mut hide_a = visible;
    hide_a.hidden = true;
    hide_a.props.insert("name".into(), "A".into());
    stream.push(&mut store, hide_a).unwrap();
    stream.flush(&mut store).unwrap();
    assert!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_later_overlay_does_not_rescue_instance_recast() {
    let (dir, log) = temp_log("stream-later-overlay-recast");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v1).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("count", "abc")]))
        .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("count".into(), "1".into())].into_iter().collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, overlay).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = stream.push(&mut store, v2).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected later overlay not to rematerialize the instance, got {err}"
    );
    assert_eq!(stream.uncommitted(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_action_hide_uses_canonical_timestamp() {
    let (dir, log) = temp_log("stream-action-hide-canonical-ts");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["opened_at".into()],
        types: vec![("opened_at".into(), mikura::PropertyType::Timestamp)],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    let mut visible = rec(
        "Widget",
        "w1",
        false,
        &[("opened_at", "2026-09-19T18:00:00+01:00")],
    );
    visible.action_id = Some("act-ts-1".into());
    stream.push(&mut store, visible.clone()).unwrap();
    let mut hide_raw = visible.clone();
    hide_raw.hidden = true;
    let err = stream.push(&mut store, hide_raw).unwrap_err();
    assert!(
        err.contains("body conflict"),
        "expected hide to compare the canonical timestamp, got {err}"
    );
    let mut hide_canonical = visible;
    hide_canonical.hidden = true;
    hide_canonical
        .props
        .insert("opened_at".into(), "2026-09-19T17:00:00.000Z".into());
    stream.push(&mut store, hide_canonical).unwrap();
    stream.flush(&mut store).unwrap();
    assert!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_action_retry_accepts_canonical_timestamp() {
    let (dir, log) = temp_log("stream-action-retry-canonical-ts");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["opened_at".into()],
        types: vec![("opened_at".into(), mikura::PropertyType::Timestamp)],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    let mut first = rec(
        "Widget",
        "w1",
        false,
        &[("opened_at", "2026-09-19T18:00:00+01:00")],
    );
    first.action_id = Some("act-ts-retry".into());
    stream.push(&mut store, first).unwrap();
    let mut retry = rec(
        "Widget",
        "w1",
        false,
        &[("opened_at", "2026-09-19T17:00:00.000Z")],
    );
    retry.action_id = Some("act-ts-retry".into());
    stream.push(&mut store, retry).unwrap();
    assert_eq!(stream.uncommitted(), 2);
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("opened_at")
            .map(String::as_str),
        Some("2026-09-19T17:00:00.000Z")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_schema_action_retry_after_later_version() {
    let (dir, log) = temp_log("stream-schema-action-historical");
    let mut store = Store::create(&log).unwrap();
    let mut v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    v1.action_id = Some("act-schema-v1".into());
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "note".into()],
        required: vec!["name".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    BatchIngest::run(&mut store, vec![v1.clone(), v2]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, v1).unwrap();
    assert_eq!(stream.uncommitted(), 0);
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .effective_version(),
        2
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_unspecified_schema_after_committed_hide_keeps_chain() {
    let (dir, log) = temp_log("stream-schema-hide-unspecified");
    let mut store = Store::create(&log).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut hide = v2.clone();
    hide.hidden = true;
    BatchIngest::run(&mut store, vec![v1, v2, hide]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let first = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 0,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, first).unwrap();
    let second = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "note".into()],
        required: vec!["name".into()],
        version: 0,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, second).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .effective_version(),
        4
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_committed_hide_skips_overlay_on_recreate() {
    let (dir, log) = temp_log("stream-committed-hide-overlay");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into()],
                required: vec!["name".into()],
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            rec("Widget", "w1", false, &[("name", "orig")]),
            rec("Widget", "w1", true, &[("name", "orig")]),
            mikura::OverlayPatch {
                namespace: "ns".into(),
                kind: "Widget".into(),
                key: "w1".into(),
                props: [("name".into(), "from-overlay".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }
            .to_record()
            .unwrap(),
        ],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let err = stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap_err();
    assert!(
        err.contains("missing required property name"),
        "expected committed hide to skip overlay fill, got {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_action_retry_canonicalizes_under_pending_schema() {
    let (dir, log) = temp_log("stream-action-retry-pending-schema");
    let mut store = Store::create(&log).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["opened_at".into()],
        types: vec![("opened_at".into(), mikura::PropertyType::Timestamp)],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut claimed = rec(
        "Widget",
        "w1",
        false,
        &[("opened_at", "2026-09-19T18:00:00+01:00")],
    );
    claimed.action_id = Some("act-pending-schema".into());
    BatchIngest::run(&mut store, vec![schema, claimed.clone()]).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["opened_at".into(), "note".into()],
        types: vec![("opened_at".into(), mikura::PropertyType::Timestamp)],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.push(&mut store, claimed).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-pending-schema")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_action_hide_skips_overlay_on_committed_hidden_recreate() {
    let (dir, log) = temp_log("stream-action-hide-committed-hidden-recreate");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into()],
                required: vec!["name".into()],
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            rec("Widget", "w1", false, &[("name", "orig")]),
            rec("Widget", "w1", true, &[("name", "orig")]),
            mikura::OverlayPatch {
                namespace: "ns".into(),
                kind: "Widget".into(),
                key: "w1".into(),
                props: [("name".into(), "from-overlay".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }
            .to_record()
            .unwrap(),
        ],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let mut visible = rec("Widget", "w1", false, &[("name", "recreated")]);
    visible.action_id = Some("act-recreate".into());
    stream.push(&mut store, visible.clone()).unwrap();
    let mut hide_overlay = visible.clone();
    hide_overlay.hidden = true;
    hide_overlay
        .props
        .insert("name".into(), "from-overlay".into());
    let err = stream.push(&mut store, hide_overlay).unwrap_err();
    assert!(
        err.contains("body conflict"),
        "expected hide to use the pre-write claimed body, got {err}"
    );
    let mut hide = visible;
    hide.hidden = true;
    stream.push(&mut store, hide).unwrap();
    stream.flush(&mut store).unwrap();
    assert!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_inherited_overlay_action_id_fails_closed() {
    let (dir, log) = temp_log("stream-inherited-overlay-action");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    overlay.action_id = Some("act-overlay".into());
    stream.push(&mut store, schema).unwrap();
    stream.push(&mut store, overlay).unwrap();
    let err = stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap_err();
    assert!(
        err.contains("already committed") && err.contains("act-overlay"),
        "expected inherited overlay Action id to fail closed at push, got {err}"
    );
    assert_eq!(stream.uncommitted(), 2);
    stream.flush(&mut store).unwrap();
    assert!(store
        .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_schema_replacement_without_committed_history() {
    let (dir, log) = temp_log("stream-schema-replace-no-history");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![rec("Widget", "w1", false, &[("name", "n")])],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v1).unwrap();
    let mut v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "note".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    v2.action_id = Some("act-schema-v2".into());
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .effective_version(),
        2
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_overlay_conflict_audit_too_large_fails_closed() {
    let (dir, log) = temp_log("stream-overlay-audit-too-large");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let schema = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        conflict: Some(mikura::ConflictStrategy::User),
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay_value = "o".repeat(2200);
    let source_value = "s".repeat(2200);
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), overlay_value)].into_iter().collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, schema).unwrap();
    stream.push(&mut store, overlay).unwrap();
    let err = stream
        .push(
            &mut store,
            rec("Widget", "w1", false, &[("name", &source_value)]),
        )
        .unwrap_err();
    assert!(
        err.contains("record larger than a page"),
        "expected overlay conflict audit to fail closed at push, got {err}"
    );
    assert_eq!(stream.uncommitted(), 2);
    stream.flush(&mut store).unwrap();
    assert!(store
        .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_action_retry_rejects_illegal_under_pending_schema() {
    let (dir, log) = temp_log("stream-action-retry-illegal-pending-schema");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut claimed = rec("Widget", "w1", false, &[("name", "ok")]);
    claimed.action_id = Some("act-retry-required".into());
    stream.push(&mut store, v1).unwrap();
    stream.push(&mut store, claimed.clone()).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "note".into()],
        required: vec!["note".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    let err = stream.push(&mut store, claimed).unwrap_err();
    assert!(
        err.contains("missing required property note"),
        "expected Action retry to validate under the current schema, got {err}"
    );
    assert_eq!(stream.uncommitted(), 3);
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .action_id
            .as_deref(),
        Some("act-retry-required")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_schema_rename_overlay_too_large_fails_closed() {
    let (dir, log) = temp_log("stream-schema-rename-overlay-too-large");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["n".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("n".into(), "o".repeat(2500))].into_iter().collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, v1).unwrap();
    stream.push(&mut store, overlay).unwrap();
    let renamed = format!("n{}", "x".repeat(1599));
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![renamed.clone()],
        version: 2,
        migration: mikura::SchemaMigration {
            rename: vec![("n".into(), renamed)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = stream.push(&mut store, v2).unwrap_err();
    assert!(
        err.contains("record larger than a page"),
        "expected migrated overlay size to fail closed at schema push, got {err}"
    );
    assert_eq!(stream.uncommitted(), 2);
    stream.flush(&mut store).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_schema_rename_committed_overlay_too_large_fails_closed() {
    let (dir, log) = temp_log("stream-schema-rename-committed-overlay-too-large");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["n".into()],
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            mikura::OverlayPatch {
                namespace: "ns".into(),
                kind: "Widget".into(),
                key: "w1".into(),
                props: [("n".into(), "o".repeat(2500))].into_iter().collect(),
                ..Default::default()
            }
            .to_record()
            .unwrap(),
        ],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let renamed = format!("n{}", "x".repeat(1599));
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![renamed.clone()],
        version: 2,
        migration: mikura::SchemaMigration {
            rename: vec![("n".into(), renamed)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = stream.push(&mut store, v2).unwrap_err();
    assert!(
        err.contains("record larger than a page"),
        "expected committed overlay migration size to fail closed at schema push, got {err}"
    );
    assert_eq!(stream.uncommitted(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_schema_version_trailer_too_large_fails_closed() {
    let (dir, log) = temp_log("stream-schema-version-trailer-too-large");
    let mut store = Store::create(&log).unwrap();
    let mut probe = rec("Widget", "w1", false, &[("name", "")]);
    let mut n = 3800;
    loop {
        probe.props.insert("name".into(), "x".repeat(n));
        if store.check_record_fits(&probe).is_err() {
            n -= 1;
            probe.props.insert("name".into(), "x".repeat(n));
            break;
        }
        n += 1;
        assert!(n < 5000, "did not find a page-limit instance payload");
    }
    assert!(store.check_record_fits(&probe).is_ok());
    probe.schema_version = 2;
    assert!(
        store.check_record_fits(&probe).is_err(),
        "payload must overflow once schema version 2 is stamped"
    );
    probe.schema_version = 1;
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into()],
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
        )
        .unwrap();
    stream
        .push(
            &mut store,
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into()],
                version: 2,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
        )
        .unwrap();
    let err = stream.push(&mut store, probe).unwrap_err();
    assert!(
        err.contains("record larger than a page"),
        "expected pending schema version stamp to fail closed at push, got {err}"
    );
    assert_eq!(stream.uncommitted(), 2);
    stream.flush(&mut store).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_committed_action_replay_is_not_pending() {
    let (dir, log) = temp_log("stream-committed-action-replay-not-pending");
    let mut store = Store::create(&log).unwrap();
    let mut claimed = rec("Widget", "w1", false, &[("count", "abc")]);
    claimed.action_id = Some("act-count".into());
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["count".into()],
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            claimed.clone(),
            rec("Widget", "w1", false, &[("count", "1")]),
        ],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, claimed).unwrap();
    assert_eq!(stream.uncommitted(), 0);
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_schema_rename_generated_audit_too_large_fails_closed() {
    let (dir, log) = temp_log("stream-schema-rename-generated-audit-too-large");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v1 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["n".into()],
        conflict: Some(mikura::ConflictStrategy::User),
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("n".into(), "o".into())].into_iter().collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    stream.push(&mut store, v1).unwrap();
    stream.push(&mut store, overlay).unwrap();
    stream
        .push(
            &mut store,
            rec("Widget", "w1", false, &[("n", &"s".repeat(2000))]),
        )
        .unwrap();
    let renamed = format!("n{}", "x".repeat(1499));
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![renamed.clone()],
        conflict: Some(mikura::ConflictStrategy::User),
        version: 2,
        migration: mikura::SchemaMigration {
            rename: vec![("n".into(), renamed)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = stream.push(&mut store, v2).unwrap_err();
    assert!(
        err.contains("record larger than a page"),
        "expected generated overlay-audit migration size to fail closed at schema push, got {err}"
    );
    assert_eq!(stream.uncommitted(), 3);
    stream.flush(&mut store).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_hidden_action_retry_is_stable_across_flush() {
    let (dir, log) = temp_log("stream-hidden-action-retry-stable");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            types: vec![("count".into(), mikura::PropertyType::Integer)],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut hide = rec("Widget", "w1", true, &[("count", "abc")]);
    hide.action_id = Some("act-hide-illegal".into());
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, hide.clone()).unwrap();
    assert_eq!(stream.uncommitted(), 1);
    stream.push(&mut store, hide.clone()).unwrap();
    assert_eq!(stream.uncommitted(), 1);
    stream.flush(&mut store).unwrap();
    stream.push(&mut store, hide).unwrap();
    assert_eq!(stream.uncommitted(), 0);
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("abc")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_action_id_blocks_later_batch() {
    let (dir, log) = temp_log("stream-buffered-action-blocks-batch");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut queued = rec(
        "Shipment",
        "s4",
        false,
        &[("order_id", "o1"), ("amount", "1")],
    );
    queued.action_id = Some("act-stream-reserve".into());
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, queued).unwrap();
    assert_eq!(stream.uncommitted(), 1);
    let mut remapped = rec(
        "Shipment",
        "s5",
        false,
        &[("order_id", "o1"), ("amount", "2")],
    );
    remapped.action_id = Some("act-stream-reserve".into());
    let err = BatchIngest::run(&mut store, vec![remapped]).unwrap_err();
    assert!(
        err.contains("already committed"),
        "expected buffered Action id to fence a later batch, got {err}"
    );
    assert!(store
        .load("ns", "Shipment", "s5", &PropertyAcl::allow_all())
        .is_err());
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Shipment", "s4", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("amount")
            .map(String::as_str),
        Some("1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_instance_hide_drops_generated_audit_for_schema_drop() {
    let (dir, log) = temp_log("stream-hide-drops-generated-audit");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into()],
                conflict: Some(mikura::ConflictStrategy::User),
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            mikura::OverlayPatch {
                namespace: "ns".into(),
                kind: "Widget".into(),
                key: "w1".into(),
                props: [("name".into(), "from-overlay".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }
            .to_record()
            .unwrap(),
        ],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            rec("Widget", "w1", false, &[("name", "from-source")]),
        )
        .unwrap();
    stream
        .push(
            &mut store,
            rec("Widget", "w1", true, &[("name", "from-overlay")]),
        )
        .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![],
        version: 2,
        migration: mikura::SchemaMigration {
            drop: vec!["name".into()],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    stream.flush(&mut store).unwrap();
    assert!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_action_apply_action_is_durable() {
    let (dir, log) = temp_log("stream-buffered-action-apply-action");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut queued = rec(
        "Shipment",
        "s1",
        false,
        &[("order_id", "o1"), ("amount", "3")],
    );
    queued.action_id = Some("act-apply-reserve".into());
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, queued.clone()).unwrap();
    store
        .apply_action(
            mikura::Action {
                id: "act-apply-reserve".into(),
                namespace: "ns".into(),
                kind: "Shipment".into(),
                key: "s1".into(),
                props: queued.props.clone(),
            },
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .load("ns", "Shipment", "s1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("amount")
            .map(String::as_str),
        Some("3")
    );
    stream.flush(&mut store).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_action_id_blocks_change_set() {
    let (dir, log) = temp_log("stream-buffered-action-blocks-change-set");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut queued = rec(
        "Shipment",
        "s4",
        false,
        &[("order_id", "o1"), ("amount", "1")],
    );
    queued.action_id = Some("act-change-set-steal".into());
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, queued).unwrap();
    let err = store
        .apply_change_set(mikura::ChangeSet {
            id: "act-change-set-steal".into(),
            namespace: "ns".into(),
            edits: vec![mikura::Edit::Put {
                kind: "Shipment".into(),
                key: "s1".into(),
                props: [
                    ("order_id".into(), "o1".into()),
                    ("amount".into(), "9".into()),
                ]
                .into_iter()
                .collect(),
                expected_gen: None,
            }],
        })
        .unwrap_err();
    assert!(
        err.contains("already committed"),
        "expected buffered Action id to fence a change set, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Shipment", "s4", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("amount")
            .map(String::as_str),
        Some("1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_visible_blocks_later_hide() {
    let (dir, log) = temp_log("stream-buffered-visible-blocks-hide");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut queued = rec(
        "Shipment",
        "s4",
        false,
        &[("order_id", "o1"), ("amount", "1")],
    );
    queued.action_id = Some("act-hide-steal".into());
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, queued.clone()).unwrap();
    let mut hide = queued;
    hide.hidden = true;
    let err = BatchIngest::run(&mut store, vec![hide]).unwrap_err();
    assert!(
        err.contains("already committed"),
        "expected buffered visible Action id to fence a later hide, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert!(
        !store
            .load("ns", "Shipment", "s4", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_abandoned_action_retry_reaches_log() {
    let (dir, log) = temp_log("stream-abandoned-action-retry");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(&mut store, fixture()).unwrap();
    let mut queued = rec(
        "Shipment",
        "s4",
        false,
        &[("order_id", "o1"), ("amount", "1")],
    );
    queued.action_id = Some("act-abandoned-retry".into());
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, queued.clone()).unwrap();
    assert_eq!(stream.uncommitted(), 1);
    drop(stream);
    let mut retry = StreamIngest::new(8).unwrap();
    retry.push(&mut store, queued).unwrap();
    assert_eq!(retry.uncommitted(), 1);
    retry.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Shipment", "s4", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("amount")
            .map(String::as_str),
        Some("1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_abandoned_instance_retry_releases_fence() {
    let (dir, log) = temp_log("stream-abandoned-instance-retry");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let body = rec("Widget", "w1", false, &[("count", "abc")]);
    let mut abandoned = StreamIngest::new(8).unwrap();
    abandoned.push(&mut store, body.clone()).unwrap();
    drop(abandoned);
    let mut retry = StreamIngest::new(8).unwrap();
    retry.push(&mut store, body).unwrap();
    retry.flush(&mut store).unwrap();
    BatchIngest::run(
        &mut store,
        vec![rec("Widget", "w1", false, &[("count", "1")])],
    )
    .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    BatchIngest::run(&mut store, vec![v2]).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_discard_keeps_sibling_instance_fence() {
    let (dir, log) = temp_log("stream-discard-keeps-sibling-instance");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let body = rec("Widget", "w1", false, &[("count", "abc")]);
    let mut first = StreamIngest::new(8).unwrap();
    first.push(&mut store, body.clone()).unwrap();
    let mut second = StreamIngest::new(8).unwrap();
    second.push(&mut store, body).unwrap();
    second.discard(&mut store);
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected discard to keep the sibling buffer's instance fence, got {err}"
    );
    first.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("abc")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_sibling_flush_keeps_instance_fence() {
    let (dir, log) = temp_log("stream-sibling-flush-keeps-instance");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let body = rec("Widget", "w1", false, &[("count", "abc")]);
    let mut first = StreamIngest::new(8).unwrap();
    first.push(&mut store, body.clone()).unwrap();
    let mut second = StreamIngest::new(8).unwrap();
    second.push(&mut store, body).unwrap();
    second.flush(&mut store).unwrap();
    BatchIngest::run(
        &mut store,
        vec![rec("Widget", "w1", false, &[("count", "1")])],
    )
    .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected a sibling stream's instance fence to survive flush of the same body, got {err}"
    );
    first.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("abc")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_instance_blocks_later_schema_cast() {
    let (dir, log) = temp_log("stream-buffered-instance-blocks-schema");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("count", "abc")]))
        .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected intervening schema recast to fail closed on a buffered instance, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("abc")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_overlay_blocks_later_schema_drop() {
    let (dir, log) = temp_log("stream-buffered-overlay-blocks-schema");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, overlay).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![],
        version: 2,
        migration: mikura::SchemaMigration {
            drop: vec!["name".into()],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("cannot migrate overlays") && err.contains("name"),
        "expected intervening schema drop to fail closed on a buffered overlay, got {err}"
    );
    stream.flush(&mut store).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_encrypted_missing_grant_fails_at_push() {
    let (dir, log) = temp_log("stream-enc-missing-grant-at-push");
    let provider: std::sync::Arc<dyn mikura::KeyProvider + Send + Sync> = std::sync::Arc::new(
        mikura::MemoryKeyProvider::new(mikura::RuntimeProfile::Development).unwrap(),
    );
    let grant = |tenant: &str| mikura::UnwrapGrant {
        tenant_id: mikura::TenantId::new(tenant).unwrap(),
        purpose: mikura::KeyPurpose::Store,
        expires_unix_secs: i64::MAX,
    };
    let keys = mikura::LogKeys::new(
        std::sync::Arc::clone(&provider),
        vec![grant(mikura::STORE_KEY_TENANT), grant("acme")],
    )
    .unwrap();
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    let acme_ns = mikura::tenant_namespace("acme", "ns").unwrap();
    let beta_ns = mikura::tenant_namespace("beta", "ns").unwrap();
    let acme = ObjectRecord {
        gen: 1,
        namespace: acme_ns.clone(),
        kind: "Item".into(),
        key: "a1".into(),
        hidden: false,
        action_id: None,
        schema_version: 1,
        props: [("secret".into(), "acme-secret".into())]
            .into_iter()
            .collect(),
    };
    let beta = ObjectRecord {
        gen: 1,
        namespace: beta_ns,
        kind: "Item".into(),
        key: "b1".into(),
        hidden: false,
        action_id: None,
        schema_version: 1,
        props: [("secret".into(), "beta-secret".into())]
            .into_iter()
            .collect(),
    };
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, acme).unwrap();
    let err = stream.push(&mut store, beta).unwrap_err();
    assert!(
        err.contains("grant") || err.contains("tenant"),
        "expected missing unwrap grant to fail at stream push, got {err}"
    );
    assert_eq!(stream.uncommitted(), 1);
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load(&acme_ns, "Item", "a1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("acme-secret")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_instance_blocks_later_required_schema() {
    let (dir, log) = temp_log("stream-buffered-instance-blocks-required");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("name", "a")]))
        .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "note".into()],
        required: vec!["note".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("missing required property note"),
        "expected intervening required schema to fail closed on a buffered instance, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("a")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_batch_replace_keeps_buffered_instance_fence() {
    let (dir, log) = temp_log("stream-batch-replace-keeps-fence");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("count", "abc")]))
        .unwrap();
    BatchIngest::run(
        &mut store,
        vec![rec("Widget", "w1", false, &[("count", "1")])],
    )
    .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected buffered instance fence to survive a later write of the same identity, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("abc")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_schema_blocks_later_schema() {
    let (dir, log) = temp_log("stream-buffered-schema-blocks-later");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into(), "note".into()],
                version: 2,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
        )
        .unwrap();
    let other = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "label".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![other]).unwrap_err();
    assert!(
        err.contains("already admitted"),
        "expected intervening schema to fail closed on a buffered descriptor, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store.schema("ns", "Widget").unwrap().unwrap().properties,
        vec!["name".to_string(), "note".to_string()]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_earlier_buffered_body_blocks_later_schema_cast() {
    let (dir, log) = temp_log("stream-earlier-body-blocks-schema");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("count", "abc")]))
        .unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("count", "1")]))
        .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected earlier buffered body to fence an intervening recast, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("1")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_identical_batch_keeps_buffered_instance_fence() {
    let (dir, log) = temp_log("stream-identical-batch-keeps-fence");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[("name", "a")]))
        .unwrap();
    BatchIngest::run(
        &mut store,
        vec![rec("Widget", "w1", false, &[("name", "a")])],
    )
    .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "note".into()],
        required: vec!["note".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("missing required property note"),
        "expected identical batch write not to drop a buffered instance fence, got {err}"
    );
    stream.flush(&mut store).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_discard_releases_schema_fence() {
    let (dir, log) = temp_log("stream-discard-releases-schema");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into(), "note".into()],
                version: 2,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
        )
        .unwrap();
    stream.discard(&mut store);
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into(), "label".into()],
            version: 2,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut properties = store.schema("ns", "Widget").unwrap().unwrap().properties;
    properties.sort();
    assert_eq!(properties, vec!["label".to_string(), "name".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_overlay_cannot_steal_action_body() {
    let (dir, log) = temp_log("stream-overlay-cannot-steal-action");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut queued = rec("Widget", "w1", false, &[("name", "A")]);
    queued.action_id = Some("act-overlay-steal".into());
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, queued).unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "B".into())].into_iter().collect(),
        action_id: Some("act-overlay-row".into()),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![overlay]).unwrap_err();
    assert!(
        err.contains("body conflict") || err.contains("already committed"),
        "expected overlay not to rewrite a buffered Action body, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("A")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_identical_schema_blocks_later_schema() {
    let (dir, log) = temp_log("stream-identical-schema-blocks-later");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "note".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, v2.clone()).unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("already admitted"),
        "expected identical intervening schema to fail closed on a buffered descriptor, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store.schema("ns", "Widget").unwrap().unwrap().properties,
        vec!["name".to_string(), "note".to_string()]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_first_schema_blocks_untyped_instance() {
    let (dir, log) = temp_log("stream-first-schema-blocks-untyped");
    let mut store = Store::create(&log).unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap();
    let first = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        version: 1,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![first]).unwrap_err();
    assert!(
        err.contains("missing required property name"),
        "expected first schema to fail closed on a buffered untyped instance, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert!(store.schema("ns", "Widget").unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_overlay_hide_blocks_buffered_instance() {
    let (dir, log) = temp_log("stream-overlay-hide-blocks-instance");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into()],
                required: vec!["name".into()],
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            mikura::OverlayPatch {
                namespace: "ns".into(),
                kind: "Widget".into(),
                key: "w1".into(),
                props: [("name".into(), "from-overlay".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }
            .to_record()
            .unwrap(),
        ],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap();
    let mut hide = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    hide.hidden = true;
    let err = BatchIngest::run(&mut store, vec![hide]).unwrap_err();
    assert!(
        err.contains("missing required property name"),
        "expected overlay hide to fail closed on a buffered instance that needs it, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("from-overlay")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_schema_blocks_later_instance_cast() {
    let (dir, log) = temp_log("stream-buffered-schema-blocks-instance");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    stream.push(&mut store, v2).unwrap();
    let err = BatchIngest::run(
        &mut store,
        vec![rec("Widget", "w1", false, &[("count", "abc")])],
    )
    .unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected intervening instance to fail closed under a buffered recast, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .property_type("count"),
        Some(mikura::PropertyType::Integer)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_overlay_newest_requires_buffered_source_timestamp() {
    let (dir, log) = temp_log("stream-overlay-newest-buffered-ts");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into(), "updated_at".into()],
            required: vec!["name".into()],
            types: vec![("updated_at".into(), mikura::PropertyType::Timestamp)],
            conflict: Some(mikura::ConflictStrategy::Newest),
            conflict_time: Some("updated_at".into()),
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            rec("Widget", "w1", false, &[("name", "source")]),
        )
        .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        at: Some("2026-10-11T10:00:00.000Z".into()),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![overlay]).unwrap_err();
    assert!(
        err.contains("newest") || err.contains("timestamp"),
        "expected intervening newest overlay to fail closed without a source timestamp, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("source")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_overlay_without_action_id_cannot_rewrite_buffered_action() {
    let (dir, log) = temp_log("stream-overlay-no-action-id-skip");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut queued = rec("Widget", "w1", false, &[("name", "A")]);
    queued.action_id = Some("act-overlay-skip".into());
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, queued).unwrap();
    stream
        .push(
            &mut store,
            mikura::OverlayPatch {
                namespace: "ns".into(),
                kind: "Widget".into(),
                key: "w1".into(),
                props: [("name".into(), "A".into())].into_iter().collect(),
                ..Default::default()
            }
            .to_record()
            .unwrap(),
        )
        .unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "B".into())].into_iter().collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![overlay]).unwrap_err();
    assert!(
        err.contains("body conflict") || err.contains("already committed"),
        "expected overlay without an Action id not to rewrite a buffered Action body, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("A")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_buffered_schema_blocks_later_overlay_drop() {
    let (dir, log) = temp_log("stream-buffered-schema-blocks-overlay");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![],
        version: 2,
        migration: mikura::SchemaMigration {
            drop: vec!["name".into()],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, v2).unwrap();
    let overlay = mikura::OverlayPatch {
        namespace: "ns".into(),
        kind: "Widget".into(),
        key: "w1".into(),
        props: [("name".into(), "from-overlay".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    }
    .to_record()
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![overlay]).unwrap_err();
    assert!(
        err.contains("cannot migrate overlays") && err.contains("name"),
        "expected intervening overlay to fail closed under a buffered drop, got {err}"
    );
    stream.flush(&mut store).unwrap();
    assert!(store
        .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_discard_keeps_sibling_action_reservation() {
    let (dir, log) = temp_log("stream-discard-keeps-sibling-action");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut queued = rec("Widget", "w1", false, &[("name", "A")]);
    queued.action_id = Some("act-sibling".into());
    let mut first = StreamIngest::new(8).unwrap();
    first.push(&mut store, queued.clone()).unwrap();
    let mut second = StreamIngest::new(8).unwrap();
    second.push(&mut store, queued).unwrap();
    first.discard(&mut store);
    let mut steal = rec("Widget", "w2", false, &[("name", "B")]);
    steal.action_id = Some("act-sibling".into());
    let err = BatchIngest::run(&mut store, vec![steal]).unwrap_err();
    assert!(
        err.contains("already committed"),
        "expected a sibling stream's Action reservation to survive discard, got {err}"
    );
    second.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("A")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_pending_newest_schema_preserves_action_merge() {
    let (dir, log) = temp_log("stream-pending-newest-action-merge");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into(), "updated_at".into()],
                types: vec![("updated_at".into(), mikura::PropertyType::Timestamp)],
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
            mikura::OverlayPatch {
                namespace: "ns".into(),
                kind: "Widget".into(),
                key: "w1".into(),
                props: [("name".into(), "from-overlay".into())]
                    .into_iter()
                    .collect(),
                at: Some("2026-10-11T10:00:00.000Z".into()),
                ..Default::default()
            }
            .to_record()
            .unwrap(),
        ],
    )
    .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "updated_at".into()],
        types: vec![("updated_at".into(), mikura::PropertyType::Timestamp)],
        conflict: Some(mikura::ConflictStrategy::Newest),
        conflict_time: Some("updated_at".into()),
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream.push(&mut store, v2).unwrap();
    let mut queued = rec(
        "Widget",
        "w1",
        false,
        &[
            ("name", "from-source"),
            ("updated_at", "2026-10-11T12:00:00.000Z"),
        ],
    );
    queued.action_id = Some("act-newest-merge".into());
    stream.push(&mut store, queued).unwrap();
    stream.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("from-source")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_unflushed_schema_does_not_steer_committed_overlay_merge() {
    let (dir, log) = temp_log("stream-unflushed-schema-overlay-merge");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::OverlayPatch {
            namespace: "ns".into(),
            kind: "Widget".into(),
            key: "w1".into(),
            props: [("name".into(), "from-overlay".into())]
                .into_iter()
                .collect(),
            at: Some("2026-10-11T10:00:00.000Z".into()),
            ..Default::default()
        }
        .to_record()
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            mikura::SchemaDescriptor {
                kind: "Widget".into(),
                properties: vec!["name".into(), "updated_at".into()],
                types: vec![("updated_at".into(), mikura::PropertyType::Timestamp)],
                conflict: Some(mikura::ConflictStrategy::Newest),
                conflict_time: Some("updated_at".into()),
                version: 1,
                ..Default::default()
            }
            .to_record("ns")
            .unwrap(),
        )
        .unwrap();
    BatchIngest::run(
        &mut store,
        vec![rec(
            "Widget",
            "w1",
            false,
            &[
                ("name", "from-source"),
                ("updated_at", "2026-10-11T12:00:00.000Z"),
            ],
        )],
    )
    .unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("from-overlay"),
        "committed overlay merge must not use an unflushed schema conflict plan"
    );
    stream.discard(&mut store);
    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("from-overlay")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_sibling_schema_conflicts_fail_closed() {
    let (dir, log) = temp_log("stream-sibling-schema-conflict");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let v2_note = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "note".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let v2_color = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["name".into(), "color".into()],
        version: 2,
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut first = StreamIngest::new(8).unwrap();
    first.push(&mut store, v2_note).unwrap();
    let mut second = StreamIngest::new(8).unwrap();
    let err = second.push(&mut store, v2_color).unwrap_err();
    assert!(
        err.contains("already admitted"),
        "expected a sibling stream not to admit a conflicting schema, got {err}"
    );
    first.flush(&mut store).unwrap();
    assert!(store
        .schema("ns", "Widget")
        .unwrap()
        .unwrap()
        .properties
        .iter()
        .any(|name| name == "note"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_discard_releases_matching_body_only() {
    let (dir, log) = temp_log("stream-discard-matching-body");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut first = StreamIngest::new(8).unwrap();
    first
        .push(&mut store, rec("Widget", "w1", false, &[("count", "abc")]))
        .unwrap();
    let mut second = StreamIngest::new(8).unwrap();
    second
        .push(&mut store, rec("Widget", "w1", false, &[("count", "1")]))
        .unwrap();
    second.discard(&mut store);
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let err = BatchIngest::run(&mut store, vec![v2]).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected discard to keep the other buffer's body fence, got {err}"
    );
    first.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("abc")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_discard_releases_merged_instance_fence() {
    let (dir, log) = temp_log("stream-discard-merged-instance");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["name".into()],
            required: vec!["name".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut stream = StreamIngest::new(8).unwrap();
    stream
        .push(
            &mut store,
            mikura::OverlayPatch {
                namespace: "ns".into(),
                kind: "Widget".into(),
                key: "w1".into(),
                props: [("name".into(), "from-overlay".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            }
            .to_record()
            .unwrap(),
        )
        .unwrap();
    stream
        .push(&mut store, rec("Widget", "w1", false, &[]))
        .unwrap();
    stream.discard(&mut store);
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec![],
        version: 2,
        migration: mikura::SchemaMigration {
            drop: vec!["name".into()],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    BatchIngest::run(&mut store, vec![v2]).unwrap();
    assert!(store
        .schema("ns", "Widget")
        .unwrap()
        .unwrap()
        .properties
        .is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_sibling_instance_blocked_by_buffered_schema() {
    let (dir, log) = temp_log("stream-sibling-instance-blocked-schema");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut first = StreamIngest::new(8).unwrap();
    first.push(&mut store, v2).unwrap();
    let mut second = StreamIngest::new(8).unwrap();
    let err = second
        .push(&mut store, rec("Widget", "w1", false, &[("count", "abc")]))
        .unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected a sibling instance to fail closed under a buffered recast, got {err}"
    );
    first.flush(&mut store).unwrap();
    assert_eq!(
        store
            .schema("ns", "Widget")
            .unwrap()
            .unwrap()
            .property_type("count"),
        Some(mikura::PropertyType::Integer)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_sibling_schema_blocked_by_buffered_instance() {
    let (dir, log) = temp_log("stream-sibling-schema-blocked-instance");
    let mut store = Store::create(&log).unwrap();
    BatchIngest::run(
        &mut store,
        vec![mikura::SchemaDescriptor {
            kind: "Widget".into(),
            properties: vec!["count".into()],
            version: 1,
            ..Default::default()
        }
        .to_record("ns")
        .unwrap()],
    )
    .unwrap();
    let mut first = StreamIngest::new(8).unwrap();
    first
        .push(&mut store, rec("Widget", "w1", false, &[("count", "abc")]))
        .unwrap();
    let v2 = mikura::SchemaDescriptor {
        kind: "Widget".into(),
        properties: vec!["count".into()],
        types: vec![("count".into(), mikura::PropertyType::Integer)],
        version: 2,
        migration: mikura::SchemaMigration {
            cast: vec![("count".into(), mikura::PropertyType::Integer)],
            ..Default::default()
        },
        ..Default::default()
    }
    .to_record("ns")
    .unwrap();
    let mut second = StreamIngest::new(8).unwrap();
    let err = second.push(&mut store, v2).unwrap_err();
    assert!(
        err.contains("cannot migrate instances") && err.contains("count"),
        "expected a sibling schema not to strand on another buffer's instance, got {err}"
    );
    first.flush(&mut store).unwrap();
    assert_eq!(
        store
            .load("ns", "Widget", "w1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("count")
            .map(String::as_str),
        Some("abc")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
