//! Concurrent readers, one writer, and per-namespace fair share (ADR 0040).

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};

/// In-flight RPCs the accept loop will run at once. Extra connections stay
/// in the OS listen backlog.
pub const DEFAULT_MAX_IN_FLIGHT: usize = 8;

pub struct FairScheduler {
    state: Mutex<State>,
    cv: Condvar,
}

struct State {
    max_reads: usize,
    inflight_reads: usize,
    by_ns: HashMap<String, usize>,
    writer_active: bool,
    writer_waiters: usize,
}

pub struct ReadPermit<'a> {
    sched: &'a FairScheduler,
    ns: String,
}

pub struct WritePermit<'a> {
    sched: &'a FairScheduler,
}

#[derive(Clone, Copy, Debug)]
pub struct SchedulerSnapshot {
    pub inflight_reads: usize,
    pub writer_waiters: usize,
    pub writer_active: bool,
}

impl FairScheduler {
    pub fn new(max_reads: usize) -> Self {
        Self {
            state: Mutex::new(State {
                max_reads: max_reads.max(1),
                inflight_reads: 0,
                by_ns: HashMap::new(),
                writer_active: false,
                writer_waiters: 0,
            }),
            cv: Condvar::new(),
        }
    }

    pub fn snapshot(&self) -> SchedulerSnapshot {
        self.state
            .lock()
            .map(|guard| SchedulerSnapshot {
                inflight_reads: guard.inflight_reads,
                writer_waiters: guard.writer_waiters,
                writer_active: guard.writer_active,
            })
            .unwrap_or(SchedulerSnapshot {
                inflight_reads: 0,
                writer_waiters: 0,
                writer_active: false,
            })
    }

    pub fn acquire_read(&self, ns: String) -> Result<ReadPermit<'_>, String> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| "scheduler lock poisoned".to_string())?;
        loop {
            if !guard.writer_active
                && guard.writer_waiters == 0
                && guard.inflight_reads < guard.max_reads
                && guard.namespace_can_take(&ns)
            {
                guard.inflight_reads += 1;
                *guard.by_ns.entry(ns.clone()).or_insert(0) += 1;
                return Ok(ReadPermit { sched: self, ns });
            }
            guard = self
                .cv
                .wait(guard)
                .map_err(|_| "scheduler lock poisoned".to_string())?;
        }
    }

    pub fn acquire_write(&self) -> Result<WritePermit<'_>, String> {
        let mut guard = self
            .state
            .lock()
            .map_err(|_| "scheduler lock poisoned".to_string())?;
        guard.writer_waiters += 1;
        loop {
            if !guard.writer_active && guard.inflight_reads == 0 {
                guard.writer_waiters -= 1;
                guard.writer_active = true;
                return Ok(WritePermit { sched: self });
            }
            guard = self
                .cv
                .wait(guard)
                .map_err(|_| "scheduler lock poisoned".to_string())?;
        }
    }
}

impl State {
    fn namespace_can_take(&self, ns: &str) -> bool {
        let mine = self.by_ns.get(ns).copied().unwrap_or(0);
        let others: usize = self
            .by_ns
            .iter()
            .filter(|(key, _)| key.as_str() != ns)
            .map(|(_, count)| *count)
            .sum();
        if others == 0 {
            return true;
        }
        mine < (self.max_reads / 2).max(1)
    }
}

impl Drop for ReadPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.sched.state.lock() {
            guard.inflight_reads = guard.inflight_reads.saturating_sub(1);
            if let Some(count) = guard.by_ns.get_mut(&self.ns) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    guard.by_ns.remove(&self.ns);
                }
            }
            self.sched.cv.notify_all();
        }
    }
}

impl Drop for WritePermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.sched.state.lock() {
            guard.writer_active = false;
            self.sched.cv.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn second_namespace_gets_a_slot_while_first_holds_half() {
        let sched = Arc::new(FairScheduler::new(4));
        let a_hold = sched.acquire_read("a".into()).unwrap();
        let a_hold2 = sched.acquire_read("a".into()).unwrap();
        let started = Instant::now();
        let sched2 = sched.clone();
        let done = thread::spawn(move || {
            let _permit = sched2.acquire_read("b".into()).unwrap();
            Instant::now()
        });
        let finished = done.join().unwrap();
        assert!(
            finished.duration_since(started) < Duration::from_millis(200),
            "namespace b starved: {:?}",
            finished.duration_since(started)
        );
        drop(a_hold);
        drop(a_hold2);
    }

    #[test]
    fn writer_waits_for_in_flight_reads() {
        let sched = FairScheduler::new(4);
        let read = sched.acquire_read("ns".into()).unwrap();
        let started = Instant::now();
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(30));
                drop(read);
            });
            let _write = sched.acquire_write().unwrap();
            assert!(started.elapsed() >= Duration::from_millis(20));
        });
    }
}
