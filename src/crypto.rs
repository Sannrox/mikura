//! Envelope encryption for `MIKURAV3` logs (ADR 0044).
//!
//! The embedder unwraps through [`sekai_keys::KeyProvider`]. Mikura never
//! stores plaintext keys. Wrapped DEKs live in the superblock catalog.
//! Sidecar and checkpoint *indexes* use the store DEK; each tenant's
//! payload is sealed with that tenant's DEK so shredding the tenant key
//! drops the snapshot copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;
pub use sekai_keys::{
    Dek, KeyError, KeyId, KeyProvider, KeyPurpose, MemoryKeyProvider, RuntimeProfile, TenantId,
    UnwrapGrant, WrappedDek,
};

use crate::codec::{read_str, take, write_str};

/// Superblock catalog tenant for sidecar and checkpoint encryption.
pub const STORE_KEY_TENANT: &str = "mikura.store";
/// Sealed sidecar / checkpoint envelope.
pub const SEAL_MAGIC: &[u8; 8] = b"MKSEAL01";
/// Store-DEK index of per-tenant sealed snapshot parts.
pub const PART_MAGIC: &[u8; 8] = b"MKPART01";
const ENGINE: &str = "mikura";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Caller-supplied provider and unwrap grants for one encrypted log.
#[derive(Clone)]
pub struct LogKeys {
    provider: Arc<dyn KeyProvider + Send + Sync>,
    grants: Vec<UnwrapGrant>,
}

impl std::fmt::Debug for LogKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogKeys")
            .field("grants", &self.grants.len())
            .finish_non_exhaustive()
    }
}

impl LogKeys {
    /// `grants` must cover `STORE_KEY_TENANT` and every tenant that should
    /// load. A missing grant omits that tenant from this process; it does
    /// not rewrite snapshots. Revocation shreds the tenant DEK.
    pub fn new(
        provider: Arc<dyn KeyProvider + Send + Sync>,
        grants: Vec<UnwrapGrant>,
    ) -> Result<Self, String> {
        if !grants
            .iter()
            .any(|grant| grant.tenant_id.as_str() == STORE_KEY_TENANT)
        {
            return Err("encrypted log requires a store-key unwrap grant".into());
        }
        Ok(Self { provider, grants })
    }

    pub(crate) fn grant_for(&self, tenant: &str) -> Option<&UnwrapGrant> {
        self.grants
            .iter()
            .find(|grant| grant.tenant_id.as_str() == tenant && grant.purpose == KeyPurpose::Store)
    }

    pub(crate) fn wrap(&self, tenant: &str, dek: &Dek) -> Result<WrappedDek, String> {
        let tenant_id = TenantId::new(tenant).map_err(|e| e.to_string())?;
        self.provider
            .wrap(&tenant_id, KeyPurpose::Store, dek)
            .map_err(|e| e.to_string())
    }

    /// Wrap a generated DEK and prove the grant can unwrap it.
    pub(crate) fn mint(&self, tenant: &str) -> Result<(Dek, WrappedDek), String> {
        let dek = Dek::generate();
        let wrapped = self.wrap(tenant, &dek)?;
        match self.unwrap_dek(&wrapped)? {
            Some(opened) if opened.as_bytes() == dek.as_bytes() => Ok((dek, wrapped)),
            Some(_) => Err("wrapped dek did not round-trip".into()),
            None => Err("unwrap grant cannot unwrap newly wrapped dek".into()),
        }
    }

    pub(crate) fn unwrap_status(&self, wrapped: &WrappedDek) -> Result<DekStatus, String> {
        let tenant = wrapped.tenant_id.as_str();
        let Some(grant) = self.grant_for(tenant) else {
            return Ok(DekStatus::Withheld);
        };
        match self
            .provider
            .unwrap(wrapped, grant, ENGINE, now_unix_secs())
        {
            Ok(out) => {
                let _audit = out.audit;
                Ok(DekStatus::Ready(out.dek))
            }
            Err(KeyError::GrantDenied | KeyError::UnknownKey) => Ok(DekStatus::Withheld),
            Err(KeyError::Revoked) => Ok(DekStatus::Shredded),
            Err(err) => Err(err.to_string()),
        }
    }

