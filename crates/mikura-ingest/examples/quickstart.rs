//! Ingest a tiny graph, evaluate two hops, apply an Action, reopen the log.

use mikura::{
    Action, Aggregate, EvaluateRequest, Hop, LocalCompute, ObjectRecord, ObjectSet, PropertyAcl,
    Store,
};
use mikura_ingest::BatchIngest;
use std::collections::HashMap;

fn rec(kind: &str, key: &str, hidden: bool, props: &[(&str, &str)]) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        kind: kind.into(),
        key: key.into(),
        hidden,
        action_id: None,
        props: props
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into()))
            .collect(),
    }
}

fn request() -> EvaluateRequest {
    EvaluateRequest {
        root_kind: "Customer".into(),
        hops: vec![
            Hop {
                far_kind: "Order".into(),
                join_property: "customer_id".into(),
                incoming: false,
            },
            Hop {
                far_kind: "Shipment".into(),
                join_property: "order_id".into(),
                incoming: false,
            },
        ],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
    }
}

fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join("mikura-quickstart");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let log = dir.join("objects.mikura");

    let mut store = Store::create(&log)?;
    BatchIngest::run(
        &mut store,
        vec![
            rec("Customer", "c0", true, &[("region", "eu")]),
            rec("Customer", "c1", false, &[("region", "us")]),
            rec("Order", "o1", false, &[("customer_id", "c1")]),
            rec("Order", "o0", true, &[("customer_id", "c0")]),
            rec(
                "Shipment",
                "s1",
                false,
                &[("order_id", "o1"), ("amount", "10")],
            ),
        ],
    )?;

    let objects = ObjectSet::new(LocalCompute);
    let visible = objects
        .evaluate(&store, &request())
        .map_err(|err| format!("{err:?}"))?;
    println!("reachable roots: {}", visible.two_hop_count);
    println!("sum amount: {}", visible.sum_amount);

    store.apply_action(Action {
        id: "act-s2".into(),
        kind: "Shipment".into(),
        key: "s2".into(),
        props: HashMap::from([
            ("order_id".into(), "o1".into()),
            ("amount".into(), "5".into()),
        ]),
    })?;
    let after = objects
        .evaluate(&store, &request())
        .map_err(|err| format!("{err:?}"))?;
    println!("after action: {}", after.sum_amount);

    let reopened = Store::open(&log)?;
    let from_log = objects
        .evaluate(&reopened, &request())
        .map_err(|err| format!("{err:?}"))?;
    assert_eq!(from_log.sum_amount, after.sum_amount);
    println!("reopened log matches");

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
