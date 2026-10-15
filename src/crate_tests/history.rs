use super::super::*;
use super::helpers::*;

fn contact(region: &str, name: &str) -> ObjectRecord {
    rec(
        "contact",
        "c-1",
        false,
        &[("region", region), ("name", name)],
    )
}

fn props(record: &ObjectRecord) -> Vec<(String, String)> {
    let mut props: Vec<_> = record.props.clone().into_iter().collect();
    props.sort();
    props
}

/// Source write, overlay edit, source refresh, whole-record Action, hide,
/// recreate. After every commit, remember the commit point and what `load`
/// answered then.
fn lifecycle(store: &mut Store) -> Vec<(u32, Option<ObjectRecord>)> {
    let acl = PropertyAcl::allow_all();
    let mut seen = Vec::new();
    let mut remember = |store: &Store| {
        seen.push((
            store.committed_pages(),
            store.load(NS, "contact", "c-1", &acl).ok(),
        ));
    };
    store.append(contact("eu", "Ada")).unwrap();
    remember(store);
    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([("note".into(), "vip".into())]),
                ..Default::default()
            },
            "act-note".into(),
            None,
        )
        .unwrap();
    remember(store);
    store.append(contact("eu", "Ada L.")).unwrap();
    remember(store);
    store
        .apply_action(
            Action {
                id: "act-move".into(),
                namespace: NS.into(),
                kind: "contact".into(),
                key: "c-1".into(),
                props: HashMap::from([
                    ("region".into(), "us".into()),
                    ("name".into(), "Ada L.".into()),
                ]),
            },
            None,
        )
        .unwrap();
    remember(store);
    store.hide(NS, "contact", "c-1", &acl).unwrap();
    remember(store);
    store.append(contact("eu", "Ada")).unwrap();
    remember(store);
    seen
}

#[test]
fn history_lists_every_generation_with_its_provenance() {
    let (dir, log) = temp_log("history-provenance");
    let mut store = Store::create(&log).unwrap();
    lifecycle(&mut store);
    let history = store
        .history(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    let summary: Vec<_> = history
        .iter()
        .map(|v| {
            (
                v.record.gen,
                v.origin,
                v.record.action_id.as_deref(),
                v.record.props.get("name").map(String::as_str),
                v.record.props.get("note").map(String::as_str),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (1, VersionOrigin::Source, None, Some("Ada"), None),
            (
                2,
                VersionOrigin::Edit,
                Some("act-note"),
                Some("Ada"),
                Some("vip")
            ),
            // A refresh merges the overlay and keeps its Action id.
            (
                3,
                VersionOrigin::Source,
                Some("act-note"),
                Some("Ada L."),
                Some("vip")
            ),
            (
                4,
                VersionOrigin::Edit,
                Some("act-move"),
                Some("Ada L."),
                None
            ),
            (
                5,
                VersionOrigin::Hide,
                Some("act-move"),
                Some("Ada L."),
                None
            ),
            (6, VersionOrigin::Source, None, Some("Ada"), None),
        ]
    );
    assert!(history.windows(2).all(|pair| pair[0].page < pair[1].page));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn as_of_answers_equal_load_at_each_commit() {
    let (dir, log) = temp_log("history-as-of");
    let mut store = Store::create(&log).unwrap();
    let seen = lifecycle(&mut store);
    drop(store);
    let store = Store::open(&log).unwrap();
    let acl = PropertyAcl::allow_all();
    for (point, then) in seen {
        let then = then.expect("the identity existed at every commit");
        let at_point = store
            .load_as_of(NS, "contact", "c-1", &acl, AsOf::CommitPoint(point))
            .unwrap();
        assert_eq!(at_point.record, then, "commit point {point}");
        let at_gen = store
            .load_as_of(NS, "contact", "c-1", &acl, AsOf::Generation(then.gen))
            .unwrap();
        assert_eq!(at_gen.record, then, "generation {}", then.gen);
    }
    assert!(store
        .load_as_of(NS, "contact", "c-1", &acl, AsOf::Generation(0))
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restrictions_apply_to_every_historical_version() {
    let (dir, log) = temp_log("history-restriction");
    let mut store = Store::create(&log).unwrap();
    let seen = lifecycle(&mut store);

    let denied = PropertyAcl::deny_property("contact", "note");
    let history = store.history(NS, "contact", "c-1", &denied).unwrap();
    assert_eq!(history.len(), 6);
    assert!(history.iter().all(|v| !v.record.props.contains_key("note")));

    // Generation 4 moved the contact to `us`; an `eu`-only view never sees it.
    let mut eu = PropertyAcl::allow_all();
    eu.insert_row_filter("contact", "region", ["eu"]).unwrap();
    let gens: Vec<_> = store
        .history(NS, "contact", "c-1", &eu)
        .unwrap()
        .iter()
        .map(|v| v.record.gen)
        .collect();
    assert_eq!(gens, [1, 2, 3, 6]);
    let (moved_at, _) = seen[3];
    assert!(store
        .load_as_of(NS, "contact", "c-1", &eu, AsOf::CommitPoint(moved_at))
        .is_err());

    assert!(store
        .history(NS, "contact", "c-2", &PropertyAcl::allow_all())
        .is_err());
    assert!(store
        .history("other", "contact", "c-1", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn older_versions_read_under_current_names() {
    let (dir, log) = temp_log("history-rename");
    let mut store = Store::create(&log).unwrap();
    let v1 = SchemaDescriptor {
        kind: "contact".into(),
        properties: vec!["name".into(), "note".into(), "region".into()],
        required: vec!["name".into()],
        ..Default::default()
    };
    store.append(v1.to_record(NS).unwrap()).unwrap();
    store
        .append(rec(
            "contact",
            "c-1",
            false,
            &[("region", "eu"), ("name", "Ada"), ("note", "first")],
        ))
        .unwrap();
    let mut v2 = v1.clone();
    v2.properties = vec!["memo".into(), "name".into(), "region".into()];
    v2.migration.rename = vec![("note".into(), "memo".into())];
    store.append(v2.to_record(NS).unwrap()).unwrap();
    store
        .append(rec(
            "contact",
            "c-1",
            false,
            &[("region", "eu"), ("name", "Ada"), ("memo", "second")],
        ))
        .unwrap();

    let history = store
        .history(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        history.iter().map(|v| props(&v.record)).collect::<Vec<_>>(),
        [
            vec![
                ("memo".into(), "first".into()),
                ("name".into(), "Ada".into()),
                ("region".into(), "eu".into()),
            ],
            vec![
                ("memo".into(), "second".into()),
                ("name".into(), "Ada".into()),
                ("region".into(), "eu".into()),
            ],
        ]
    );
    assert_eq!(history[0].record.schema_version, 1);
    assert_eq!(history[1].record.schema_version, 2);

    // Denying the current name hides the value it had under its old name.
    let denied = PropertyAcl::deny_property("contact", "memo");
    let history = store.history(NS, "contact", "c-1", &denied).unwrap();
    assert!(history
        .iter()
        .all(|v| !v.record.props.contains_key("memo") && !v.record.props.contains_key("note")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn uncommitted_writes_are_not_history() {
    let (dir, log) = temp_log("history-uncommitted");
    let mut store = Store::create(&log).unwrap();
    store.append(contact("eu", "Ada")).unwrap();
    store.append_uncommitted(contact("eu", "Ada L.")).unwrap();
    let history = store
        .history(NS, "contact", "c-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].record.gen, 1);
    let _ = std::fs::remove_dir_all(&dir);
}
