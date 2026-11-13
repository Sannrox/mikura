//! Online backup, verify, and point-in-time restore of an object log
//! (ADR 0037, #245).
//!
//! Committed pages never change after their commit; only the superblock is
//! rewritten. A backup reads the commit point from the superblock and copies
//! the pages up to it, so it is consistent while another process writes.
//! Every commit ends on a page boundary, so a commit point names an exact
//! state. Restore always writes a new file and never touches its source.
//!
//! The log checkpoint travels with the log when it was taken at or before
//! the copied commit point: after compact it is the only holder of
//! superseded Action ids (ADR 0034). The join sidecar is rebuildable and
//! never copied.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::log::{
    decode_page, is_manifest_stub, manifest_commit_point, objects_dir, page_crc, parse_superblock,
    read_committed_page, superblock_magic, superblock_with_committed, verify_page, PAGE,
};
use crate::store::{checkpoint_stamp, Store};

/// Attempts to read a superblock that a concurrent writer may be replacing.
const SUPERBLOCK_READS: usize = 5;

/// A committed state of one log: the commit point and the stored CRC of the
/// page that ends it. The CRC ties the point to that log's history, so a
/// point from another log, or from a history that diverged, fails closed.
/// Written as `mkrp1:<commit point>:<crc hex>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestorePoint {
    pub commit_point: u32,
    pub page_crc: u32,
}

impl fmt::Display for RestorePoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mkrp1:{}:{:08x}", self.commit_point, self.page_crc)
    }
}

impl FromStr for RestorePoint {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        let bad = || format!("bad restore point {value}");
        let rest = value.strip_prefix("mkrp1:").ok_or_else(bad)?;
        let (point, crc) = rest.split_once(':').ok_or_else(bad)?;
        if crc.len() != 8 {
            return Err(bad());
        }
        Ok(Self {
            commit_point: point.parse().map_err(|_| bad())?,
            page_crc: u32::from_str_radix(crc, 16).map_err(|_| bad())?,
        })
    }
}

/// What [`verify_log`] checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyReport {
    pub restore_point: RestorePoint,
    /// Committed records that decoded.
    pub records: usize,
}

/// Copy the committed pages of `src` to a new file `dst`. `dst` must not
/// exist. Safe while a writer appends to `src`.
pub fn backup_log(src: &Path, dst: &Path) -> Result<RestorePoint, String> {
    require_absent(dst)?;
    if is_manifest_log(src)? {
        return backup_manifest_log(src, dst);
    }
    let mut source = File::open(src).map_err(|e| format!("open {}: {e}", src.display()))?;
    let (committed, page0) = read_superblock(&mut source)?;
    let superblock = superblock_with_committed(&page0, committed)?;
    write_new(dst, |out| {
        out.write_all(&superblock).map_err(|e| e.to_string())?;
        copy_pages(&mut source, out, 1, committed)
    })?;
    carry_checkpoint_or_unpublish(src, dst, committed)?;
    restore_point_at(dst, committed)
}

/// Bring the backup `dst` of `src` up to the current commit point of `src`,
/// copying only pages after the backup's own commit point. The backup's last
/// page must equal that page of `src`. A crash leaves the previous backup
/// valid: new pages land first and the superblock moves last.
pub fn backup_log_incremental(src: &Path, dst: &Path) -> Result<RestorePoint, String> {
    if same_file(src, dst)? {
        return Err("backup destination is the source log".into());
    }
    if is_manifest_log(src)? || is_manifest_log(dst).unwrap_or(false) {
        return backup_manifest_incremental(src, dst);
    }
    let held = verify_superblock_only(dst)?;
    let mut source = File::open(src).map_err(|e| format!("open {}: {e}", src.display()))?;
    let (committed, page0) = read_superblock(&mut source)?;
    if committed < held.commit_point {
        return Err(format!(
            "backup is past the source: {} > {committed}",
            held.commit_point
        ));
    }
    if held.commit_point > 0
        && page_crc(&read_page(&mut source, held.commit_point)?) != held.page_crc
    {
        return Err("backup does not belong to this log".into());
    }
    let mut out = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dst)
        .map_err(|e| e.to_string())?;
    // Bytes past the held commit point were never part of the backup:
    // readers stop at the superblock's point, which moves only after every
    // new page is synced.
    let end = PAGE as u64 * u64::from(1 + held.commit_point);
    out.set_len(end).map_err(|e| e.to_string())?;
    out.seek(SeekFrom::Start(end)).map_err(|e| e.to_string())?;
    copy_pages(&mut source, &mut out, held.commit_point + 1, committed)?;
    out.sync_data().map_err(|e| e.to_string())?;
    let superblock = superblock_with_committed(&page0, committed)?;
    out.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    out.write_all(&superblock).map_err(|e| e.to_string())?;
    out.sync_data().map_err(|e| e.to_string())?;
    carry_checkpoint(src, dst, committed)?;
    restore_point_at(dst, committed)
}

