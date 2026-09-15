//! 4KiB checksummed pages + group-commit pointer.
//! Rebuild trusts only pages 1..=committed.

use crc32fast::Hasher as Crc;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::store::ObjectRecord;

pub const PAGE: usize = 4096;
const MAGIC: &[u8; 8] = b"MIKURAV1";
const CRC_LEN: usize = 4;
const USED_LEN: usize = 2;
const PAGE_HDR: usize = CRC_LEN + USED_LEN;
const SUPER_COMMIT_OFF: usize = CRC_LEN + 8 + 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncPolicy {
    None,
    Page,
    Group(u32),
}

impl SyncPolicy {
    fn durable(self) -> bool {
        !matches!(self, Self::None)
    }

    fn group_size(self) -> u32 {
        match self {
            Self::None => u32::MAX,
            Self::Page => 1,
            Self::Group(n) => n,
        }
    }
}

pub struct LogWriter {
    file: File,
    page: Vec<u8>,
    used: usize,
    written: u32,
    committed: u32,
    sync: SyncPolicy,
    fsync_count: u64,
}

impl LogWriter {
    pub fn create(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let file = File::create(path).map_err(|e| e.to_string())?;
        let mut writer = Self {
            file,
            page: vec![0u8; PAGE],
            used: 0,
            written: 0,
            committed: 0,
            sync,
            fsync_count: 0,
        };
        writer.write_superblock()?;
        Ok(writer)
    }

    pub fn open(path: &Path, sync: SyncPolicy) -> Result<Self, String> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        let committed = read_committed(&mut file)?;
        let end = PAGE as u64 * u64::from(1 + committed);
        file.seek(SeekFrom::Start(end)).map_err(|e| e.to_string())?;
        Ok(Self {
            file,
            page: vec![0u8; PAGE],
            used: 0,
            written: committed,
            committed,
            sync,
            fsync_count: 0,
        })
    }

    #[cfg(test)]
    pub fn fsync_count(&self) -> u64 {
        self.fsync_count
    }

    pub fn append_record(&mut self, record: &ObjectRecord) -> Result<(), String> {
        let body = encode_body(record)?;
        let framed = framed_record(&body)?;
        if PAGE_HDR + framed.len() > PAGE {
            return Err("record larger than a page".into());
        }
        if PAGE_HDR + self.used + framed.len() > PAGE {
            self.flush_page()?;
        }
        let start = PAGE_HDR + self.used;
        self.page[start..start + framed.len()].copy_from_slice(&framed);
        self.used += framed.len();
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), String> {
        self.flush_page()?;
        self.commit_pending()?;
        self.file.flush().map_err(|e| e.to_string())
    }

    fn flush_page(&mut self) -> Result<(), String> {
        if self.used == 0 {
            return Ok(());
        }
        self.page[CRC_LEN..PAGE_HDR].copy_from_slice(&(self.used as u16).to_le_bytes());
        seal_page(&mut self.page);
        self.file.write_all(&self.page).map_err(|e| e.to_string())?;
        self.written += 1;
        self.page = vec![0u8; PAGE];
        self.used = 0;
        if self.sync == SyncPolicy::None {
            self.committed = self.written;
            return self.write_superblock();
        }
        let pending = self.written - self.committed;
        if pending >= self.sync.group_size() {
            self.commit_pending()?;
        }
        Ok(())
    }

    fn commit_pending(&mut self) -> Result<(), String> {
        if self.written == self.committed {
            return Ok(());
        }
        self.sync_data()?;
        self.committed = self.written;
        self.write_superblock()
    }

    fn write_superblock(&mut self) -> Result<(), String> {
        let mut superblock = vec![0u8; PAGE];
        superblock[CRC_LEN..CRC_LEN + 8].copy_from_slice(MAGIC);
        superblock[CRC_LEN + 8..CRC_LEN + 10].copy_from_slice(&(PAGE as u16).to_le_bytes());
        superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4]
            .copy_from_slice(&self.committed.to_le_bytes());
        seal_page(&mut superblock);
        let pos = self.file.stream_position().map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        self.file
            .write_all(&superblock)
            .map_err(|e| e.to_string())?;
        self.sync_data()?;
        self.file
            .seek(SeekFrom::Start(pos.max(PAGE as u64)))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn sync_data(&mut self) -> Result<(), String> {
        if self.sync.durable() {
            self.file.sync_data().map_err(|e| e.to_string())?;
            self.fsync_count += 1;
        }
        Ok(())
    }
}

