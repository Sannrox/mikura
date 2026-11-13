use super::super::*;
use super::helpers::*;
use crate::crypto::{is_sealed, LogKeys, MemoryKeyProvider, RuntimeProfile, STORE_KEY_TENANT};
use crate::{KeyPurpose, TenantId, UnwrapGrant};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

fn budget() -> ServingBudget {
    ServingBudget::from_bytes(1024 * 1024).expect("nonzero")
}

#[test]
fn serving_budget_zero_fails_closed() {
    let err = ServingBudget::from_bytes(0).unwrap_err();
    assert!(err.contains("greater than zero"), "{err}");
}

#[test]
fn default_open_keeps_join06() {
    let (dir, log) = temp_log("paged-default-06");
    let mut store = Store::create(&log).unwrap();
    store
        .append(rec("Thing", "k", false, &[("n", "1")]))
        .unwrap();
    drop(store);
    let mut magic = [0u8; 8];
    std::fs::File::open(Store::join_map_path(&log))
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    assert_eq!(&magic, JOIN_FORMAT);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn budgeted_open_of_previous_magic_serves() {
    let (dir, log) = temp_log("paged-from-06");
    let mut store = Store::create(&log).unwrap();
    append_all(&mut store, fixture());
    drop(store);
    let mut magic = [0u8; 8];
    std::fs::File::open(Store::join_map_path(&log))
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    assert_eq!(&magic, JOIN_FORMAT);
    let budgeted = Store::open_with_serving_budget(&log, budget()).unwrap();
    let answer = ObjectSet::new(LocalCompute)
        .evaluate(&budgeted, &fixture_request())
        .unwrap();
    assert_eq!(answer.sum_amount, 10);
    drop(budgeted);
    std::fs::File::open(Store::join_map_path(&log))
        .unwrap()
        .read_exact(&mut magic)
        .unwrap();
    assert_eq!(&magic, JOIN_FORMAT_PAGED);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn torn_paged_sidecar_fails_closed_then_rebuilds() {
    let (dir, log) = temp_log("paged-torn");
    let mut store = Store::create(&log).unwrap();
    store
        .append(rec("Thing", "k", false, &[("n", "1")]))
        .unwrap();
    drop(store);
    drop(Store::open_with_serving_budget(&log, budget()).unwrap());
    let sidecar = Store::join_map_path(&log);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&sidecar)
        .unwrap();
    file.seek(SeekFrom::Start(4092)).unwrap();
    file.write_all(&[0, 0, 0, 0]).unwrap();
    drop(file);
    let err = match Store::open_with_serving_budget(&log, budget()) {
        Ok(_) => panic!("torn paged sidecar must fail closed"),
        Err(err) => err,
    };
    assert!(
        err.contains("checksum") || err.contains("magic") || err.contains("crc"),
        "{err}"
    );
    std::fs::remove_file(&sidecar).unwrap();
    let rebuilt = Store::open_with_serving_budget(&log, budget()).unwrap();
    let row = rebuilt
        .load(NS, "Thing", "k", &PropertyAcl::allow_all())
        .unwrap();
    assert_eq!(row.props.get("n").map(String::as_str), Some("1"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn torn_data_page_fails_closed() {
    let (dir, log) = temp_log("paged-torn-data");
    let mut store = Store::create(&log).unwrap();
    store
        .append(rec("Thing", "k", false, &[("n", "1")]))
        .unwrap();
    drop(store);
    drop(Store::open_with_serving_budget(&log, budget()).unwrap());
    let sidecar = Store::join_map_path(&log);
    let len = std::fs::metadata(&sidecar).unwrap().len();
    assert!(len >= 8192, "need a data page after the superblock");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&sidecar)
        .unwrap();
    let pages = len / 4096;
    for i in 1..pages {
        file.seek(SeekFrom::Start(i * 4096 + 4092)).unwrap();
        file.write_all(&[0, 0, 0, 0]).unwrap();
    }
    drop(file);
    let err = match Store::open_with_serving_budget(&log, budget()) {
        Ok(budgeted) => match budgeted.load(NS, "Thing", "k", &PropertyAcl::allow_all()) {
            Ok(_) => panic!("torn data page must fail closed"),
            Err(err) => err,
        },
        Err(err) => err,
    };
    assert!(
        err.contains("checksum") || err.contains("crc") || err.contains("magic"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn budgeted_reopen_keeps_schema() {
    let (dir, log) = temp_log("paged-schema");
    let mut store = Store::create(&log).unwrap();
    let schema = SchemaDescriptor {
        kind: "Thing".into(),
        properties: vec!["n".into()],
        ..SchemaDescriptor::default()
    }
    .to_record(NS)
    .unwrap();
    store.append(schema).unwrap();
    store
        .append(rec("Thing", "k", false, &[("n", "1")]))
        .unwrap();
    drop(store);
    drop(Store::open_with_serving_budget(&log, budget()).unwrap());
    let reopened = Store::open_with_serving_budget(&log, budget()).unwrap();
    let desc = reopened
        .schema(NS, "Thing")
        .unwrap()
        .expect("schema survives budgeted reopen");
    assert_eq!(desc.kind, "Thing");
    assert_eq!(desc.properties, vec!["n".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

fn grant(tenant: &str) -> UnwrapGrant {
    UnwrapGrant {
        tenant_id: TenantId::new(tenant).unwrap(),
        purpose: KeyPurpose::Store,
        expires_unix_secs: i64::MAX,
    }
}

#[test]
fn budgeted_encrypted_sidecar_stays_sealed() {
    let (dir, log) = temp_log("paged-enc-seal");
    let provider: Arc<dyn crate::KeyProvider + Send + Sync> =
        Arc::new(MemoryKeyProvider::new(RuntimeProfile::Development).unwrap());
    let keys = LogKeys::new(
        Arc::clone(&provider),
        vec![grant(STORE_KEY_TENANT), grant("acme")],
    )
    .unwrap();
    let ns = tenant_namespace("acme", "ns").unwrap();
    let mut store = Store::create_with_keys(&log, keys.clone()).unwrap();
    store
        .append(rec_in(
            &ns,
            "Item",
            "k1",
            false,
            &[("secret", "top-secret")],
        ))
        .unwrap();
    drop(store);
    let err = match Store::open_with_keys_and_serving_budget(&log, keys, budget()) {
        Ok(_) => panic!("encrypted stores must refuse a serving budget"),
        Err(err) => err,
    };
    assert!(
        err.contains("serving budget") && err.contains("encrypted"),
        "{err}"
    );
    let sidecar = std::fs::read(Store::join_map_path(&log)).unwrap();
    assert!(is_sealed(&sidecar), "sidecar must stay sealed");
    assert_ne!(&sidecar[..8], JOIN_FORMAT_PAGED);
    assert!(
        !sidecar
            .windows(b"top-secret".len())
            .any(|w| w == b"top-secret"),
        "plaintext leaked into the sidecar"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
