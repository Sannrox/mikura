//! Snapshot-page cursors for evaluate (ADR 0015).
//!
//! The token is opaque. It binds the deny list, the query (including sort
//! and page size), and the live writer stamp (`committed_pages`, written
//! pages, current page used). A different view or a later write, including
//! an uncommitted stream append, fails closed.

use crate::acl::PropertyAcl;
use crate::objectset::{EvaluateRequest, Hop, Predicate, Sort};

const CURSOR_PREFIX: &str = "mkc1:";

/// The last row of a page: its key and, per sort key, whether the value is
/// missing and the stored value.
pub(crate) struct PageCursor {
    pub after_key: String,
    pub after: Vec<(bool, String)>,
}

/// Version 2 stores one position per sort key (ADR 0039); version 1 tokens
/// still decode as one key.
pub(crate) fn encode_cursor(
    request: &EvaluateRequest,
    stamp: (u32, u32, u32),
    after_key: &str,
    after: &[(bool, String)],
) -> String {
    let mut body = Vec::new();
    body.push(2);
    body.extend_from_slice(&stamp.0.to_le_bytes());
    body.extend_from_slice(&stamp.1.to_le_bytes());
    body.extend_from_slice(&stamp.2.to_le_bytes());
    let fingerprint = query_fingerprint(request);
    write_bytes(&mut body, &fingerprint);
    write_str(&mut body, after_key);
    body.push(after.len() as u8);
    for (missing, value) in after {
        body.push(u8::from(*missing));
        write_str(&mut body, value);
    }
    format!("{CURSOR_PREFIX}{}", hex_encode(&body))
}

pub(crate) fn decode_cursor(
    token: &str,
    request: &EvaluateRequest,
    stamp: (u32, u32, u32),
) -> Result<PageCursor, String> {
    let hex = token
        .strip_prefix(CURSOR_PREFIX)
        .ok_or_else(|| page_err("cursor is not a snapshot token"))?;
    let body = hex_decode(hex)?;
    let mut cur = body.as_slice();
    let version = read_u8(&mut cur)?;
    if version != 1 && version != 2 {
        return Err(page_err("cursor version is not supported"));
    }
    let committed = read_u32(&mut cur)?;
    let written = read_u32(&mut cur)?;
    let used = read_u32(&mut cur)?;
    let fingerprint = read_bytes(&mut cur)?;
    let after_key = read_str(&mut cur)?;
    let parts = if version == 1 { 1 } else { read_u8(&mut cur)? };
    let mut after = Vec::with_capacity(usize::from(parts));
    for _ in 0..parts {
        let missing = match read_u8(&mut cur)? {
            0 => false,
            1 => true,
            _ => return Err(page_err("cursor is malformed")),
        };
        after.push((missing, read_str(&mut cur)?));
    }
    if !cur.is_empty() {
        return Err(page_err("cursor is malformed"));
    }
    if (committed, written, used) != stamp {
        return Err(page_err("cursor snapshot is not the current log head"));
    }
    if fingerprint != query_fingerprint(request) {
        return Err(page_err("cursor does not match this query or restriction"));
    }
    Ok(PageCursor { after_key, after })
}

fn query_fingerprint(request: &EvaluateRequest) -> Vec<u8> {
    let mut out = Vec::new();
    write_acl(&mut out, &request.acl);
    write_str(&mut out, &request.namespace);
    write_str(&mut out, &request.root_kind);
    out.extend_from_slice(&(request.hops.len() as u32).to_le_bytes());
    for hop in &request.hops {
        write_hop(&mut out, hop);
    }
    write_str(&mut out, &request.sum_kind);
    write_str(&mut out, &request.sum_property);
    match &request.filter {
        None => out.push(0),
        Some(filter) => {
            out.push(1);
            write_str(&mut out, &filter.property);
            write_str(&mut out, &filter.value);
        }
    }
    match &request.predicate {
        None => out.push(0),
        Some(pred) => {
            out.push(1);
            write_predicate(&mut out, pred);
        }
    }
    match &request.sort {
        None => out.push(0),
        Some(sort) => {
            out.push(1);
            write_sort(&mut out, sort);
        }
    }
    // A version 1 cursor came from a request without further keys.
    if !request.then_by.is_empty() {
        out.push(u8::try_from(request.then_by.len()).unwrap_or(u8::MAX));
        for sort in &request.then_by {
            write_sort(&mut out, sort);
        }
    }
    out.extend_from_slice(&(request.object_bound as u64).to_le_bytes());
    out.extend_from_slice(&(request.page_size as u64).to_le_bytes());
    out
}

fn write_acl(out: &mut Vec<u8>, acl: &PropertyAcl) {
    let denied = acl.denied_sorted();
    out.extend_from_slice(&(denied.len() as u32).to_le_bytes());
    for (kind, property) in denied {
        write_str(out, kind);
        write_str(out, property);
    }
    let kinds = acl.hide_kinds_sorted();
    out.extend_from_slice(&(kinds.len() as u32).to_le_bytes());
    for kind in kinds {
        write_str(out, kind);
    }
    let identities = acl.hide_identities_sorted();
    out.extend_from_slice(&(identities.len() as u32).to_le_bytes());
    for (kind, key) in identities {
        write_str(out, kind);
        write_str(out, key);
    }
    let terms = acl.row_filters_sorted();
    out.extend_from_slice(&(terms.len() as u32).to_le_bytes());
    for (kind, property, values) in terms {
        write_str(out, kind);
        write_str(out, property);
        out.extend_from_slice(&(values.len() as u32).to_le_bytes());
        for value in values {
            write_str(out, value);
        }
    }
}