    pub(crate) fn unwrap_dek(&self, wrapped: &WrappedDek) -> Result<Option<Dek>, String> {
        match self.unwrap_status(wrapped)? {
            DekStatus::Ready(dek) => Ok(Some(dek)),
            DekStatus::Withheld | DekStatus::Shredded => Ok(None),
        }
    }
}

/// Outcome of unwrapping a catalog DEK for this process.
pub(crate) enum DekStatus {
    Ready(Dek),
    /// Missing or expired grant: omit from memory, keep snapshots.
    Withheld,
    /// `revoke_dek`: omit from memory and later complete snapshots.
    Shredded,
}

/// Tenant id encoded in a `tenant_id/namespace` store key (ADR 0030 / 0044).
pub(crate) fn tenant_of_namespace(namespace: &str) -> Result<&str, String> {
    let (tenant, rest) = namespace
        .split_once('/')
        .ok_or_else(|| "encrypted log requires tenant-prefixed namespace".to_string())?;
    if tenant.is_empty() || rest.is_empty() {
        return Err("encrypted log requires tenant-prefixed namespace".into());
    }
    if tenant == STORE_KEY_TENANT {
        return Err("tenant id mikura.store is reserved".into());
    }
    TenantId::new(tenant).map_err(|e| e.to_string())?;
    Ok(tenant)
}

pub(crate) fn encode_catalog(entries: &BTreeMap<String, WrappedDek>) -> Result<Vec<u8>, String> {
    let n = u16::try_from(entries.len()).map_err(|_| "too many key catalog entries".to_string())?;
    let mut body = Vec::new();
    body.extend_from_slice(&n.to_le_bytes());
    for (tenant, wrapped) in entries {
        write_str(&mut body, tenant)?;
        let json = serde_json::to_string(wrapped).map_err(|e| e.to_string())?;
        write_str(&mut body, &json)?;
    }
    Ok(body)
}

pub(crate) fn decode_catalog(cur: &mut &[u8]) -> Result<BTreeMap<String, WrappedDek>, String> {
    if cur.len() < 2 {
        return Err("short key catalog".into());
    }
    let n = u16::from_le_bytes(take::<2>(cur)?.try_into().unwrap()) as usize;
    let mut entries = BTreeMap::new();
    for _ in 0..n {
        let tenant = read_str(cur)?;
        let json = read_str(cur)?;
        let wrapped: WrappedDek = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        entries.insert(tenant, wrapped);
    }
    Ok(entries)
}

pub(crate) fn encrypted_overhead(tenant_len: usize) -> usize {
    // CRC is outside the inner plaintext. Header: tenant_len u16 + tenant + nonce + tag.
    2 + tenant_len + NONCE_LEN + TAG_LEN
}

pub(crate) fn seal_page_body(
    dek: &Dek,
    tenant: &str,
    page_index: u32,
    inner: &[u8],
) -> Result<Vec<u8>, String> {
    let aad = page_aad(tenant, page_index);
    let mut body = Vec::new();
    write_str(&mut body, tenant)?;
    body.extend_from_slice(&seal(dek, inner, &aad)?);
    Ok(body)
}

pub(crate) fn open_page_body(
    dek: &Dek,
    tenant: &str,
    page_index: u32,
    body: &[u8],
) -> Result<Vec<u8>, String> {
    let aad = page_aad(tenant, page_index);
    open(dek, body, &aad)
}

