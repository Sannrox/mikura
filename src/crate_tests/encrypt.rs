use super::super::*;
use super::helpers::*;
use crate::backup::{backup_log, restore_log, verify_log};
use crate::crypto::{decode_catalog, LogKeys, MemoryKeyProvider, RuntimeProfile, STORE_KEY_TENANT};
use crate::{KeyId, KeyProvider, KeyPurpose, TenantId, UnwrapGrant};
use std::sync::Arc;

fn grant(tenant: &str) -> UnwrapGrant {
    UnwrapGrant {
        tenant_id: TenantId::new(tenant).unwrap(),
        purpose: KeyPurpose::Store,
        expires_unix_secs: i64::MAX,
    }
}

fn enc_keys(tenants: &[&str]) -> (Arc<dyn KeyProvider + Send + Sync>, LogKeys) {
    let provider: Arc<dyn KeyProvider + Send + Sync> =
        Arc::new(MemoryKeyProvider::new(RuntimeProfile::Development).unwrap());
    let mut grants = vec![grant(STORE_KEY_TENANT)];
    for tenant in tenants {
        grants.push(grant(tenant));
    }
    (
        Arc::clone(&provider),
        LogKeys::new(Arc::clone(&provider), grants).unwrap(),
    )
}

fn secret_rec(tenant: &str, key: &str, secret: &str) -> ObjectRecord {
    rec_in(
        &tenant_namespace(tenant, "ns").unwrap(),
        "Item",
        key,
        false,
        &[("secret", secret)],
    )
}

