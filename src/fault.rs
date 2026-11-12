//! Test-edge IO interceptor for the seeded recovery harness (#345).
//!
//! Production builds call through to the real syscall. Tests may install a
//! per-thread plan that fails, delays, or crashes at a generated step.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Write,
    Sync,
    Truncate,
    Rename,
    Open,
}

#[cfg(test)]
pub(crate) const FAILURE: &str = "injected failure";
#[cfg(test)]
pub(crate) const CRASH: &str = "injected crash";

#[inline]
pub(crate) fn run<T>(op: Op, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    #[cfg(test)]
    {
        live::run(op, f)
    }
    #[cfg(not(test))]
    {
        let _ = op;
        f()
    }
}

pub(crate) fn write_all(file: &mut impl Write, buf: &[u8]) -> Result<(), String> {
    run(Op::Write, || file.write_all(buf).map_err(|e| e.to_string()))
}

pub(crate) fn sync_data(file: &File) -> Result<(), String> {
    run(Op::Sync, || file.sync_data().map_err(|e| e.to_string()))
}

pub(crate) fn set_len(file: &File, len: u64) -> Result<(), String> {
    run(Op::Truncate, || {
        file.set_len(len).map_err(|e| e.to_string())
    })
}

pub(crate) fn open_rw(path: &Path) -> Result<File, String> {
    run(Op::Open, || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())
    })
}

pub(crate) fn open_rw_create(path: &Path) -> Result<File, String> {
    run(Op::Open, || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| e.to_string())
    })
}

pub(crate) fn create(path: &Path) -> Result<File, String> {
    run(Op::Open, || File::create(path).map_err(|e| e.to_string()))
}

pub(crate) fn open_append(path: &Path) -> Result<File, String> {
    run(Op::Open, || {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| e.to_string())
    })
}

pub(crate) fn open_read(path: &Path) -> Result<File, String> {
    run(Op::Open, || File::open(path).map_err(|e| e.to_string()))
}

pub(crate) fn crashed() -> bool {
    #[cfg(test)]
    {
        live::crashed()
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// Treat extra pages past the superblock as committed. Reintroduces the
/// orphan-page-as-authority bug so the harness can re-find it.
#[cfg(test)]
pub(crate) fn maybe_orphan_pages(committed: u32, file_len: u64, page: usize) -> u32 {
    live::maybe_orphan_pages(committed, file_len, page)
}

#[cfg(test)]
pub(crate) use live::{allow_recovery, install, take_trace, FaultKind, Plan, CI_SEEDS};

#[cfg(test)]
mod live {
    use super::*;
    use std::cell::{Cell, RefCell};

    pub const FAIL_BEFORE: &str = "fail-before";
    pub const FAIL_AFTER: &str = "fail-after";
    pub const DELAY: &str = "delay";
    pub const CI_SEEDS: u64 = 32;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum FaultKind {
        FailBefore,
        FailAfter,
        Delay,
        Crash,
    }

    impl FaultKind {
        pub fn from_seed(seed: u64) -> Self {
            match seed % 4 {
                0 => Self::FailBefore,
                1 => Self::FailAfter,
                2 => Self::Delay,
                _ => Self::Crash,
            }
        }

        pub fn label(self) -> &'static str {
            match self {
                Self::FailBefore => FAIL_BEFORE,
                Self::FailAfter => FAIL_AFTER,
                Self::Delay => DELAY,
                Self::Crash => super::CRASH,
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    pub struct Plan {
        pub step: u64,
        pub kind: FaultKind,
    }

    impl Plan {
        pub fn from_seed(seed: u64, ops: u64) -> Option<Self> {
            if ops == 0 {
                return None;
            }
            Some(Self {
                step: seed % ops,
                // Kind advances on wrap so a step is not glued to one
                // FaultKind when `ops` is a multiple of four.
                kind: FaultKind::from_seed(seed / ops + seed),
            })
        }
    }

    thread_local! {
        static PLAN: Cell<Option<Plan>> = const { Cell::new(None) };
        static STEP: Cell<u64> = const { Cell::new(0) };
        static CRASHED: Cell<bool> = const { Cell::new(false) };
        static ORPHANS: Cell<bool> = const { Cell::new(false) };
        static TRACE: RefCell<Vec<(Op, Option<&'static str>)>> = const { RefCell::new(Vec::new()) };
    }

    pub struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            PLAN.with(|p| p.set(None));
            STEP.with(|s| s.set(0));
            CRASHED.with(|c| c.set(false));
            ORPHANS.with(|o| o.set(false));
            TRACE.with(|t| t.borrow_mut().clear());
        }
    }

    /// Install a plan (or record-only when `plan` is `None`). Clears on drop.
    pub fn install(plan: Option<Plan>, orphans: bool) -> Guard {
        PLAN.with(|p| p.set(plan));
        STEP.with(|s| s.set(0));
        CRASHED.with(|c| c.set(false));
        ORPHANS.with(|o| o.set(orphans));
        TRACE.with(|t| t.borrow_mut().clear());
        Guard
    }

    pub fn take_trace() -> Vec<(Op, Option<&'static str>)> {
        TRACE.with(|t| t.borrow().clone())
    }

    pub fn crashed() -> bool {
        CRASHED.with(|c| c.get())
    }

    /// Clear the crash latch so recovery open can read the files a kill
    /// would have left. The plan stays consumed.
    pub fn allow_recovery() {
        CRASHED.with(|c| c.set(false));
    }

    pub fn maybe_orphan_pages(committed: u32, file_len: u64, page: usize) -> u32 {
        if !ORPHANS.with(|o| o.get()) {
            return committed;
        }
        let pages = file_len
            .checked_div(page as u64)
            .unwrap_or(0)
            .saturating_sub(1) as u32;
        pages.max(committed)
    }

    pub fn run<T>(op: Op, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        if crashed() {
            return Err(CRASH.into());
        }
        let step = STEP.with(|s| {
            let n = s.get();
            s.set(n + 1);
            n
        });
        let plan = PLAN.with(|p| p.get());
        let applied = plan.filter(|p| p.step == step).map(|p| p.kind);
        TRACE.with(|t| t.borrow_mut().push((op, applied.map(FaultKind::label))));
        match applied {
            // Delay occupies a plan slot and is recorded in the replay
            // trace. It does not sleep: wall-clock waits are neither
            // deterministic nor safe next to concurrent agents.
            None | Some(FaultKind::Delay) => f(),
            Some(FaultKind::FailBefore) => Err(FAILURE.into()),
            Some(FaultKind::FailAfter) => {
                f()?;
                Err(FAILURE.into())
            }
            Some(FaultKind::Crash) => {
                let result = f();
                CRASHED.with(|c| c.set(true));
                result.and(Err(CRASH.into()))
            }
        }
    }
}