pub(crate) fn read_page_tenant(body: &[u8]) -> Result<(&str, &[u8]), String> {
    if body.len() < 2 {
        return Err("short encrypted page tenant".into());
    }
    let tenant_len = u16::from_le_bytes(body[..2].try_into().unwrap()) as usize;
    let header = 2 + tenant_len;
    if body.len() < header {
        return Err("short encrypted page tenant".into());
    }
    let tenant = std::str::from_utf8(&body[2..header]).map_err(|_| "not utf8".to_string())?;
    Ok((tenant, &body[header..]))
}

pub(crate) fn seal_blob(dek: &Dek, stamp: u32, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let aad = blob_aad(stamp);
    let mut out = Vec::from(*SEAL_MAGIC);
    out.extend_from_slice(&stamp.to_le_bytes());
    out.extend_from_slice(&seal(dek, plaintext, &aad)?);
    out.extend_from_slice(&crc32fast::hash(&out).to_le_bytes());
    Ok(out)
}

pub(crate) fn sealed_stamp(bytes: &[u8]) -> Option<u32> {
    let body = sealed_body(bytes)?;
    Some(u32::from_le_bytes(body[8..12].try_into().unwrap()))
}

fn sealed_body(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 12 + NONCE_LEN + TAG_LEN + 4 || &bytes[..8] != SEAL_MAGIC {
        return None;
    }
    let (body, crc_bytes) = bytes.split_at(bytes.len() - 4);
    let expected = u32::from_le_bytes(crc_bytes.try_into().unwrap());
    if crc32fast::hash(body) != expected {
        return None;
    }
    Some(body)
}

pub(crate) fn is_sealed(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[..8] == SEAL_MAGIC
}

pub(crate) fn is_partitioned(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[..8] == PART_MAGIC
}