fn write_hop(out: &mut Vec<u8>, hop: &Hop) {
    write_str(out, &hop.far_kind);
    write_str(out, &hop.join_property);
    out.push(u8::from(hop.incoming));
    match &hop.predicate {
        None => out.push(0),
        Some(pred) => {
            out.push(1);
            write_predicate(out, pred);
        }
    }
}

fn write_sort(out: &mut Vec<u8>, sort: &Sort) {
    write_str(out, &sort.property);
    out.push(u8::from(sort.descending));
    if let Some(nearest) = &sort.nearest {
        write_opt_str(out, Some(nearest));
    }
}

fn write_predicate(out: &mut Vec<u8>, pred: &Predicate) {
    match pred {
        Predicate::Eq { property, value } => {
            out.push(0);
            write_str(out, property);
            write_str(out, value);
        }
        Predicate::Neq { property, value } => {
            out.push(1);
            write_str(out, property);
            write_str(out, value);
        }
        Predicate::Range { property, min, max } => {
            out.push(2);
            write_str(out, property);
            write_opt_str(out, min.as_deref());
            write_opt_str(out, max.as_deref());
        }
        Predicate::Missing { property } => {
            out.push(3);
            write_str(out, property);
        }
        Predicate::And(args) => {
            out.push(4);
            out.extend_from_slice(&(args.len() as u32).to_le_bytes());
            for arg in args {
                write_predicate(out, arg);
            }
        }
        Predicate::Or(args) => {
            out.push(5);
            out.extend_from_slice(&(args.len() as u32).to_le_bytes());
            for arg in args {
                write_predicate(out, arg);
            }
        }
        Predicate::Not(inner) => {
            out.push(6);
            write_predicate(out, inner);
        }
        Predicate::Bbox {
            property,
            min_lat,
            min_lon,
            max_lat,
            max_lon,
        } => {
            out.push(7);
            write_str(out, property);
            write_str(out, min_lat);
            write_str(out, min_lon);
            write_str(out, max_lat);
            write_str(out, max_lon);
        }
    }
}

fn write_opt_str(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        None => out.push(0),
        Some(value) => {
            out.push(1);
            write_str(out, value);
        }
    }
}

fn write_str(out: &mut Vec<u8>, value: &str) {
    write_bytes(out, value.as_bytes());
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
}

fn read_u8(cur: &mut &[u8]) -> Result<u8, String> {
    let (head, rest) = cur
        .split_first()
        .ok_or_else(|| page_err("cursor is truncated"))?;
    *cur = rest;
    Ok(*head)
}

fn read_u32(cur: &mut &[u8]) -> Result<u32, String> {
    if cur.len() < 4 {
        return Err(page_err("cursor is truncated"));
    }
    let (head, rest) = cur.split_at(4);
    *cur = rest;
    Ok(u32::from_le_bytes(head.try_into().unwrap()))
}

fn read_bytes(cur: &mut &[u8]) -> Result<Vec<u8>, String> {
    let len = read_u32(cur)? as usize;
    if cur.len() < len {
        return Err(page_err("cursor is truncated"));
    }
    let (head, rest) = cur.split_at(len);
    *cur = rest;
    Ok(head.to_vec())
}

fn read_str(cur: &mut &[u8]) -> Result<String, String> {
    String::from_utf8(read_bytes(cur)?).map_err(|_| page_err("cursor is malformed"))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err(page_err("cursor is malformed"));
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    for pair in bytes.as_chunks::<2>().0 {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(page_err("cursor is malformed")),
    }
}

fn page_err(message: &str) -> String {
    message.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version 1 token (one sort key, no part count) still decodes.
    #[test]
    fn version_one_cursors_decode_as_one_key() {
        let request = EvaluateRequest {
            sort: Some(Sort {
                property: "status".into(),
                descending: false,
                nearest: None,
            }),
            ..Default::default()
        };
        let stamp: (u32, u32, u32) = (3, 3, 0);
        let mut body = vec![1];
        for part in [stamp.0, stamp.1, stamp.2] {
            body.extend_from_slice(&part.to_le_bytes());
        }
        write_bytes(&mut body, &query_fingerprint(&request));
        write_str(&mut body, "o2");
        body.push(0);
        write_str(&mut body, "closed");
        let token = format!("{CURSOR_PREFIX}{}", hex_encode(&body));
        let cursor = decode_cursor(&token, &request, stamp).unwrap();
        assert_eq!(cursor.after_key, "o2");
        assert_eq!(cursor.after, [(false, "closed".to_string())]);
        let current = encode_cursor(&request, stamp, "o2", &cursor.after);
        assert_eq!(
            decode_cursor(&current, &request, stamp).unwrap().after,
            cursor.after
        );
    }
}