/// Check every committed page's CRC and decode every committed record, all
/// up to one commit point read once, which the report names. Read-only.
pub fn verify_log(path: &Path) -> Result<VerifyReport, String> {
    if is_manifest_log(path)? {
        return verify_manifest_log(path);
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let (committed, superblock) = read_superblock(&mut file)?;
    let encrypted = superblock_magic(&superblock)? == crate::LOG_FORMAT_ENCRYPTED;
    let mut records = 0;
    let mut last = None;
    for index in 1..=committed {
        let page = read_page(&mut file, index)?;
        if !encrypted {
            records += decode_page(&page)
                .map_err(|e| format!("committed page {index}: {e}"))?
                .len();
        }
        last = Some(page);
    }
    Ok(VerifyReport {
        restore_point: RestorePoint {
            commit_point: committed,
            page_crc: last.as_deref().map_or(0, page_crc),
        },
        records,
    })
}

/// The restore point naming commit point `commit_point` of `log`.
pub fn restore_point_at(log: &Path, commit_point: u32) -> Result<RestorePoint, String> {
    if is_manifest_log(log)? {
        let committed = manifest_commit_point(log)?.unwrap_or(0);
        if commit_point > committed {
            return Err(format!(
                "commit point {commit_point} is past committed pages {committed}"
            ));
        }
        let page_crc = if commit_point == 0 {
            0
        } else {
            page_crc(&read_committed_page(log, commit_point)?)
        };
        return Ok(RestorePoint {
            commit_point,
            page_crc,
        });
    }
    let mut file = File::open(log).map_err(|e| e.to_string())?;
    let committed = read_commit_point(&mut file)?;
    if commit_point > committed {
        return Err(format!(
            "commit point {commit_point} is past committed pages {committed}"
        ));
    }
    let page_crc = if commit_point == 0 {
        0
    } else {
        page_crc(&read_page(&mut file, commit_point)?)
    };
    Ok(RestorePoint {
        commit_point,
        page_crc,
    })
}

/// Write a new log `dst` holding `backup` up to `to` (its whole committed
/// state when `None`). `dst` must not exist; `backup` is never modified.
pub fn restore_log(
    backup: &Path,
    dst: &Path,
    to: Option<RestorePoint>,
) -> Result<RestorePoint, String> {
    require_absent(dst)?;
    let point = match to {
        Some(point) => {
            let held = restore_point_at(backup, point.commit_point)?;
            if held != point {
                return Err(format!("restore point {point} is not in this backup"));
            }
            point
        }
        None => {
            if is_manifest_log(backup)? {
                restore_point_at(backup, manifest_commit_point(backup)?.unwrap_or(0))?
            } else {
                let mut file = File::open(backup).map_err(|e| e.to_string())?;
                restore_point_at(backup, read_commit_point(&mut file)?)?
            }
        }
    };
    if is_manifest_log(backup)? {
        restore_manifest_log(backup, dst, point)?;
        carry_checkpoint_or_unpublish(backup, dst, point.commit_point)?;
        return Ok(point);
    }
    let mut source = File::open(backup).map_err(|e| e.to_string())?;
    let (_, page0) = read_superblock(&mut source)?;
    let superblock = superblock_with_committed(&page0, point.commit_point)?;
    write_new(dst, |out| {
        out.write_all(&superblock).map_err(|e| e.to_string())?;
        copy_pages(&mut source, out, 1, point.commit_point)
    })?;
    carry_checkpoint_or_unpublish(backup, dst, point.commit_point)?;
    Ok(point)
}

/// [`carry_checkpoint`] for a `dst` this call just published. On failure the
/// log is removed again, so a failed backup or restore leaves nothing behind
/// that could be opened without its checkpoint.
fn carry_checkpoint_or_unpublish(from: &Path, dst: &Path, commit_point: u32) -> Result<(), String> {
    carry_checkpoint(from, dst, commit_point).inspect_err(|_| {
        let _ = std::fs::remove_file(Store::checkpoint_path(dst));
        crate::log::remove_log(dst);
    })
}

/// Copy the newest valid log checkpoint of `from` taken at or before
/// `commit_point` next to `to`, replacing an older copy. A checkpoint past
/// the point is not usable there and is left behind.
pub(crate) fn carry_checkpoint(from: &Path, to: &Path, commit_point: u32) -> Result<(), String> {
    for path in [
        Store::checkpoint_path(from),
        Store::checkpoint_prev_path(from),
    ] {
        // A writer may replace the file at any time: check and copy the same
        // bytes.
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(stamp) = checkpoint_stamp(&bytes) else {
            continue;
        };
        if stamp > commit_point {
            continue;
        }
        let target = Store::checkpoint_path(to);
        let _ = std::fs::remove_file(Store::checkpoint_prev_path(to));
        let mut tmp = PathBuf::from(target.as_os_str());
        tmp.as_mut_os_string().push(".tmp");
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        let published = (|| {
            out.write_all(&bytes).map_err(|e| e.to_string())?;
            out.sync_all().map_err(|e| e.to_string())?;
            crate::durable::rename(&tmp, &target)
        })();
        if published.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        return published;
    }
    Ok(())
}

/// Whether two existing paths name one file, through symlinks or hard links.
fn same_file(a: &Path, b: &Path) -> Result<bool, String> {
    let (ma, mb) = (
        std::fs::metadata(a).map_err(|e| format!("{}: {e}", a.display()))?,
        std::fs::metadata(b).map_err(|e| format!("{}: {e}", b.display()))?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (ma, mb);
        Ok(std::fs::canonicalize(a).map_err(|e| e.to_string())?
            == std::fs::canonicalize(b).map_err(|e| e.to_string())?)
    }
}

fn require_absent(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    Ok(())
}

/// Write through a new `{dst}.tmp`, sync, then publish it as `dst` with a
/// hard link, which fails if `dst` appeared meanwhile: `dst` appears whole
/// or not at all and is never replaced. Returns once the link is durable; a
/// failed directory sync removes `dst` again.
fn write_new(dst: &Path, fill: impl FnOnce(&mut File) -> Result<(), String>) -> Result<(), String> {
    let mut tmp = PathBuf::from(dst.as_os_str());
    tmp.as_mut_os_string().push(".tmp");
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    let result = (|| {
        fill(&mut out)?;
        out.sync_all().map_err(|e| e.to_string())?;
        std::fs::hard_link(&tmp, dst).map_err(|e| format!("{}: {e}", dst.display()))
    })();
    let _ = std::fs::remove_file(&tmp);
    result?;
    crate::durable::sync_parent(dst).inspect_err(|_| {
        let _ = std::fs::remove_file(dst);
    })
}

/// Commit point of a log whose superblock a writer may be replacing: a torn
/// read is retried, a persistent mismatch fails closed.
fn read_superblock(file: &mut File) -> Result<(u32, Vec<u8>), String> {
    let mut last = String::new();
    for _ in 0..SUPERBLOCK_READS {
        match read_page_at(file, 0) {
            Ok(page) => match parse_superblock(&page) {
                Ok(committed) => return Ok((committed, page)),
                Err(err) => last = err,
            },
            Err(err) => last = err,
        }
    }
    Err(last)
}

fn read_commit_point(file: &mut File) -> Result<u32, String> {
    Ok(read_superblock(file)?.0)
}

/// Superblock-only check of an existing backup, for incremental appends.
fn verify_superblock_only(path: &Path) -> Result<RestorePoint, String> {
    let mut file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let committed = read_commit_point(&mut file)?;
    restore_point_at(path, committed)
}

fn read_page(file: &mut File, index: u32) -> Result<Vec<u8>, String> {
    let page = read_page_at(file, index)?;
    verify_page(&page).map_err(|e| format!("committed page {index}: {e}"))?;
    Ok(page)
}

fn read_page_at(file: &mut File, index: u32) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(PAGE as u64 * u64::from(index)))
        .map_err(|e| e.to_string())?;
    let mut page = vec![0u8; PAGE];
    file.read_exact(&mut page)
        .map_err(|_| format!("page {index}: short read"))?;
    Ok(page)
}

