use super::super::*;
use super::helpers::*;

fn sample_schema() -> SchemaDescriptor {
    SchemaDescriptor {
        kind: "sample".into(),
        properties: vec![
            "embedding".into(),
            "file".into(),
            "location".into(),
            "name".into(),
            "region".into(),
            "series".into(),
        ],
        required: vec!["name".into()],
        links: Vec::new(),
        sums: Vec::new(),
        types: vec![
            ("embedding".into(), PropertyType::Vector { dim: 2 }),
            ("file".into(), PropertyType::Media),
            ("location".into(), PropertyType::GeoPoint),
            ("region".into(), PropertyType::GeoShape),
            ("series".into(), PropertyType::TimeSeries),
        ],
        ..Default::default()
    }
}

fn sample(key: &str, props: &[(&str, &str)]) -> ObjectRecord {
    rec("sample", key, false, props)
}

fn list_samples(
    predicate: Option<Predicate>,
    sort: Option<Sort>,
    object_bound: usize,
    page_size: usize,
) -> EvaluateRequest {
    EvaluateRequest {
        namespace: NS.into(),
        root_kind: "sample".into(),
        hops: vec![],
        sum_kind: "sample".into(),
        sum_property: "name".into(),
        aggregate: Aggregate::CountAndSum,
        acl: PropertyAcl::allow_all(),
        filter: None,
        predicate,
        object_bound,
        sort,
        page_size,
        cursor: None,
        breakdown: None,
        then_by: Vec::new(),
    }
}

fn keys(response: &EvaluateResponse) -> Vec<&str> {
    response
        .objects
        .iter()
        .map(|row| row.key.as_str())
        .collect()
}

fn seed(store: &mut Store) {
    store
        .append(sample_schema().to_record("ns").unwrap())
        .unwrap();
    store
        .append(sample(
            "s1",
            &[
                ("name", "origin"),
                ("embedding", "0,0"),
                (
                    "series",
                    "sensor:1 2026-09-19T17:00:00.000Z 2026-09-19T18:00:00.000Z",
                ),
                ("location", "51.5,-0.1"),
                ("region", "shape:1 51,-1,52,0"),
                ("file", "blob:one"),
            ],
        ))
        .unwrap();
    store
        .append(sample(
            "s2",
            &[
                ("name", "far"),
                ("embedding", "3,4"),
                (
                    "series",
                    "sensor:2 2026-09-19T19:00:00.000Z 2026-09-19T20:00:00.000Z",
                ),
                ("location", "0,0"),
                ("region", "shape:2 0,0,1,1"),
                ("file", "blob:two"),
            ],
        ))
        .unwrap();
    store
        .append(sample(
            "s3",
            &[
                ("name", "near"),
                ("embedding", "1,0"),
                (
                    "series",
                    "sensor:3 2026-09-19T17:30:00.000Z 2026-09-19T17:45:00.000Z",
                ),
                ("location", "51.45,-0.08"),
                ("region", "shape:3 51.4,-0.2,51.6,0"),
                ("file", "blob:three"),
            ],
        ))
        .unwrap();
    store
        .append(sample("s4", &[("name", "no-vector")]))
        .unwrap();
}