pub(crate) fn encode_parts(parts: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, String> {
    let n = u16::try_from(parts.len()).map_err(|_| "too many tenant snapshot parts".to_string())?;
    let mut body = Vec::from(*PART_MAGIC);
    body.extend_from_slice(&n.to_le_bytes());
    for (tenant, blob) in parts {
        write_str(&mut body, tenant)?;
        let len =
            u32::try_from(blob.len()).map_err(|_| "tenant snapshot part too large".to_string())?;
        body.extend_from_slice(&len.to_le_bytes());
        body.extend_from_slice(blob);
    }
    Ok(body)
}

pub(crate) fn decode_parts(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if !is_partitioned(bytes) {
        return Err("not a partitioned snapshot".into());
    }
    let mut cur = &bytes[8..];
    if cur.len() < 2 {
        return Err("short partitioned snapshot".into());
    }
    let n = u16::from_le_bytes(take::<2>(&mut cur)?.try_into().unwrap()) as usize;
    let mut parts = BTreeMap::new();
    for _ in 0..n {
        let tenant = read_str(&mut cur)?;
        if cur.len() < 4 {
            return Err("short tenant snapshot part".into());
        }
        let len = u32::from_le_bytes(take::<4>(&mut cur)?.try_into().unwrap()) as usize;
        if cur.len() < len {
            return Err("short tenant snapshot part".into());
        }
        let blob = cur[..len].to_vec();
        cur = &cur[len..];
        parts.insert(tenant, blob);
    }
    if !cur.is_empty() {
        return Err("trailing partitioned snapshot bytes".into());
    }
    Ok(parts)
}

/// True when `namespace` belongs to `tenant`, or when no tenant filter is set.
pub(crate) fn matches_tenant(namespace: &str, tenant: Option<&str>) -> bool {
    match tenant {
        None => true,
        Some(want) => tenant_of_namespace(namespace).is_ok_and(|got| got == want),
    }
}

pub(crate) fn open_blob(dek: &Dek, bytes: &[u8]) -> Result<(u32, Vec<u8>), String> {
    let body = sealed_body(bytes).ok_or_else(|| "not a sealed projection".to_string())?;
    let stamp = u32::from_le_bytes(body[8..12].try_into().unwrap());
    let aad = blob_aad(stamp);
    let plain = open(dek, &body[12..], &aad)?;
    Ok((stamp, plain))
}

fn page_aad(tenant: &str, page_index: u32) -> Vec<u8> {
    let mut aad = Vec::from(*crate::LOG_FORMAT_ENCRYPTED);
    aad.extend_from_slice(&page_index.to_le_bytes());
    aad.extend_from_slice(tenant.as_bytes());
    aad
}

fn blob_aad(stamp: u32) -> Vec<u8> {
    let mut aad = Vec::from(*SEAL_MAGIC);
    aad.extend_from_slice(&stamp.to_le_bytes());
    aad
}

fn seal(dek: &Dek, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
    let cipher = Aes256Gcm::new_from_slice(dek.as_bytes()).map_err(|e| e.to_string())?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| "encrypt failed".to_string())?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn open(dek: &Dek, body: &[u8], aad: &[u8]) -> Result<Vec<u8>, String> {
    if body.len() < NONCE_LEN + TAG_LEN {
        return Err("short ciphertext".into());
    }
    let cipher = Aes256Gcm::new_from_slice(dek.as_bytes()).map_err(|e| e.to_string())?;
    let nonce = Nonce::from_slice(&body[..NONCE_LEN]);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &body[NONCE_LEN..],
                aad,
            },
        )
        .map_err(|_| "decrypt failed".to_string())
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (Arc<MemoryKeyProvider>, LogKeys) {
        let provider = Arc::new(MemoryKeyProvider::new(RuntimeProfile::Development).unwrap());
        let grant = |tenant: &str| UnwrapGrant {
            tenant_id: TenantId::new(tenant).unwrap(),
            purpose: KeyPurpose::Store,
            expires_unix_secs: i64::MAX,
        };
        let keys = LogKeys::new(
            provider.clone(),
            vec![grant(STORE_KEY_TENANT), grant("acme")],
        )
        .unwrap();
        (provider, keys)
    }

    #[test]
    fn page_round_trip_hides_plaintext() {
        let (_provider, keys) = keys();
        let dek = Dek::generate();
        let wrapped = keys.wrap("acme", &dek).unwrap();
        let opened = keys.unwrap_dek(&wrapped).unwrap().expect("granted");
        let inner = b"secret-payload-should-not-leak";
        let sealed = seal_page_body(&opened, "acme", 1, inner).unwrap();
        assert!(!sealed.windows(inner.len()).any(|w| w == inner));
        let (tenant, body) = read_page_tenant(&sealed).unwrap();
        assert_eq!(tenant, "acme");
        assert_eq!(open_page_body(&opened, tenant, 1, body).unwrap(), inner);
    }

    #[test]
    fn revoked_dek_is_unavailable() {
        let (provider, keys) = keys();
        let dek = Dek::generate();
        let wrapped = keys.wrap("acme", &dek).unwrap();
        provider.revoke_dek(&wrapped.dek_id).unwrap();
        assert!(keys.unwrap_dek(&wrapped).unwrap().is_none());
    }

    #[test]
    fn partitioned_index_round_trips() {
        let mut parts = BTreeMap::new();
        parts.insert("acme".into(), b"sealed-acme".to_vec());
        parts.insert("beta".into(), b"sealed-beta".to_vec());
        let bytes = encode_parts(&parts).unwrap();
        assert!(is_partitioned(&bytes));
        assert_eq!(decode_parts(&bytes).unwrap(), parts);
    }

    #[test]
    fn sealed_stamp_rejects_truncated_and_corrupt_envelopes() {
        let dek = Dek::generate();
        let sealed = seal_blob(&dek, 7, b"ckpt").unwrap();
        assert_eq!(sealed_stamp(&sealed), Some(7));
        assert!(sealed_stamp(&sealed[..sealed.len().saturating_sub(1)]).is_none());
        let mut corrupt = sealed.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        assert!(sealed_stamp(&corrupt).is_none());
        let (stamp, plain) = open_blob(&dek, &sealed).unwrap();
        assert_eq!(stamp, 7);
        assert_eq!(plain, b"ckpt");
    }
}