pub fn read_records(path: &Path) -> Result<Vec<ObjectRecord>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() < PAGE {
        return Err("missing superblock".into());
    }
    let superblock = &bytes[..PAGE];
    verify_page(superblock).map_err(|e| format!("superblock: {e}"))?;
    if &superblock[CRC_LEN..CRC_LEN + 8] != MAGIC {
        return Err("bad magic".into());
    }
    let committed = u32::from_le_bytes(
        superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let needed = PAGE * (1 + committed);
    if bytes.len() < needed {
        return Err("committed pages missing".into());
    }
    let mut records = Vec::new();
    for index in 1..=committed {
        let page = &bytes[index * PAGE..(index + 1) * PAGE];
        verify_page(page).map_err(|e| format!("committed page {index}: {e}"))?;
        decode_data_page(page, &mut records)?;
    }
    Ok(records)
}

fn read_committed(file: &mut File) -> Result<u32, String> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut superblock = vec![0u8; PAGE];
    std::io::Read::read_exact(file, &mut superblock).map_err(|e| e.to_string())?;
    verify_page(&superblock).map_err(|e| format!("superblock: {e}"))?;
    if &superblock[CRC_LEN..CRC_LEN + 8] != MAGIC {
        return Err("bad magic".into());
    }
    Ok(u32::from_le_bytes(
        superblock[SUPER_COMMIT_OFF..SUPER_COMMIT_OFF + 4]
            .try_into()
            .unwrap(),
    ))
}

fn seal_page(page: &mut [u8]) {
    let mut hasher = Crc::new();
    hasher.update(&page[CRC_LEN..]);
    page[..CRC_LEN].copy_from_slice(&hasher.finalize().to_le_bytes());
}

fn verify_page(page: &[u8]) -> Result<(), String> {
    if page.len() != PAGE {
        return Err("short page".into());
    }
    let expected = u32::from_le_bytes(page[..CRC_LEN].try_into().unwrap());
    let mut hasher = Crc::new();
    hasher.update(&page[CRC_LEN..]);
    if hasher.finalize() != expected {
        return Err("checksum mismatch".into());
    }
    Ok(())
}