#[test]
fn typed_refs_round_trip_validate_and_query() {
    let (dir, log) = temp_log("typed-refs");
    let mut store = Store::create(&log).unwrap();
    seed(&mut store);

    let loaded = store
        .load(NS, "sample", "s1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        loaded.props.get("embedding").map(String::as_str),
        Some("0,0")
    );
    assert_eq!(
        loaded.props.get("series").map(String::as_str),
        Some("sensor:1 2026-09-19T17:00:00.000Z 2026-09-19T18:00:00.000Z")
    );
    assert_eq!(
        loaded.props.get("location").map(String::as_str),
        Some("51.5,-0.1")
    );
    assert_eq!(
        loaded.props.get("region").map(String::as_str),
        Some("shape:1 51,-1,52,0")
    );
    assert_eq!(
        loaded.props.get("file").map(String::as_str),
        Some("blob:one")
    );

    let dim = store
        .append(sample("bad-dim", &[("name", "n"), ("embedding", "1")]))
        .unwrap_err();
    assert!(dim.contains("vector wants 2"), "{dim}");
    let trailing = store
        .append(sample("bad-vec", &[("name", "n"), ("embedding", "1.0,0")]))
        .unwrap_err();
    assert!(trailing.contains("not canonical"), "{trailing}");
    let inverted = store
        .append(sample(
            "bad-ts",
            &[
                ("name", "n"),
                (
                    "series",
                    "sensor:x 2026-09-19T18:00:00.000Z 2026-09-19T17:00:00.000Z",
                ),
            ],
        ))
        .unwrap_err();
    assert!(inverted.contains("start is after end"), "{inverted}");
    let lat = store
        .append(sample("bad-lat", &[("name", "n"), ("location", "91,0")]))
        .unwrap_err();
    assert!(lat.contains("out of range"), "{lat}");
    let shape = store
        .append(sample(
            "bad-shape",
            &[("name", "n"), ("region", "shape:x 52,-1,51,0")],
        ))
        .unwrap_err();
    assert!(shape.contains("min must not exceed max"), "{shape}");
    let media = store
        .append(sample("bad-media", &[("name", "n"), ("file", "a b")]))
        .unwrap_err();
    assert!(media.contains("comma or whitespace"), "{media}");
    let payload = store
        .append(sample(
            "payload",
            &[("name", "n"), ("file", &"x".repeat(2049))],
        ))
        .unwrap_err();
    assert!(payload.contains("2048"), "{payload}");

    store
        .apply_overlay(
            OverlayPatch {
                namespace: NS.into(),
                kind: "sample".into(),
                key: "s2".into(),
                props: HashMap::from([("embedding".into(), "6,8".into())]),
                cleared: Vec::new(),
                action_id: None,
                ..Default::default()
            },
            "act-s2-embed".into(),
            None,
        )
        .unwrap();
    let overlayed = store
        .load(NS, "sample", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        overlayed.props.get("embedding").map(String::as_str),
        Some("6,8")
    );
    assert_eq!(
        overlayed.props.get("file").map(String::as_str),
        Some("blob:two")
    );

    let oss = ObjectSet::new(LocalCompute);
    let nearest = oss
        .evaluate(
            &store,
            &list_samples(None, Some(Sort::nearest("embedding", "0,0")), 8, 2),
        )
        .unwrap();
    assert_eq!(keys(&nearest), ["s1", "s3"]);
    assert!(nearest.cursor.is_none());
    let all_nn = oss
        .evaluate(
            &store,
            &list_samples(None, Some(Sort::nearest("embedding", "0,0")), 8, 8),
        )
        .unwrap();
    assert_eq!(keys(&all_nn), ["s1", "s3", "s2"]);
    let no_k = oss
        .evaluate(
            &store,
            &EvaluateRequest {
                page_size: 0,
                ..list_samples(None, Some(Sort::nearest("embedding", "0,0")), 8, 2)
            },
        )
        .unwrap_err();
    assert!(
        matches!(no_k, ComputeError::Page(ref msg) if msg.contains("page_size")),
        "{no_k:?}"
    );

    let range = oss
        .evaluate(
            &store,
            &list_samples(
                Some(Predicate::range(
                    "series",
                    Some("2026-09-19T17:15:00.000Z"),
                    Some("2026-09-19T17:40:00.000Z"),
                )),
                None,
                8,
                0,
            ),
        )
        .unwrap();
    let mut range_keys = keys(&range);
    range_keys.sort();
    assert_eq!(range_keys, ["s1", "s3"]);

    let bbox = oss
        .evaluate(
            &store,
            &list_samples(
                Some(Predicate::bbox("location", "51", "-1", "52", "0")),
                None,
                8,
                0,
            ),
        )
        .unwrap();
    let mut bbox_keys = keys(&bbox);
    bbox_keys.sort();
    assert_eq!(bbox_keys, ["s1", "s3"]);

    let envelope = oss
        .evaluate(
            &store,
            &list_samples(
                Some(Predicate::bbox("region", "51.4", "-0.2", "51.6", "0")),
                None,
                8,
                0,
            ),
        )
        .unwrap();
    let mut envelope_keys = keys(&envelope);
    envelope_keys.sort();
    assert_eq!(envelope_keys, ["s1", "s3"]);

    let media_eq = oss
        .evaluate(
            &store,
            &list_samples(Some(Predicate::eq("file", "blob:one")), None, 8, 0),
        )
        .unwrap();
    assert_eq!(keys(&media_eq), ["s1"]);

    let huge = format!("{},0", "1".to_string() + &"0".repeat(200));
    store
        .append(sample("huge", &[("name", "huge"), ("embedding", &huge)]))
        .unwrap();
    let overflow = oss
        .evaluate(
            &store,
            &list_samples(None, Some(Sort::nearest("embedding", "0,0")), 8, 2),
        )
        .unwrap_err();
    assert!(
        matches!(overflow, ComputeError::Page(ref msg) if msg.contains("not finite")),
        "{overflow:?}"
    );

    let vector_sort = oss
        .evaluate(
            &store,
            &list_samples(None, Some(Sort::by("embedding")), 8, 8),
        )
        .unwrap_err();
    assert!(
        matches!(vector_sort, ComputeError::Page(ref msg) if msg.contains("nearest")),
        "{vector_sort:?}"
    );
    let bbox_on_name = oss
        .evaluate(
            &store,
            &list_samples(
                Some(Predicate::bbox("name", "0", "0", "1", "1")),
                None,
                8,
                0,
            ),
        )
        .unwrap_err();
    assert!(
        matches!(bbox_on_name, ComputeError::Predicate(ref msg) if msg.contains("bbox")),
        "{bbox_on_name:?}"
    );
    let range_on_vec = oss
        .evaluate(
            &store,
            &list_samples(
                Some(Predicate::range("embedding", Some("0,0"), Some("1,0"))),
                None,
                8,
                0,
            ),
        )
        .unwrap_err();
    assert!(
        matches!(range_on_vec, ComputeError::Predicate(ref msg) if msg.contains("range")),
        "{range_on_vec:?}"
    );

    drop(store);
    let reopened = Store::open(&log).unwrap();
    let again = reopened
        .load(NS, "sample", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        again.props.get("embedding").map(String::as_str),
        Some("6,8")
    );
    drop(reopened);
    std::fs::remove_file(Store::join_map_path(&log)).unwrap();
    let replayed = Store::open(&log).unwrap();
    let rebuilt = replayed
        .load(NS, "sample", "s2", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(rebuilt.props, again.props);
    assert_eq!(
        replayed.schema(NS, "sample").unwrap().unwrap().types,
        sample_schema().types
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn max_dim_zero_vector_writes() {
    let (dir, log) = temp_log("max-dim-vector");
    let mut store = Store::create(&log).unwrap();
    let schema = SchemaDescriptor {
        kind: "vec".into(),
        properties: vec!["embedding".into(), "name".into()],
        required: vec!["name".into()],
        types: vec![(
            "embedding".into(),
            PropertyType::Vector {
                dim: VECTOR_DIM_MAX,
            },
        )],
        ..Default::default()
    };
    store.append(schema.to_record(NS).unwrap()).unwrap();
    let zeros = vec!["0"; VECTOR_DIM_MAX as usize].join(",");
    store
        .append(rec(
            "vec",
            "v1",
            false,
            &[("name", "n"), ("embedding", &zeros)],
        ))
        .unwrap();
    let loaded = store
        .load(NS, "vec", "v1", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(
        loaded.props.get("embedding").map(String::as_str),
        Some(zeros.as_str())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Query cost on a 10^6 set; run with
/// `cargo test --release -p mikura --lib -- --ignored typed_query_envelope --nocapture`.
#[test]
#[ignore = "measurement, not a check"]
fn typed_query_envelope() {
    let (dir, log) = temp_log("typed-query-envelope");
    let mut store = Store::create(&log).unwrap();
    store
        .append(sample_schema().to_record("ns").unwrap())
        .unwrap();
    const N: usize = 1_000_000;
    for start in (0..N).step_by(50_000) {
        let end = (start + 50_000).min(N);
        let records: Vec<_> = (start..end)
            .map(|i| {
                let key = format!("k{i}");
                let embedding = if i == 0 { "0,0" } else { "10,0" };
                let series = if i == 0 {
                    "sensor:0 2026-01-01T00:00:00.000Z 2026-01-01T01:00:00.000Z"
                } else {
                    "sensor:x 2026-06-01T00:00:00.000Z 2026-06-01T01:00:00.000Z"
                };
                let location = if i == 0 { "0,0" } else { "10,10" };
                let region = if i == 0 {
                    "shape:0 0,0,1,1"
                } else {
                    "shape:x 9,9,11,11"
                };
                let file = if i == 0 { "blob:0" } else { "blob:x" };
                sample(
                    &key,
                    &[
                        ("name", &key),
                        ("embedding", embedding),
                        ("series", series),
                        ("location", location),
                        ("region", region),
                        ("file", file),
                    ],
                )
            })
            .collect();
        store.append_batch(records).unwrap();
    }
    let oss = ObjectSet::new(LocalCompute);
    let started = std::time::Instant::now();
    let nearest = oss
        .evaluate(
            &store,
            &list_samples(None, Some(Sort::nearest("embedding", "0,0")), N, 1),
        )
        .unwrap();
    let nn_ms = started.elapsed();
    assert_eq!(keys(&nearest), ["k0"]);
    let started = std::time::Instant::now();
    let range = oss
        .evaluate(
            &store,
            &list_samples(
                Some(Predicate::range(
                    "series",
                    Some("2026-01-01T00:30:00.000Z"),
                    Some("2026-01-01T00:45:00.000Z"),
                )),
                None,
                N,
                0,
            ),
        )
        .unwrap();
    let range_ms = started.elapsed();
    assert_eq!(keys(&range), ["k0"]);
    let started = std::time::Instant::now();
    let bbox = oss
        .evaluate(
            &store,
            &list_samples(
                Some(Predicate::bbox("location", "-1", "-1", "1", "1")),
                None,
                N,
                0,
            ),
        )
        .unwrap();
    let bbox_ms = started.elapsed();
    assert_eq!(keys(&bbox), ["k0"]);
    println!("10^6 typed-ref queries: nn={nn_ms:?} time-range={range_ms:?} bbox={bbox_ms:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
