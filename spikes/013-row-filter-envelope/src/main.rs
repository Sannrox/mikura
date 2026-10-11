//! Two-hop count + sum with a restriction row filter (ADR 0029) at scale.
//! Throwaway. Same Customer→Order→Shipment ratios as spike 011; `region`
//! spreads customers over 100 values so `region = r7` keeps 1 %.

use mikura::{
    Aggregate, EvaluateRequest, Hop, LocalCompute, ObjectRecord, ObjectSet, Predicate, PropertyAcl,
    Store,
};
use mikura_ingest::BatchIngest;
use std::time::Instant;

const NS: &str = "bench";
const CHUNK: usize = 1_000_000;

fn rec(kind: &str, key: String, props: Vec<(&str, String)>) -> ObjectRecord {
    ObjectRecord {
        gen: 1,
        namespace: NS.into(),
        kind: kind.into(),
        key,
        hidden: false,
        action_id: None,
        props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn region(id: i64) -> String {
    format!("r{}", id % 100)
}

fn ingest(store: &mut Store, objects: i64) -> Result<(), String> {
    let customers = objects / 100;
    let orders = objects / 10;
    let shipments = objects - customers - orders;
    let mut batch = Vec::with_capacity(CHUNK);
    let flush = |store: &mut Store, batch: &mut Vec<ObjectRecord>, force: bool| {
        if batch.len() >= CHUNK || (force && !batch.is_empty()) {
            BatchIngest::run(store, std::mem::take(batch))?;
        }
        Ok::<_, String>(())
    };
    for id in 0..customers {
        batch.push(rec(
            "Customer",
            format!("c{id}"),
            vec![("region", region(id))],
        ));
        flush(store, &mut batch, false)?;
    }
    for id in 0..orders {
        let customer = format!("c{}", id % customers);
        batch.push(rec(
            "Order",
            format!("o{id}"),
            vec![("customer_id", customer)],
        ));
        flush(store, &mut batch, false)?;
    }
    for id in 0..shipments {
        batch.push(rec(
            "Shipment",
            format!("s{id}"),
            vec![
                ("order_id", format!("o{}", id % orders)),
                ("amount", format!("{}", (id % 50) + 1)),
                ("region", region(id)),
            ],
        ));
        flush(store, &mut batch, false)?;
    }
    flush(store, &mut batch, true)
}

fn two_hop(acl: PropertyAcl, predicate: Option<Predicate>) -> EvaluateRequest {
    let hop = |far: &str, join: &str| Hop {
        far_kind: far.into(),
        join_property: join.into(),
        incoming: false,
        predicate: None,
    };
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "Customer".into(),
        hops: vec![hop("Order", "customer_id"), hop("Shipment", "order_id")],
        sum_kind: "Shipment".into(),
        sum_property: "amount".into(),
        aggregate: Aggregate::CountAndSum,
        acl,
        predicate,
        ..EvaluateRequest::default()
    }
}

fn row_filter(kind: &str) -> PropertyAcl {
    let mut acl = PropertyAcl::allow_all();
    acl.insert_row_filter(kind, "region", ["r7"]).unwrap();
    acl
}

fn timed(store: &Store, label: &str, request: &EvaluateRequest) -> (usize, i64) {
    let oss = ObjectSet::new(LocalCompute);
    let t = Instant::now();
    let out = oss.evaluate(store, request).expect(label);
    println!(
        "{label}: {} ms two_hop={} sum={}",
        t.elapsed().as_millis(),
        out.two_hop_count,
        out.sum_amount
    );
    (out.two_hop_count, out.sum_amount)
}

fn main() {
    let objects: i64 = std::env::args()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or(10_000);
    let dir = std::env::temp_dir().join(format!("mikura-row-filter-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = Store::create(&dir.join("objects.mikura")).unwrap();
    let t = Instant::now();
    ingest(&mut store, objects).unwrap();
    println!("objects={objects} ingest: {} ms", t.elapsed().as_millis());

    timed(
        &store,
        "open two-hop",
        &two_hop(PropertyAcl::allow_all(), None),
    );
    let filtered = timed(
        &store,
        "root row filter",
        &two_hop(row_filter("Customer"), None),
    );
    let oracle = timed(
        &store,
        "root predicate (oracle)",
        &two_hop(
            PropertyAcl::allow_all(),
            Some(Predicate::eq("region", "r7")),
        ),
    );
    assert_eq!(filtered, oracle, "row filter must equal the root predicate");
    timed(
        &store,
        "hop-target row filter",
        &two_hop(row_filter("Shipment"), None),
    );
    let _ = std::fs::remove_dir_all(&dir);
}