/// Copy verified pages `first..=last` from `source` to the current position
/// of `out`.
fn copy_pages(source: &mut File, out: &mut File, first: u32, last: u32) -> Result<(), String> {
    for index in first..=last {
        out.write_all(&read_page(source, index)?)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn is_manifest_log(path: &Path) -> Result<bool, String> {
    let mut file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut page = vec![0u8; PAGE];
    file.read_exact(&mut page)
        .map_err(|_| format!("{}: missing superblock", path.display()))?;
    Ok(is_manifest_stub(&page))
}

fn backup_manifest_log(src: &Path, dst: &Path) -> Result<RestorePoint, String> {
    require_absent(&objects_dir(dst))?;
    if let Err(error) = copy_live_objects(src, dst) {
        crate::log::remove_log(dst);
        return Err(error);
    }
    let stub = read_committed_page(src, 0)?;
    if let Err(error) = write_new(dst, |out| out.write_all(&stub).map_err(|e| e.to_string())) {
        crate::log::remove_log(dst);
        return Err(error);
    }
    if let Err(error) = verify_manifest_log(dst) {
        crate::log::remove_log(dst);
        let _ = std::fs::remove_file(Store::checkpoint_path(dst));
        return Err(error);
    }
    // The copied snapshot is the backup's commit point. Reading `src` after
    // the copy can observe a later swap and name a point the dest does not hold.
    let committed = manifest_commit_point(dst)?.unwrap_or(0);
    carry_checkpoint_or_unpublish(src, dst, committed)?;
    restore_point_at(dst, committed)
}

fn backup_manifest_incremental(src: &Path, dst: &Path) -> Result<RestorePoint, String> {
    if !is_manifest_log(src)? || !is_manifest_log(dst)? {
        return Err("backup does not belong to this log".into());
    }
    let held = restore_point_at(dst, manifest_commit_point(dst)?.unwrap_or(0))?;
    let source_committed = manifest_commit_point(src)?.unwrap_or(0);
    if source_committed < held.commit_point {
        return Err(format!(
            "backup is past the source: {} > {source_committed}",
            held.commit_point
        ));
    }
    if held.commit_point > 0 {
        let src_crc = page_crc(&read_committed_page(src, held.commit_point)?);
        if src_crc != held.page_crc {
            return Err("backup does not belong to this log".into());
        }
    }
    copy_live_objects(src, dst)?;
    verify_manifest_log(dst)?;
    let committed = manifest_commit_point(dst)?.unwrap_or(0);
    carry_checkpoint(src, dst, committed)?;
    restore_point_at(dst, committed)
}

fn verify_manifest_log(path: &Path) -> Result<VerifyReport, String> {
    let stub = read_committed_page(path, 0)?;
    let encrypted = stub.get(4 + 8 + 2).copied().unwrap_or(0) & 1 != 0;
    let committed = manifest_commit_point(path)?.unwrap_or(0);
    let mut records = 0;
    let mut last = None;
    for index in 1..=committed {
        let page = read_committed_page(path, index)?;
        if !encrypted {
            records += decode_page(&page)
                .map_err(|e| format!("committed page {index}: {e}"))?
                .len();
        }
        last = Some(page);
    }
    Ok(VerifyReport {
        restore_point: RestorePoint {
            commit_point: committed,
            page_crc: last.as_deref().map_or(0, page_crc),
        },
        records,
    })
}

fn restore_manifest_log(backup: &Path, dst: &Path, point: RestorePoint) -> Result<(), String> {
    require_absent(&objects_dir(dst))?;
    let committed = manifest_commit_point(backup)?.unwrap_or(0);
    if point.commit_point == committed {
        if let Err(error) = copy_live_objects(backup, dst) {
            crate::log::remove_log(dst);
            return Err(error);
        }
        let stub = read_committed_page(backup, 0)?;
        if let Err(error) = write_new(dst, |out| out.write_all(&stub).map_err(|e| e.to_string())) {
            crate::log::remove_log(dst);
            return Err(error);
        }
        return Ok(());
    }
    if let Err(error) = restore_manifest_prefix(backup, dst, point.commit_point) {
        crate::log::remove_log(dst);
        return Err(error);
    }
    Ok(())
}

fn restore_manifest_prefix(backup: &Path, dst: &Path, commit_point: u32) -> Result<(), String> {
    let stub = read_committed_page(backup, 0)?;
    let encrypted = stub.get(4 + 8 + 2).copied().unwrap_or(0) & 1 != 0;
    let src = crate::objects::FileStore::open(&objects_dir(backup), false)?;
    let mut manifest = match src.current()? {
        Some((version, bytes)) => {
            let mut parsed = crate::objects::decode_manifest(&bytes)?;
            parsed.version = version;
            parsed
        }
        None if commit_point == 0 => crate::objects::Manifest::empty(encrypted),
        None => return Err("committed pages missing".into()),
    };
    if !manifest.prefix_key.is_empty() {
        let prefix = src.get(&manifest.prefix_key)?;
        manifest.prefix_segments = crate::objects::decode_manifest(&prefix)?.segments;
    }
    let dest = crate::objects::FileStore::create(&objects_dir(dst), true)?;
    let mut kept = Vec::new();
    let mut segments = manifest.prefix_segments.clone();
    segments.extend(manifest.segments.iter().cloned());
    for segment in &segments {
        if segment.first_page > commit_point {
            continue;
        }
        let last = segment.first_page + segment.page_count - 1;
        let bytes = src.get(&segment.key)?;
        if last <= commit_point {
            dest.put(&segment.key, &bytes)?;
            kept.push(segment.clone());
            continue;
        }
        let keep = commit_point - segment.first_page + 1;
        let end = keep as usize * PAGE;
        if bytes.len() < end {
            return Err("committed pages missing".into());
        }
        dest.put(&segment.key, &bytes[..end])?;
        kept.push(crate::objects::Segment {
            key: segment.key.clone(),
            first_page: segment.first_page,
            page_count: keep,
        });
    }
    manifest.segments = kept;
    manifest.prefix_segments.clear();
    manifest.prefix_key.clear();
    manifest.checkpoint_key.clear();
    manifest.projection_key.clear();
    manifest.checkpoint_pages = 0;
    manifest.committed_pages = commit_point;
    manifest.version = 0;
    dest.cas(0, &crate::objects::encode_manifest(&manifest)?)?;
    write_new(dst, |out| out.write_all(&stub).map_err(|e| e.to_string()))?;
    verify_manifest_log(dst).map(|_| ())
}

fn copy_live_objects(src: &Path, dst: &Path) -> Result<(), String> {
    let from = objects_dir(src);
    let to = objects_dir(dst);
    std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
    let source = crate::objects::FileStore::open(&from, false)?;
    let dest = crate::objects::FileStore::open(&to, true)?;
    let live = match source.current()? {
        Some((version, bytes)) => {
            let mut manifest = crate::objects::decode_manifest(&bytes)?;
            manifest.version = version;
            if !manifest.prefix_key.is_empty() {
                let prefix = source.get(&manifest.prefix_key)?;
                manifest.prefix_segments = crate::objects::decode_manifest(&prefix)?.segments;
            }
            manifest.live_keys()
        }
        None => Default::default(),
    };
    let mut keys: Vec<_> = live.into_iter().collect();
    keys.sort_by_key(|key| key.starts_with("manifest."));
    for key in keys {
        let bytes = source.get(&key)?;
        match dest.put(&key, &bytes) {
            Ok(_) => {}
            Err(error) if error.contains("already exists") => {
                if dest.get(&key)? != bytes {
                    return Err(format!("backup object {key} does not match the source"));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
