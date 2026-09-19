use super::super::*;
use super::helpers::*;

fn incident_typed_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec![
            "affects".into(),
            "cost".into(),
            "name".into(),
            "open".into(),
            "opened_at".into(),
            "priority".into(),
        ],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: vec![
            ("cost".into(), PropertyType::Decimal { scale: 2 }),
            ("open".into(), PropertyType::Boolean),
            ("opened_at".into(), PropertyType::Timestamp),
            ("priority".into(), PropertyType::Integer),
        ],
    }
}

fn typed_incident(props: &[(&str, &str)]) -> ObjectRecord {
    rec("incident", "inc-1", false, props)
}

#[test]
fn typed_scalars_round_trip_overlay_deny_and_rebuild() {
    let (dir, log) = temp_log("typed-values");
    let mut store = Store::create(&log).unwrap();
    store
        .append(rec(
            "incident",
            "legacy",
            false,
            &[("name", "old"), ("alias", "pre-schema")],
        ))
        .unwrap();
    let schema = incident_typed_schema();
    store.append(schema.to_record().unwrap()).unwrap();
    store
        .append(typed_incident(&[
            ("name", "elevated latency"),
            ("affects", "svc-api"),
            ("open", "true"),
            ("priority", "9007199254740993"),
            ("opened_at", "2026-09-19T18:00:00+01:00"),
            ("cost", "1500.00"),
        ]))
        .unwrap();

    let loaded = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(loaded.props.get("open").map(String::as_str), Some("true"));
    assert_eq!(
        loaded.props.get("priority").map(String::as_str),
        Some("9007199254740993")
    );
    assert_eq!(
        loaded.props.get("opened_at").map(String::as_str),
        Some("2026-09-19T17:00:00.000Z")
    );
    assert_eq!(
        loaded.props.get("cost").map(String::as_str),
        Some("1500.00")
    );

    let overflow = store
        .append(typed_incident(&[
            ("name", "n"),
            ("priority", "9223372036854775808"),
        ]))
        .unwrap_err();
    assert!(
        overflow.contains("overflows i64") || overflow.contains("not canonical"),
        "{overflow}"
    );
    let bad_bool = store
        .append(typed_incident(&[("name", "n"), ("open", "TRUE")]))
        .unwrap_err();
    assert!(bad_bool.contains("invalid boolean"), "{bad_bool}");
    let empty = store
        .append(typed_incident(&[("name", "n"), ("cost", "")]))
        .unwrap_err();
    assert!(empty.contains("decimal"), "{empty}");

    store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("open".into(), "false".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-open".into(),
            None,
        )
        .unwrap();
    let overlayed = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        overlayed.props.get("open").map(String::as_str),
        Some("false")
    );
    assert_eq!(
        overlayed.props.get("cost").map(String::as_str),
        Some("1500.00")
    );

    let denied = store
        .load(
            "incident",
            "inc-1",
            &PropertyAcl::deny_property("incident", "cost"),
        )
        .unwrap();
    assert!(!denied.props.contains_key("cost"));
    assert_ne!(denied.props.get("cost"), Some(&String::new()));
    assert_eq!(denied.props.get("open").map(String::as_str), Some("false"));

    let historical = store
        .load("incident", "legacy", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        historical.props.get("alias").map(String::as_str),
        Some("pre-schema")
    );
    drop(store);

    let reopened = Store::open(&log).unwrap();
    let again = reopened
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        again.props.get("opened_at").map(String::as_str),
        Some("2026-09-19T17:00:00.000Z")
    );
    assert_eq!(again.props.get("open").map(String::as_str), Some("false"));
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    let rebuilt = replayed
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(rebuilt.props, again.props);
    assert_eq!(
        replayed.schema("incident").unwrap().unwrap().types,
        incident_typed_schema().types
    );
    let _ = std::fs::remove_dir_all(&dir);
}
