use super::super::*;
use super::helpers::*;
use crate::log::{SyncPolicy, PAGE};

fn put(kind: &str, key: &str, props: &[(&str, &str)], expected_gen: Option<u64>) -> Edit {
    Edit::Put {
        kind: kind.into(),
        key: key.into(),
        props: props
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
        expected_gen,
    }
}

fn hide(kind: &str, key: &str) -> Edit {
    Edit::Hide {
        kind: kind.into(),
        key: key.into(),
        expected_gen: None,
    }
}

fn set(id: &str, edits: Vec<Edit>) -> ChangeSet {
    ChangeSet {
        id: id.into(),
        namespace: NS.into(),
        edits,
    }
}

/// Reassign: link s2 to o1, update o1, hide s1.
fn reassign() -> ChangeSet {
    set(
        "act-reassign",
        vec![
            put(
                "Shipment",
                "s2",
                &[("order_id", "o1"), ("amount", "5")],
                Some(0),
            ),
            put(
                "Order",
                "o1",
                &[("customer_id", "c1"), ("state", "moved")],
                None,
            ),
            hide("Shipment", "s1"),
        ],
    )
}

fn gen_of(store: &Store, kind: &str, key: &str) -> Option<u64> {
    store
        .load(NS, kind, key, &PropertyAcl::allow_all())
        .ok()
        .map(|record| record.gen)
}

fn sum(store: &Store) -> i64 {
    ObjectSet::new(LocalCompute)
        .evaluate(store, &fixture_request())
        .unwrap()
        .sum_amount
}

