//! HTTP object backend against a loopback store (ADR 0047 §12, #360).

use super::super::*;
use super::helpers::*;
use crate::crypto::{LogKeys, MemoryKeyProvider, RuntimeProfile, STORE_KEY_TENANT};
use crate::log::{objects_dir, LogWriter, SyncPolicy};
use crate::{
    backup::{backup_log, restore_log},
    tenant_namespace, KeyPurpose, LocalObjectServer, ObjectBackend, TenantId, UnwrapGrant,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static NEXT: AtomicU64 = AtomicU64::new(1);

fn item(key: &str, n: &str) -> ObjectRecord {
    rec("Item", key, false, &[("n", n)])
}

fn backend(server: &LocalObjectServer) -> ObjectBackend {
    ObjectBackend::http(server.endpoint(), None).unwrap()
}

fn unique_log(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mikura-obj-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("objects.mikura");
    (dir, log)
}

#[test]
fn http_create_refuses_a_non_empty_namespace() {
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("occupied");
    let mut store = Store::create_with(&log, backend(&server)).unwrap();
    store.append(item("a", "1")).unwrap();
    drop(store);
    let (dir2, log2) = unique_log("occupied2");
    let err = Store::create_with(&log2, backend(&server))
        .err()
        .expect("occupied namespace must fail create");
    assert!(err.contains("object namespace is not empty"), "{err}");
    assert!(
        !crate::log::objects_dir(&log2).is_file(),
        "failed create must not leave a locator"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn http_open_refuses_a_file_backend_log() {
    let (dir, log) = unique_log("file-open");
    let mut store = Store::create(&log).unwrap();
    store.append(item("a", "1")).unwrap();
    drop(store);
    let server = LocalObjectServer::start().unwrap();
    let err = Store::open_with(&log, backend(&server))
        .err()
        .expect("file log must not open as object");
    assert!(err.contains("file-backend"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_object_len_does_not_count_as_get() {
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("head");
    let mut store = Store::create_with(&log, backend(&server)).unwrap();
    store.append(item("a", "1")).unwrap();
    drop(store);
    let objects =
        crate::objects::ObjectStore::open(&crate::log::objects_dir(&log), false, &backend(&server))
            .unwrap();
    let before = objects.storage_calls();
    assert!(objects.object_len("manifest.1").unwrap() > 0);
    let delta = objects.storage_calls().saturating_sub(before);
    assert_eq!(delta.object_len, 1, "{delta:?}");
    assert_eq!(delta.gets, 0, "{delta:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_endpoint_rejects_a_path() {
    let err = ObjectBackend::http("http://127.0.0.1:1/bucket", None).unwrap_err();
    assert_eq!(err.kind_name(), "unavailable");
    assert!(err.to_string().contains("no path"), "{err}");
}

#[test]
fn http_backup_restores_acked_history() {
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("backup");
    let mut store = Store::create_with(&log, backend(&server)).unwrap();
    store.append(item("a", "1")).unwrap();
    store.append(item("b", "2")).unwrap();
    drop(store);
    let backup = dir.join("backup.mikura");
    backup_log(&log, &backup).unwrap();
    let restored = dir.join("restored.mikura");
    restore_log(&backup, &restored, None).unwrap();
    let reopened = Store::open(&restored).unwrap();
    assert_eq!(
        reopened
            .load(NS, "Item", "a", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("n")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        reopened
            .load(NS, "Item", "b", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("n")
            .map(String::as_str),
        Some("2")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_compact_refuses_file_backend_target() {
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("compact");
    let mut store = Store::create_with(&log, backend(&server)).unwrap();
    store.append(item("a", "1")).unwrap();
    let err = store.compact_to(&dir.join("compact.mikura")).unwrap_err();
    assert!(err.contains("object-backend"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_token_requires_loopback() {
    let err = ObjectBackend::http("http://10.0.0.1:1", Some("secret".into())).unwrap_err();
    assert_eq!(err.kind_name(), "unavailable");
    assert!(
        err.to_string().contains("loopback") || err.to_string().contains("MIKURA_OBJECT_TOKEN"),
        "{err}"
    );
}

#[test]
fn http_explicit_token_requires_env() {
    let err = ObjectBackend::http("http://127.0.0.1:1", Some("secret".into())).unwrap_err();
    assert_eq!(err.kind_name(), "unavailable");
    assert!(err.to_string().contains("MIKURA_OBJECT_TOKEN"), "{err}");
}

#[test]
fn http_probe_refuses_a_store_that_accepts_both_writes() {
    let server = LocalObjectServer::start_weak().unwrap();
    let (dir, log) = unique_log("weak");
    let err = Store::create_with(&log, backend(&server))
        .err()
        .expect("weak store must fail the probe");
    assert!(err.contains("conditional write probe failed"), "{err}");
    assert!(err.contains("both competing puts succeeded"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_collect_drops_only_fenced_unreferenced_objects() {
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("collect");
    let mut store = Store::create_with(&log, backend(&server)).unwrap();
    store.append(item("a", "1")).unwrap();
    store.set_crash_before_swap(true);
    let _ = store.append(item("b", "2"));
    store.crash();
    let mut store = Store::open(&log).unwrap();
    store.collect().unwrap();
    assert!(store
        .load(NS, "Item", "a", &PropertyAcl::allow_all())
        .is_ok());
    assert!(store
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_err());
    let objects =
        crate::objects::ObjectStore::open(&objects_dir(&log), false, &backend(&server)).unwrap();
    let (version, bytes) = objects.current().unwrap().expect("manifest");
    let mut manifest = crate::objects::decode_manifest(&bytes).unwrap();
    manifest.version = version;
    let live = manifest.live_keys();
    for name in objects.list().unwrap() {
        if name.starts_with('.') {
            continue;
        }
        assert!(live.contains(&name), "leftover {name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_crash_and_replay_keep_only_acked_writes() {
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("replay");
    let mut store = Store::create_with(&log, backend(&server)).unwrap();
    store.append(item("a", "1")).unwrap();
    store.set_crash_before_swap(true);
    let err = store.append(item("b", "2")).unwrap_err();
    assert!(err.contains("crash before swap"), "{err}");
    store.crash();
    let reopened = Store::open(&log).unwrap();
    assert!(reopened
        .load(NS, "Item", "a", &PropertyAcl::allow_all())
        .is_ok());
    assert!(reopened
        .load(NS, "Item", "b", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_swap_then_reopen_keeps_acked_history() {
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("swap");
    let mut store = Store::create_with(&log, backend(&server)).unwrap();
    store.append(item("a", "1")).unwrap();
    store.append(item("b", "2")).unwrap();
    drop(store);
    let reopened = Store::open(&log).unwrap();
    assert_eq!(
        reopened
            .load(NS, "Item", "a", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("n")
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        reopened
            .load(NS, "Item", "b", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("n")
            .map(String::as_str),
        Some("2")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_data_commit_is_two_waves() {
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("waves");
    let mut writer =
        LogWriter::create_with_backend(&log, SyncPolicy::None, backend(&server)).unwrap();
    let before = writer.storage_calls();
    writer.append_record(&item("a", "1")).unwrap();
    writer.flush().unwrap();
    let delta = writer.storage_calls().saturating_sub(before);
    assert_eq!(delta.puts, 1, "{delta:?}");
    assert_eq!(delta.cas, 1, "{delta:?}");
    assert_eq!(delta.commit_waves(), 2, "{delta:?}");
    drop(writer);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_errors_are_typed_without_transport_dumps() {
    let err = ObjectBackend::http("", None).unwrap_err();
    assert_eq!(err.kind_name(), "unavailable");
    assert!(!err.to_string().contains("TcpStream"), "{err}");
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("typed");
    let store = Store::create_with(&log, backend(&server)).unwrap();
    drop(store);
    let objects =
        crate::objects::ObjectStore::open(&crate::log::objects_dir(&log), false, &backend(&server))
            .unwrap();
    let err = objects.get("missing-key").unwrap_err();
    assert!(err.starts_with("not found:"), "{err}");
    assert!(!err.contains("TcpStream"), "{err}");
    assert!(!err.contains("Transport"), "{err}");
    let err = objects.put("manifest.1", b"x").unwrap_err();
    assert!(
        err.contains("already exists") || err.contains("refused precondition"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_encrypted_objects_stay_ciphertext() {
    let server = LocalObjectServer::start().unwrap();
    let (dir, log) = unique_log("enc");
    let provider: Arc<dyn crate::KeyProvider + Send + Sync> =
        Arc::new(MemoryKeyProvider::new(RuntimeProfile::Development).unwrap());
    let keys = LogKeys::new(
        Arc::clone(&provider),
        vec![
            UnwrapGrant {
                tenant_id: TenantId::new(STORE_KEY_TENANT).unwrap(),
                purpose: KeyPurpose::Store,
                expires_unix_secs: i64::MAX,
            },
            UnwrapGrant {
                tenant_id: TenantId::new("acme").unwrap(),
                purpose: KeyPurpose::Store,
                expires_unix_secs: i64::MAX,
            },
        ],
    )
    .unwrap();
    let mut store = Store::create_with_keys_on(&log, keys, backend(&server)).unwrap();
    store
        .append(rec_in(
            &tenant_namespace("acme", "ns").unwrap(),
            "Item",
            "k1",
            false,
            &[("secret", "top-secret-payload")],
        ))
        .unwrap();
    drop(store);
    assert!(
        !log_tree_contains(&log, b"top-secret-payload"),
        "plaintext leaked into the stub"
    );
    assert!(
        !server.contains_bytes(b"top-secret-payload"),
        "plaintext leaked into an object"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