#[test]
fn encrypted_log_scan_finds_no_plaintext() {
    let (dir, log) = temp_log("enc-scan");
    let (_provider, keys) = enc_keys(&["acme"]);
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    store
        .append(secret_rec("acme", "k1", "top-secret-payload"))
        .unwrap();
    drop(store);
    let bytes = std::fs::read(&log).unwrap();
    assert!(
        !bytes
            .windows(b"top-secret-payload".len())
            .any(|w| w == b"top-secret-payload"),
        "plaintext leaked into the log"
    );
    assert_eq!(&bytes[4..12], LOG_FORMAT_ENCRYPTED);
    let err = Store::open(&log).err().expect("plaintext open must fail");
    assert!(err.contains("encrypted log requires keys"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn revocation_isolates_one_tenant() {
    let (dir, log) = temp_log("enc-revoke");
    let (provider, keys) = enc_keys(&["acme", "beta"]);
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    store
        .append(secret_rec("acme", "a1", "acme-secret"))
        .unwrap();
    store
        .append(secret_rec("beta", "b1", "beta-secret"))
        .unwrap();
    let acme_ns = tenant_namespace("acme", "ns").unwrap();
    let beta_ns = tenant_namespace("beta", "ns").unwrap();
    assert_eq!(
        store
            .load(&acme_ns, "Item", "a1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("acme-secret")
    );
    drop(store);

    let bytes = std::fs::read(&log).unwrap();
    let mut catalog = &bytes[18..];
    let catalog = decode_catalog(&mut catalog).unwrap();
    let acme_id = KeyId::new(catalog.get("acme").expect("acme dek").dek_id.as_str()).unwrap();
    provider.revoke_dek(&acme_id).unwrap();
    let keys_all = LogKeys::new(
        provider,
        vec![grant(STORE_KEY_TENANT), grant("acme"), grant("beta")],
    )
    .unwrap();
    let store = Store::open_with_keys(&log, keys_all).unwrap();
    assert!(store
        .load(&acme_ns, "Item", "a1", &PropertyAcl::allow_all())
        .is_err());
    assert_eq!(
        store
            .load(&beta_ns, "Item", "b1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("beta-secret")
    );
    let mut store = store;
    let err = store.append(secret_rec("acme", "a2", "nope")).unwrap_err();
    assert!(
        err.contains("grant") || err.contains("revoked") || err.contains("tenant"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn live_revoke_refuses_writes_and_compact_omits_tenant() {
    let (dir, log) = temp_log("enc-live-revoke");
    let (provider, keys) = enc_keys(&["acme", "beta"]);
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    store
        .append(secret_rec("acme", "a1", "acme-secret"))
        .unwrap();
    store
        .append(secret_rec("beta", "b1", "beta-secret"))
        .unwrap();
    let bytes = std::fs::read(&log).unwrap();
    let mut catalog = &bytes[18..];
    let catalog = decode_catalog(&mut catalog).unwrap();
    let acme_id = KeyId::new(catalog.get("acme").expect("acme dek").dek_id.as_str()).unwrap();
    provider.revoke_dek(&acme_id).unwrap();
    let acme_ns = tenant_namespace("acme", "ns").unwrap();
    assert!(
        store
            .load(&acme_ns, "Item", "a1", &PropertyAcl::allow_all())
            .is_err(),
        "live revoke must hide the tenant before the next write"
    );
    let err = store.append(secret_rec("acme", "a2", "nope")).unwrap_err();
    assert!(
        err.contains("revoked") || err.contains("unavailable") || err.contains("tenant"),
        "{err}"
    );
    let dst = dir.join("compacted.mikura");
    store.compact_to(&dst).unwrap();
    let keys_all = LogKeys::new(
        provider,
        vec![grant(STORE_KEY_TENANT), grant("acme"), grant("beta")],
    )
    .unwrap();
    let compacted = Store::open_with_keys(&dst, keys_all).unwrap();
    let beta_ns = tenant_namespace("beta", "ns").unwrap();
    assert!(compacted
        .load(&acme_ns, "Item", "a1", &PropertyAcl::allow_all())
        .is_err());
    assert_eq!(
        compacted
            .load(&beta_ns, "Item", "b1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("beta-secret")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn abort_after_revocation_keeps_tenant_filtered() {
    let (dir, log) = temp_log("enc-abort-revoke");
    let (provider, keys) = enc_keys(&["acme", "beta"]);
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    store
        .append(secret_rec("acme", "a1", "acme-secret"))
        .unwrap();
    store
        .append(secret_rec("beta", "b1", "beta-secret"))
        .unwrap();
    drop(store);

    let bytes = std::fs::read(&log).unwrap();
    let mut catalog = &bytes[18..];
    let catalog = decode_catalog(&mut catalog).unwrap();
    let acme_id = KeyId::new(catalog.get("acme").expect("acme dek").dek_id.as_str()).unwrap();
    provider.revoke_dek(&acme_id).unwrap();
    let keys_all = LogKeys::new(
        provider,
        vec![grant(STORE_KEY_TENANT), grant("acme"), grant("beta")],
    )
    .unwrap();
    let mut store = Store::open_with_keys(&log, keys_all).unwrap();
    let acme_ns = tenant_namespace("acme", "ns").unwrap();
    let beta_ns = tenant_namespace("beta", "ns").unwrap();
    let err = store
        .append_batch(vec![
            secret_rec("beta", "b2", "beta-two"),
            secret_rec("acme", "a2", "should-abort"),
        ])
        .unwrap_err();
    assert!(
        err.contains("grant") || err.contains("revoked") || err.contains("tenant"),
        "{err}"
    );
    assert!(store
        .load(&acme_ns, "Item", "a1", &PropertyAcl::allow_all())
        .is_err());
    assert_eq!(
        store
            .load(&beta_ns, "Item", "b1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("beta-secret")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn withheld_grant_does_not_drop_other_tenant_snapshot() {
    let (dir, log) = temp_log("enc-withheld");
    let (provider, keys) = enc_keys(&["acme", "beta"]);
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    store
        .append(secret_rec("acme", "a1", "acme-secret"))
        .unwrap();
    store
        .append(secret_rec("beta", "b1", "beta-secret"))
        .unwrap();
    drop(store);

    let keys_beta = LogKeys::new(
        Arc::clone(&provider),
        vec![grant(STORE_KEY_TENANT), grant("beta")],
    )
    .unwrap();
    let mut store = Store::open_with_keys(&log, keys_beta).unwrap();
    store.append(secret_rec("beta", "b2", "beta-two")).unwrap();
    drop(store);

    let keys_all = LogKeys::new(
        provider,
        vec![grant(STORE_KEY_TENANT), grant("acme"), grant("beta")],
    )
    .unwrap();
    let store = Store::open_with_keys(&log, keys_all).unwrap();
    let acme_ns = tenant_namespace("acme", "ns").unwrap();
    let beta_ns = tenant_namespace("beta", "ns").unwrap();
    assert_eq!(
        store
            .load(&acme_ns, "Item", "a1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("acme-secret")
    );
    assert_eq!(
        store
            .load(&beta_ns, "Item", "b2", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("beta-two")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn store_dek_cannot_read_tenant_checkpoint_payload() {
    let (dir, log) = temp_log("enc-ckpt-parts");
    let (_provider, keys) = enc_keys(&["acme"]);
    let mut store = Store::create_with_keys(&log, keys.clone()).unwrap();
    store
        .append(secret_rec("acme", "k1", "acme-secret"))
        .unwrap();
    drop(store);

    let log_bytes = std::fs::read(&log).unwrap();
    let mut catalog = &log_bytes[18..];
    let catalog = decode_catalog(&mut catalog).unwrap();
    let store_wrapped = catalog.get("").expect("store dek");
    let store_dek = keys
        .unwrap_dek(store_wrapped)
        .unwrap()
        .expect("store grant");
    let ckpt = std::fs::read(Store::checkpoint_path(&log)).unwrap();
    let (_, inner) = crate::crypto::open_blob(&store_dek, &ckpt).unwrap();
    assert!(
        !inner
            .windows(b"acme-secret".len())
            .any(|w| w == b"acme-secret"),
        "store DEK unwrapped tenant payload"
    );
    let (index, _) = inner.split_at(inner.len() - 4);
    let parts = crate::crypto::decode_parts(index).unwrap();
    let acme_blob = parts.get("acme").expect("acme part");
    let acme_wrapped = catalog.get("acme").expect("acme dek");
    let acme_dek = keys.unwrap_dek(acme_wrapped).unwrap().expect("acme grant");
    let (_, acme_plain) = crate::crypto::open_blob(&acme_dek, acme_blob).unwrap();
    assert!(
        acme_plain
            .windows(b"acme-secret".len())
            .any(|w| w == b"acme-secret"),
        "tenant DEK must open its snapshot"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn backup_restore_round_trip_with_keys() {
    let (dir, log) = temp_log("enc-backup");
    let (_provider, keys) = enc_keys(&["acme"]);
    let mut store = Store::create_with_keys(&log, keys.clone()).unwrap();
    store.append(secret_rec("acme", "k1", "keep-me")).unwrap();
    drop(store);
    let backup = dir.join("backup.mikura");
    backup_log(&log, &backup).unwrap();
    verify_log(&backup).unwrap();
    let restored = dir.join("restored.mikura");
    restore_log(&backup, &restored, None).unwrap();
    let store = Store::open_with_keys(&restored, keys).unwrap();
    let ns = tenant_namespace("acme", "ns").unwrap();
    assert_eq!(
        store
            .load(&ns, "Item", "k1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("keep-me")
    );
    let bytes = std::fs::read(&backup).unwrap();
    assert!(!bytes.windows(b"keep-me".len()).any(|w| w == b"keep-me"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn encrypted_open_ignores_plaintext_dirty_set() {
    let (dir, log) = temp_log("enc-plain-delta");
    let (_provider, keys) = enc_keys(&["acme"]);
    let mut store = Store::create_with_keys(&log, keys.clone()).unwrap();
    store
        .append(secret_rec("acme", "k1", "acme-secret"))
        .unwrap();
    drop(store);

    let plain_log = dir.join("plain.mikura");
    let mut plain = Store::create(&plain_log).unwrap();
    for i in 0..5 {
        plain
            .append(rec(
                "Item",
                &format!("seed{i}"),
                false,
                &[("secret", "seed")],
            ))
            .unwrap();
    }
    plain
        .append(rec("Item", "k2", false, &[("secret", "forged-delta")]))
        .unwrap();
    drop(plain);
    let src_delta = Store::join_delta_path(&plain_log);
    assert!(
        src_delta.exists(),
        "plaintext store should have written a dirty-set"
    );
    std::fs::copy(src_delta, Store::join_delta_path(&log)).unwrap();
    let store = Store::open_with_keys(&log, keys).unwrap();
    let ns = tenant_namespace("acme", "ns").unwrap();
    assert_eq!(
        store
            .load(&ns, "Item", "k1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("acme-secret")
    );
    assert!(store
        .load(&ns, "Item", "k2", &PropertyAcl::allow_all())
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn encrypted_open_refuses_plaintext_snapshot() {
    let (dir, log) = temp_log("enc-plain-sidecar");
    let (_provider, keys) = enc_keys(&["acme"]);
    let mut store = Store::create_with_keys(&log, keys.clone()).unwrap();
    store
        .append(secret_rec("acme", "k1", "acme-secret"))
        .unwrap();
    drop(store);

    let plain_log = dir.join("plain.mikura");
    let mut plain = Store::create(&plain_log).unwrap();
    plain
        .append(rec("Item", "k1", false, &[("secret", "forged")]))
        .unwrap();
    drop(plain);
    std::fs::copy(Store::join_map_path(&plain_log), Store::join_map_path(&log)).unwrap();
    let err = Store::open_with_keys(&log, keys)
        .err()
        .expect("plaintext snapshot must fail");
    assert!(err.contains("sealed"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reserved_store_tenant_is_refused() {
    let (dir, log) = temp_log("enc-reserved");
    let (_provider, keys) = enc_keys(&[STORE_KEY_TENANT]);
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    let err = store
        .append(secret_rec(STORE_KEY_TENANT, "k1", "nope"))
        .unwrap_err();
    assert!(err.contains("reserved"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oversized_tenant_prefix_fails_closed() {
    let (dir, log) = temp_log("enc-huge-tenant");
    let tenant = "t".repeat(5000);
    let (_provider, keys) = enc_keys(&["acme"]);
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    let rec = rec_in(
        &format!("{tenant}/ns"),
        "Item",
        "k1",
        false,
        &[("secret", "x")],
    );
    let err = store.append(rec).unwrap_err();
    assert!(
        err.contains("tenant") || err.contains("page") || err.contains("invalid"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expired_grant_does_not_drop_other_tenant_snapshot() {
    let (dir, log) = temp_log("enc-expired-keep");
    let (provider, keys) = enc_keys(&["acme", "beta"]);
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    store
        .append(secret_rec("acme", "a1", "acme-secret"))
        .unwrap();
    store
        .append(secret_rec("beta", "b1", "beta-secret"))
        .unwrap();
    drop(store);

    let expired = UnwrapGrant {
        tenant_id: TenantId::new("acme").unwrap(),
        purpose: KeyPurpose::Store,
        expires_unix_secs: 0,
    };
    let keys_beta = LogKeys::new(
        Arc::clone(&provider),
        vec![grant(STORE_KEY_TENANT), expired, grant("beta")],
    )
    .unwrap();
    let mut store = Store::open_with_keys(&log, keys_beta).unwrap();
    store.append(secret_rec("beta", "b2", "beta-two")).unwrap();
    drop(store);

    let keys_all = LogKeys::new(
        provider,
        vec![grant(STORE_KEY_TENANT), grant("acme"), grant("beta")],
    )
    .unwrap();
    let store = Store::open_with_keys(&log, keys_all).unwrap();
    let acme_ns = tenant_namespace("acme", "ns").unwrap();
    assert_eq!(
        store
            .load(&acme_ns, "Item", "a1", &PropertyAcl::allow_all())
            .unwrap()
            .props
            .get("secret")
            .map(String::as_str),
        Some("acme-secret")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expired_grant_refuses_first_write() {
    let (dir, log) = temp_log("enc-expired");
    let provider: Arc<dyn KeyProvider + Send + Sync> =
        Arc::new(MemoryKeyProvider::new(RuntimeProfile::Development).unwrap());
    let expired = UnwrapGrant {
        tenant_id: TenantId::new("acme").unwrap(),
        purpose: KeyPurpose::Store,
        expires_unix_secs: 0,
    };
    let keys = LogKeys::new(
        Arc::clone(&provider),
        vec![grant(STORE_KEY_TENANT), expired],
    )
    .unwrap();
    let mut store = Store::create_with_keys(&log, keys).unwrap();
    let err = store.append(secret_rec("acme", "k1", "nope")).unwrap_err();
    assert!(
        err.contains("grant") || err.contains("unwrap") || err.contains("tenant"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn encrypted_write_overhead_is_measured() {
    let (dir, plain_log) = temp_log("enc-overhead-plain");
    let enc_log = dir.join("enc.mikura");
    let mut plain = Store::create(&plain_log).unwrap();
    let rec = rec("Item", "k1", false, &[("n", "1")]);
    plain.append(rec.clone()).unwrap();
    drop(plain);
    let (_provider, keys) = enc_keys(&["acme"]);
    let mut enc = Store::create_with_keys(&enc_log, keys).unwrap();
    enc.append(secret_rec("acme", "k1", "1")).unwrap();
    drop(enc);
    let plain_len = std::fs::metadata(&plain_log).unwrap().len() as f64;
    let enc_len = std::fs::metadata(&enc_log).unwrap().len() as f64;
    let ratio = enc_len / plain_len.max(1.0);
    assert!(
        ratio < 8.0,
        "encrypted log {enc_len} vs plaintext {plain_len} ratio {ratio}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
