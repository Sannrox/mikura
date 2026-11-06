//! Crash-durable publication of synced files (#341).
//!
//! A rename or hard link changes directory metadata, not the file: after a
//! power loss the new name can be gone even though the file's data was
//! synced. Publishing therefore ends with a sync of the parent directory.

#[cfg(test)]
use std::cell::RefCell;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
thread_local! {
    static SYNCED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// Rename the already synced `from` to `to`, then sync the directory of
/// `to`, so the new name survives power loss once this returns.
pub(crate) fn rename(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::rename(from, to).map_err(|e| e.to_string())?;
    sync_parent(to)
}

/// Sync the directory holding `path`, making renames, links, and removals
/// of entries in it durable.
pub(crate) fn sync_parent(path: &Path) -> Result<(), String> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    sync_dir(dir).map_err(|e| format!("sync {}: {e}", dir.display()))?;
    #[cfg(test)]
    SYNCED.with(|synced| synced.borrow_mut().push(dir.to_path_buf()));
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// Directories cannot be opened for sync here; the rename is as durable as
/// the platform makes it.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Directories this thread synced since the last call, oldest first. Crate
/// tests use it as the crash model: a name published in a directory that is
/// not listed here may be gone after power loss.
#[cfg(test)]
pub(crate) fn take_synced_dirs() -> Vec<PathBuf> {
    SYNCED.with(|synced| std::mem::take(&mut *synced.borrow_mut()))
}