#[test]
fn a_change_set_commits_every_member_under_one_id_and_replays() {
    let (dir, log) = temp_log("change-set");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    store.apply_change_set(reassign()).unwrap();

    let s2 = store
        .load(NS, "Shipment", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(s2.action_id.as_deref(), Some("act-reassign"));
    let s1 = store
        .load(NS, "Shipment", "s1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(s1.hidden);
    assert_eq!(s1.action_id.as_deref(), Some("act-reassign"));
    assert_eq!(sum(&store), 5);
    let gens = [
        gen_of(&store, "Shipment", "s2"),
        gen_of(&store, "Order", "o1"),
        gen_of(&store, "Shipment", "s1"),
    ];

    // Replay ignores the now stale expected generation and appends nothing.
    store.apply_change_set(reassign()).unwrap();
    let pages = store.snapshot_stamp();
    drop(store);

    for drop_sidecar in [false, true] {
        if drop_sidecar {
            std::fs::remove_file(Store::join_map_path(&log)).unwrap();
            let _ = std::fs::remove_file(Store::join_delta_path(&log));
        }
        let mut store = Store::open(&log).unwrap();
        store.apply_change_set(reassign()).unwrap();
        assert_eq!(store.snapshot_stamp(), pages);
        assert_eq!(gen_of(&store, "Order", "o1"), gens[1]);
        assert_eq!(gen_of(&store, "Shipment", "s1"), gens[2]);

        let mut changed = reassign();
        changed.edits.pop();
        let error = store.apply_change_set(changed).unwrap_err();
        assert!(error.contains("body conflict"), "{error}");
        let error = store
            .apply_action(
                Action {
                    id: "act-reassign".into(),
                    namespace: NS.into(),
                    kind: "Shipment".into(),
                    key: "s2".into(),
                    props: HashMap::from([
                        ("order_id".into(), "o1".into()),
                        ("amount".into(), "5".into()),
                    ]),
                },
                None,
            )
            .unwrap_err();
        assert!(error.contains("already committed"), "{error}");
        assert_eq!(store.snapshot_stamp(), pages);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_id_used_by_another_write_fails_closed() {
    let (dir, log) = temp_log("change-set-taken");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let mut claimed = rec("Shipment", "s2", false, &[("order_id", "o1")]);
    claimed.action_id = Some("act-reassign".into());
    store.append(claimed).unwrap();
    let error = store.apply_change_set(reassign()).unwrap_err();
    assert!(error.contains("already committed"), "{error}");
    assert_eq!(gen_of(&store, "Order", "o1"), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn any_failing_member_rejects_the_whole_set() {
    let (dir, log) = temp_log("change-set-reject");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let stamp = store.snapshot_stamp();
    let unchanged = |store: &Store| {
        assert_eq!(gen_of(store, "Shipment", "s2"), None);
        assert_eq!(gen_of(store, "Order", "o1"), Some(1));
        assert!(
            !store
                .load(NS, "Shipment", "s1", &PropertyAcl::allow_all())
                .unwrap()
                .hidden
        );
        assert_eq!(gen_of(store, CHANGE_SET_KIND, "act-reassign"), None);
    };

    let mut stale = reassign();
    stale.edits[1] = put("Order", "o1", &[("customer_id", "c1")], Some(7));
    let error = store.apply_change_set(stale).unwrap_err();
    assert!(error.contains("stale generation"), "{error}");
    unchanged(&store);

    // The last member fails while writing, after the others were buffered.
    let mut oversize = reassign();
    oversize
        .edits
        .push(put("Item", "big", &[("payload", &"x".repeat(PAGE))], None));
    let error = store.apply_change_set(oversize).unwrap_err();
    assert!(error.contains("larger than a page"), "{error}");
    unchanged(&store);
    assert_eq!(store.snapshot_stamp(), stamp);

    for (edits, expected) in [
        (Vec::new(), "no edits"),
        (vec![hide("Order", "o1"), hide("Order", "o1")], "twice"),
        (vec![put(SCHEMA_KIND, "Order", &[], None)], "reserved"),
        (vec![hide("Order", "missing")], "unknown identity"),
    ] {
        let error = store.apply_change_set(set("act-x", edits)).unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    let mut forged = rec(CHANGE_SET_KIND, "act-y", false, &[]);
    forged.action_id = Some("act-y".into());
    let error = store.append(forged).unwrap_err();
    assert!(error.contains("apply_change_set"), "{error}");

    // The aborted id stays free: the valid set still commits under it.
    store.apply_change_set(reassign()).unwrap();
    drop(store);
    let store = Store::open(&log).unwrap();
    assert_eq!(sum(&store), 5);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_invisible_or_denied_member_rejects_the_whole_set() {
    let (dir, log) = temp_log("change-set-restriction");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    let stamp = store.snapshot_stamp();

    let mut hidden_identity = PropertyAcl::allow_all();
    hidden_identity.insert_hide_identity("Order", "o1").unwrap();
    let denied_put = PropertyAcl::deny_property("Order", "state");
    // Hiding s1 copies `amount`, so a deny on it fails the hide member.
    let denied_copy = PropertyAcl::deny_property("Shipment", "amount");
    for acl in [hidden_identity, denied_put, denied_copy] {
        store
            .apply_change_set_in_view(reassign(), &acl)
            .unwrap_err();
        assert_eq!(store.snapshot_stamp(), stamp);
        assert_eq!(gen_of(&store, "Shipment", "s2"), None);
        assert_eq!(gen_of(&store, "Order", "o1"), Some(1));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_crash_at_any_page_boundary_keeps_the_set_all_or_nothing() {
    let (dir, log) = temp_log("change-set-crash");
    let mut store = Store::create_with_sync(&log, SyncPolicy::Group(1)).unwrap();
    append_all(&mut store, fixture());
    let before = std::fs::read(&log).unwrap();
    let payload = "x".repeat(1500);
    let mut edits: Vec<Edit> = (0..8)
        .map(|i| put("Item", &format!("k{i}"), &[("payload", &payload)], None))
        .collect();
    edits.push(hide("Shipment", "s1"));
    store.apply_change_set(set("act-bulk", edits)).unwrap();
    drop(store);
    let after = std::fs::read(&log).unwrap();
    let (first, last) = (before.len() / PAGE, after.len() / PAGE);
    assert!(last - first >= 4, "set spans {} pages", last - first);
    let crashed = dir.join("crashed.mikura");

    // Data pages up to `pages` reached the disk; the superblock either
    // still names the old commit point or, only once every page is there,
    // the new one.
    let mut cases: Vec<(usize, bool)> = (first..=last).map(|pages| (pages, false)).collect();
    cases.push((last, true));
    for (pages, committed) in cases {
        let superblock = if committed { &after } else { &before };
        let mut bytes = superblock[..PAGE].to_vec();
        bytes.extend_from_slice(&after[PAGE..pages * PAGE]);
        std::fs::write(&crashed, bytes).unwrap();
        let store = Store::open(&crashed).unwrap();
        let present = (0..8)
            .filter(|i| gen_of(&store, "Item", &format!("k{i}")).is_some())
            .count();
        let s1_hidden = store
            .load(NS, "Shipment", "s1", &PropertyAcl::allow_all())
            .unwrap()
            .hidden;
        let marker = gen_of(&store, CHANGE_SET_KIND, "act-bulk").is_some();
        if committed {
            assert_eq!((present, s1_hidden, marker), (8, true, true));
        } else {
            assert_eq!((present, s1_hidden, marker), (0, false, false), "{pages}");
        }
        drop(store);
        let _ = std::fs::remove_file(Store::join_map_path(&crashed));
        let _ = std::fs::remove_file(Store::join_delta_path(&crashed));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_replay_compares_put_bodies_after_canonicalization() {
    let (dir, log) = temp_log("change-set-canonical");
    let mut store = Store::create(&log).unwrap();
    let schema = SchemaDescriptor {
        kind: "incident".into(),
        properties: vec!["opened_at".into()],
        types: vec![("opened_at".into(), PropertyType::Timestamp)],
        ..Default::default()
    };
    store.append(schema.to_record(NS).unwrap()).unwrap();
    let offset = || {
        set(
            "act-open",
            vec![put(
                "incident",
                "inc-1",
                &[("opened_at", "2026-09-19T18:00:00+01:00")],
                None,
            )],
        )
    };
    store.apply_change_set(offset()).unwrap();
    let stamp = store.snapshot_stamp();
    store.apply_change_set(offset()).unwrap();
    assert_eq!(store.snapshot_stamp(), stamp);
    let mut later = offset();
    later.edits = vec![put(
        "incident",
        "inc-1",
        &[("opened_at", "2026-09-19T18:00:01+01:00")],
        None,
    )];
    let error = store.apply_change_set(later).unwrap_err();
    assert!(error.contains("body conflict"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}