fn framed_record(body: &[u8]) -> Result<Vec<u8>, String> {
    let len = u16::try_from(body.len()).map_err(|_| "record too large".to_string())?;
    let mut out = Vec::with_capacity(2 + body.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

fn encode_body(record: &ObjectRecord) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    body.extend_from_slice(&record.gen.to_le_bytes());
    body.push(u8::from(record.hidden));
    write_str(&mut body, &record.kind)?;
    write_str(&mut body, &record.key)?;
    let nprops = u16::try_from(record.props.len()).map_err(|_| "too many props".to_string())?;
    body.extend_from_slice(&nprops.to_le_bytes());
    let mut keys: Vec<_> = record.props.keys().cloned().collect();
    keys.sort();
    for key in keys {
        write_str(&mut body, &key)?;
        write_str(&mut body, record.props.get(&key).expect("prop"))?;
    }
    Ok(body)
}

fn write_str(body: &mut Vec<u8>, value: &str) -> Result<(), String> {
    let len = u16::try_from(value.len()).map_err(|_| "string too long".to_string())?;
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(value.as_bytes());
    Ok(())
}

fn decode_data_page(page: &[u8], records: &mut Vec<ObjectRecord>) -> Result<(), String> {
    let used = u16::from_le_bytes(page[CRC_LEN..PAGE_HDR].try_into().unwrap()) as usize;
    if PAGE_HDR + used > PAGE {
        return Err("used past page".into());
    }
    let mut cur = &page[PAGE_HDR..PAGE_HDR + used];
    while !cur.is_empty() {
        if cur.len() < 2 {
            return Err("short record len".into());
        }
        let len = u16::from_le_bytes(cur[..2].try_into().unwrap()) as usize;
        cur = &cur[2..];
        if cur.len() < len {
            return Err("short record body".into());
        }
        records.push(decode_body(&cur[..len])?);
        cur = &cur[len..];
    }
    Ok(())
}

fn decode_body(body: &[u8]) -> Result<ObjectRecord, String> {
    let mut cur = body;
    let gen = u64::from_le_bytes(take::<8>(&mut cur)?.try_into().unwrap());
    let hidden = take::<1>(&mut cur)?[0] != 0;
    let kind = read_str(&mut cur)?;
    let key = read_str(&mut cur)?;
    let nprops = u16::from_le_bytes(take::<2>(&mut cur)?.try_into().unwrap()) as usize;
    let mut props = HashMap::new();
    for _ in 0..nprops {
        let k = read_str(&mut cur)?;
        let v = read_str(&mut cur)?;
        props.insert(k, v);
    }
    if !cur.is_empty() {
        return Err("trailing body bytes".into());
    }
    Ok(ObjectRecord {
        gen,
        kind,
        key,
        hidden,
        props,
    })
}

fn take<'a, const N: usize>(cur: &mut &'a [u8]) -> Result<&'a [u8], String> {
    if cur.len() < N {
        return Err("short body".into());
    }
    let (head, rest) = cur.split_at(N);
    *cur = rest;
    Ok(head)
}

fn read_str(cur: &mut &[u8]) -> Result<String, String> {
    let len = u16::from_le_bytes(take::<2>(cur)?.try_into().unwrap()) as usize;
    if cur.len() < len {
        return Err("short string".into());
    }
    let (head, rest) = cur.split_at(len);
    *cur = rest;
    String::from_utf8(head.to_vec()).map_err(|_| "not utf8".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_page_is_not_authority() {
        let dir = std::env::temp_dir().join("mikura-log-orphan");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.mikura");
        let mut writer = LogWriter::create(&path, SyncPolicy::None).unwrap();
        writer
            .append_record(&ObjectRecord {
                gen: 1,
                kind: "Customer".into(),
                key: "c1".into(),
                hidden: false,
                props: HashMap::from([("region".into(), "eu".into())]),
            })
            .unwrap();
        writer.flush().unwrap();
        let before = read_records(&path).unwrap();
        assert_eq!(before.len(), 1);
        let mut bytes = std::fs::read(&path).unwrap();
        let mut page = vec![0u8; PAGE];
        page[CRC_LEN..PAGE_HDR].copy_from_slice(&0u16.to_le_bytes());
        seal_page(&mut page);
        bytes.extend_from_slice(&page);
        std::fs::write(&path, bytes).unwrap();
        let after = read_records(&path).unwrap();
        assert_eq!(after.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_committed_page_fails_closed() {
        let dir = std::env::temp_dir().join("mikura-log-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("objects.mikura");
        let mut writer = LogWriter::create(&path, SyncPolicy::None).unwrap();
        writer
            .append_record(&ObjectRecord {
                gen: 1,
                kind: "Customer".into(),
                key: "c1".into(),
                hidden: false,
                props: HashMap::from([("region".into(), "eu".into())]),
            })
            .unwrap();
        writer.flush().unwrap();
        assert_eq!(read_records(&path).unwrap().len(), 1);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() >= PAGE * 2);
        std::fs::write(&path, &bytes[..PAGE]).unwrap();
        let err = read_records(&path).unwrap_err();
        assert!(
            err.contains("missing") || err.contains("committed"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
