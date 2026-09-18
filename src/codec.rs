use std::collections::HashSet;

pub(crate) fn write_str(body: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let len = u16::try_from(value.len()).map_err(|_| "string too long".to_string())?;
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(value.as_bytes());
    Ok(())
}

pub(crate) fn read_str(cur: &mut &[u8]) -> Result<String, String> {
    let len = u16::from_le_bytes(take::<2>(cur)?.try_into().unwrap()) as usize;
    if cur.len() < len {
        return Err("short string".into());
    }
    let (head, rest) = cur.split_at(len);
    *cur = rest;
    String::from_utf8(head.to_vec()).map_err(|_| "not utf8".into())
}

pub(crate) fn read_u32(cur: &mut &[u8]) -> Result<u32, String> {
    Ok(u32::from_le_bytes(take::<4>(cur)?.try_into().unwrap()))
}

pub(crate) fn read_u64(cur: &mut &[u8]) -> Result<u64, String> {
    Ok(u64::from_le_bytes(take::<8>(cur)?.try_into().unwrap()))
}

pub(crate) fn take<'a, const N: usize>(cur: &mut &'a [u8]) -> Result<&'a [u8], String> {
    if cur.len() < N {
        return Err("short body".into());
    }
    let (head, rest) = cur.split_at(N);
    *cur = rest;
    Ok(head)
}

pub(crate) fn split_unique_csv(
    raw: &str,
    empty_entry: &str,
    duplicate: impl Fn(&str) -> String,
) -> Result<Vec<String>, String> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for part in raw.split(',') {
        let token = part.trim();
        if token.is_empty() {
            return Err(empty_entry.into());
        }
        if !seen.insert(token) {
            return Err(duplicate(token));
        }
        out.push(token.to_string());
    }
    Ok(out)
}
