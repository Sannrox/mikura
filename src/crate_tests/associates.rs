use super::super::*;
use super::helpers::*;
use std::collections::HashMap;

fn label_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "label".into(),
        properties: vec!["name".into()],
        required: vec!["name".into()],
        links: vec![SchemaLink {
            name: "label".into(),
            far_kind: "incident_label".into(),
            outgoing: false,
        }],
        sums: Vec::new(),
        types: Vec::new(),
    }
}

fn incident_label_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "incident_label".into(),
        properties: vec!["incident".into(), "label".into()],
        required: vec!["incident".into(), "label".into()],
        links: vec![
            SchemaLink {
                name: "incident".into(),
                far_kind: "incident".into(),
                outgoing: true,
            },
            SchemaLink {
                name: "label".into(),
                far_kind: "label".into(),
                outgoing: true,
            },
        ],
        sums: Vec::new(),
        types: Vec::new(),
    }
}

fn association_seed() -> Vec<ObjectRecord> {
    let mut records = product_loop_seed();
    records.extend(vec![
        rec("label", "sev-high", false, &[("name", "high")]),
        rec("label", "region-eu", false, &[("name", "eu")]),
        rec(
            "incident_label",
            "il-inc-1-sev-high",
            false,
            &[("incident", "inc-1"), ("label", "sev-high")],
        ),
        rec(
            "incident_label",
            "il-inc-1-region-eu",
            false,
            &[("incident", "inc-1"), ("label", "region-eu")],
        ),
    ]);
    records
}

fn hop_incident_to_labels(bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "incident".into(),
        hops: vec![
            Hop {
                far_kind: "incident_label".into(),
                join_property: "incident".into(),
                incoming: false,
                predicate: None,
            },
            Hop {
                far_kind: "label".into(),
                join_property: "label".into(),
                incoming: true,
                predicate: None,
            },
        ],
        sum_kind: "label".into(),
        sum_property: "name".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: bound,
    }
}

fn hop_label_to_incidents(bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "label".into(),
        hops: vec![
            Hop {
                far_kind: "incident_label".into(),
                join_property: "label".into(),
                incoming: false,
                predicate: None,
            },
            Hop {
                far_kind: "incident".into(),
                join_property: "incident".into(),
                incoming: true,
                predicate: None,
            },
        ],
        sum_kind: "incident".into(),
        sum_property: "name".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: bound,
    }
}

fn list_associations(bound: usize) -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "incident_label".into(),
        hops: vec![],
        sum_kind: "incident_label".into(),
        sum_property: "incident".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate: None,
        object_bound: bound,
    }
}

fn object_keys(response: &EvaluateResponse) -> Vec<String> {
    let mut keys: Vec<String> = response.objects.iter().map(|row| row.key.clone()).collect();
    keys.sort();
    keys
}

fn commit_association_fixture(store: &mut Store) {
    let (component, incident) = product_loop_schemas();
    store.append(component.to_record().unwrap()).unwrap();
    store.append(incident.to_record().unwrap()).unwrap();
    store.append(label_schema().to_record().unwrap()).unwrap();
    store
        .append(incident_label_schema().to_record().unwrap())
        .unwrap();
    append_all(store, association_seed());
}

