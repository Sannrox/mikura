use super::super::*;
use super::helpers::*;

fn incident_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "incident".into(),
        properties: vec!["affects".into(), "name".into(), "note".into()],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "affects".into(),
            far_kind: "component".into(),
            outgoing: true,
        }],
        sums: Vec::new(),
        types: Vec::new(),
    }
}

fn seed_incident(store: &mut Store) {
    store
        .append(incident_schema().to_record().unwrap())
        .unwrap();
    store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("note", "acked"),
            ],
        ))
        .unwrap();
}

#[test]
fn compatible_replacements_preserve_load_and_rebuild() {
    let (dir, log) = temp_log("schema-evolve-allow");
    let mut store = Store::create(&log).unwrap();
    seed_incident(&mut store);

    let mut evolved = incident_schema();
    evolved.properties.push("open".into());
    evolved.properties.push("priority".into());
    evolved.required.push("priority".into());
    evolved.types = vec![
        ("open".into(), PropertyType::Boolean),
        ("priority".into(), PropertyType::Integer),
    ];
    evolved.sums = vec!["name".into()];
    evolved.links.push(SchemaLink {
        name: "opened_by".into(),
        far_kind: "component".into(),
        outgoing: false,
    });
    store.append(evolved.to_record().unwrap()).unwrap();

    let historical = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        historical.props.get("note").map(String::as_str),
        Some("acked")
    );
    assert!(!historical.props.contains_key("open"));
    let missing_required = store
        .append(rec(
            "incident",
            "inc-2",
            false,
            &[("name", "disk full"), ("affects", "svc-api")],
        ))
        .unwrap_err();
    assert!(
        missing_required.contains("missing required"),
        "{missing_required}"
    );

    evolved.required.retain(|name| name != "priority");
    store.append(evolved.to_record().unwrap()).unwrap();
    store
        .append(rec(
            "incident",
            "inc-2",
            false,
            &[
                ("name", "disk full"),
                ("affects", "svc-api"),
                ("open", "true"),
            ],
        ))
        .unwrap();

    let mut shrunk = evolved.clone();
    shrunk.properties.retain(|name| name != "note");
    store.append(shrunk.to_record().unwrap()).unwrap();
    let still_stored = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        still_stored.props.get("note").map(String::as_str),
        Some("acked")
    );
    let unknown = store
        .append(rec(
            "incident",
            "inc-1",
            false,
            &[
                ("name", "elevated latency"),
                ("affects", "svc-api"),
                ("note", "acked"),
            ],
        ))
        .unwrap_err();
    assert!(unknown.contains("unknown property"), "{unknown}");
    drop(store);

    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened.schema("incident").unwrap().unwrap().properties,
        shrunk.properties
    );
    assert_eq!(
        reopened
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("note")
            .map(String::as_str),
        Some("acked")
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    assert_eq!(replayed.schema("incident").unwrap().unwrap(), shrunk);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn incompatible_replacements_leave_previous_state() {
    let (dir, log) = temp_log("schema-evolve-reject");
    let mut store = Store::create(&log).unwrap();
    let (component_schema, incident_schema) = product_loop_schemas();
    store.append(component_schema.to_record().unwrap()).unwrap();
    store.append(incident_schema.to_record().unwrap()).unwrap();
    append_all(&mut store, product_loop_seed());

    let mut recast = incident_schema.clone();
    recast.types = vec![("name".into(), PropertyType::Integer)];
    let recast_err = store.append(recast.to_record().unwrap()).unwrap_err();
    assert!(recast_err.contains("cannot recast name"), "{recast_err}");

    let mut retarget = incident_schema.clone();
    retarget.links[0].far_kind = "service".into();
    let link_err = store.append(retarget.to_record().unwrap()).unwrap_err();
    assert!(
        link_err.contains("cannot retarget outgoing link affects"),
        "{link_err}"
    );

    assert_eq!(store.schema("incident").unwrap().unwrap(), incident_schema);
    assert_eq!(
        store
            .load("incident", "inc-1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("name")
            .map(String::as_str),
        Some("elevated latency")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overlay_and_action_follow_current_descriptor() {
    let (dir, log) = temp_log("schema-evolve-writeback");
    let mut store = Store::create(&log).unwrap();
    seed_incident(&mut store);
    store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "paged".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-note".into(),
            None,
        )
        .unwrap();
    store
        .apply_action(
            Action {
                id: "act-inc-1-rename".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([
                    ("name".into(), "elevated latency".into()),
                    ("affects".into(), "svc-api".into()),
                    ("note".into(), "paged".into()),
                ]),
            },
            None,
        )
        .unwrap();

    let mut shrunk = incident_schema();
    shrunk.properties.retain(|name| name != "note");
    store.append(shrunk.to_record().unwrap()).unwrap();

    let overlay_err = store
        .apply_overlay(
            OverlayPatch {
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([("note".into(), "stale".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-inc-1-note-2".into(),
            None,
        )
        .unwrap_err();
    assert!(overlay_err.contains("unknown property"), "{overlay_err}");

    let action_err = store
        .apply_action(
            Action {
                id: "act-inc-1-note-keep".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([
                    ("name".into(), "elevated latency".into()),
                    ("affects".into(), "svc-api".into()),
                    ("note".into(), "paged".into()),
                ]),
            },
            None,
        )
        .unwrap_err();
    assert!(action_err.contains("unknown property"), "{action_err}");

    store
        .apply_action(
            Action {
                id: "act-inc-1-rename".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([
                    ("name".into(), "elevated latency".into()),
                    ("affects".into(), "svc-api".into()),
                    ("note".into(), "paged".into()),
                ]),
            },
            None,
        )
        .unwrap();
    store
        .apply_action(
            Action {
                id: "act-inc-1-current".into(),
                kind: "incident".into(),
                key: "inc-1".into(),
                props: HashMap::from([
                    ("name".into(), "elevated latency".into()),
                    ("affects".into(), "svc-api".into()),
                ]),
            },
            None,
        )
        .unwrap();
    let current = store
        .load("incident", "inc-1", &PropertyAcl::allow_all())
        .unwrap();
    assert!(!current.props.contains_key("note"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hide_schema_then_restore_previous_body() {
    let (dir, log) = temp_log("schema-evolve-hide");
    let mut store = Store::create(&log).unwrap();
    seed_incident(&mut store);
    let mut hidden = incident_schema().to_record().unwrap();
    hidden.hidden = true;
    store.append(hidden).unwrap();
    assert!(store.schema("incident").unwrap().is_none());
    store
        .append(rec("incident", "inc-2", false, &[("alias", "unvalidated")]))
        .unwrap();
    store
        .append(incident_schema().to_record().unwrap())
        .unwrap();
    assert_eq!(
        store.schema("incident").unwrap().unwrap(),
        incident_schema()
    );
    let extra = store
        .append(rec(
            "incident",
            "inc-3",
            false,
            &[("name", "n"), ("alias", "x")],
        ))
        .unwrap_err();
    assert!(extra.contains("unknown property"), "{extra}");
    let _ = std::fs::remove_dir_all(&dir);
}