#[test]
fn association_objects_traverse_hide_and_rebuild() {
    let (dir, log) = temp_log("associates");
    let mut store = Store::create(&log).unwrap();
    commit_association_fixture(&mut store);
    let oss = ObjectSet::new(LocalCompute);

    let labels = oss.evaluate(&store, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(object_keys(&labels), vec!["region-eu", "sev-high"]);
    assert_eq!(labels.two_hop_count, 1);

    let reverse = oss.evaluate(&store, &hop_label_to_incidents(8)).unwrap();
    assert_eq!(object_keys(&reverse), vec!["inc-1"]);

    let affects = oss.evaluate(&store, &hop_incident_to_component(8)).unwrap();
    assert_eq!(object_keys(&affects), vec!["svc-api"]);

    store
        .append(rec(
            "incident_label",
            "il-inc-1-sev-high-dup",
            false,
            &[("incident", "inc-1"), ("label", "sev-high")],
        ))
        .unwrap();
    let listed = oss.evaluate(&store, &list_associations(8)).unwrap();
    assert_eq!(
        object_keys(&listed),
        vec![
            "il-inc-1-region-eu",
            "il-inc-1-sev-high",
            "il-inc-1-sev-high-dup"
        ]
    );
    let labels_after_dup = oss.evaluate(&store, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(
        object_keys(&labels_after_dup),
        vec!["region-eu", "sev-high"]
    );

    store
        .append(rec(
            "incident_label",
            "il-dangling",
            false,
            &[("incident", "inc-1"), ("label", "missing-label")],
        ))
        .unwrap();
    let dangling_load = store
        .load("incident_label", "il-dangling", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        dangling_load.props.get("label").map(String::as_str),
        Some("missing-label")
    );
    let labels_dangling = oss.evaluate(&store, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(object_keys(&labels_dangling), vec!["region-eu", "sev-high"]);

    store
        .hide(
            "incident_label",
            "il-inc-1-region-eu",
            &PropertyAcl::allow_all(),
        )
        .unwrap();
    let after_hide_assoc = oss.evaluate(&store, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(object_keys(&after_hide_assoc), vec!["sev-high"]);
    assert!(
        store
            .load(
                "incident_label",
                "il-inc-1-region-eu",
                &PropertyAcl::allow_all(),
            )
            .unwrap()
            .hidden
    );

    store
        .hide("label", "region-eu", &PropertyAcl::allow_all())
        .unwrap();
    let after_hide_label = oss.evaluate(&store, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(object_keys(&after_hide_label), vec!["sev-high"]);
    assert!(
        store
            .load("label", "region-eu", &PropertyAcl::allow_all())
            .unwrap()
            .hidden
    );

    store
        .append(rec("label", "region-eu", false, &[("name", "eu")]))
        .unwrap();
    let after_recreate = oss.evaluate(&store, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(object_keys(&after_recreate), vec!["sev-high"]);

    let mut denied = hop_incident_to_labels(8);
    denied.acl = PropertyAcl::deny_property("incident_label", "label");
    assert!(matches!(
        oss.evaluate(&store, &denied),
        Err(ComputeError::Acl(AclError::Denied { .. }))
    ));

    store
        .apply_overlay(
            OverlayPatch {
                kind: "incident_label".into(),
                key: "il-inc-1-sev-high".into(),
                props: HashMap::from([("label".into(), "region-eu".into())]),
                cleared: Vec::new(),
                action_id: None,
            },
            "act-il-relabel".into(),
            Some(1),
        )
        .unwrap();
    store
        .append(rec(
            "incident_label",
            "il-inc-1-sev-high",
            false,
            &[("incident", "inc-1"), ("label", "sev-high")],
        ))
        .unwrap();
    let overlaid = store
        .load(
            "incident_label",
            "il-inc-1-sev-high",
            &PropertyAcl::allow_all(),
        )
        .unwrap();
    assert_eq!(
        overlaid.props.get("label").map(String::as_str),
        Some("region-eu")
    );
    let after_overlay = oss.evaluate(&store, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(object_keys(&after_overlay), vec!["region-eu", "sev-high"]);

    store
        .apply_action(
            Action {
                id: "act-il-replay".into(),
                kind: "incident_label".into(),
                key: "il-inc-1-sev-high-dup".into(),
                props: HashMap::from([
                    ("incident".into(), "inc-1".into()),
                    ("label".into(), "sev-high".into()),
                ]),
            },
            None,
        )
        .unwrap();
    store
        .apply_action(
            Action {
                id: "act-il-replay".into(),
                kind: "incident_label".into(),
                key: "il-inc-1-sev-high-dup".into(),
                props: HashMap::from([
                    ("incident".into(), "inc-1".into()),
                    ("label".into(), "sev-high".into()),
                ]),
            },
            None,
        )
        .unwrap();
    let replayed_row = store
        .load(
            "incident_label",
            "il-inc-1-sev-high-dup",
            &PropertyAcl::allow_all(),
        )
        .unwrap();
    assert_eq!(replayed_row.action_id.as_deref(), Some("act-il-replay"));

    let missing = store
        .append(rec(
            "incident_label",
            "il-incomplete",
            false,
            &[("incident", "inc-1")],
        ))
        .unwrap_err();
    assert!(missing.contains("missing required"), "{missing}");

    drop(store);
    let reopened = Store::open(&log).unwrap();
    let reopened_labels = oss.evaluate(&reopened, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(object_keys(&reopened_labels), vec!["region-eu", "sev-high"]);
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let rebuilt = Store::open(&log).unwrap();
    let rebuilt_labels = oss.evaluate(&rebuilt, &hop_incident_to_labels(8)).unwrap();
    assert_eq!(object_keys(&rebuilt_labels), vec!["region-eu", "sev-high"]);
    assert_eq!(
        object_keys(
            &oss.evaluate(&rebuilt, &hop_incident_to_component(8))
                .unwrap()
        ),
        vec!["svc-api"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
