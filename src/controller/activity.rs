use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::time::mono_ms;

/// Lock-free request accounting shared between request handlers and the group actor.
#[derive(Debug, Default)]
pub struct Activity {
    /// Monotonic ms of the last request; 0 = never.
    last: AtomicU64,
    /// Open connections / in-flight requests (native proxy only).
    inflight: AtomicU64,
    requests: AtomicU64,
    /// Requests currently held while the group wakes up.
    waiters: AtomicUsize,
}

impl Activity {
    pub fn touch(&self) {
        self.last.store(mono_ms(), Ordering::Relaxed);
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Mark activity without counting a request (used when a group becomes running so the
    /// idle timer starts from that moment).
    pub fn mark(&self) {
        self.last.store(mono_ms(), Ordering::Relaxed);
    }

    pub fn last_mono_ms(&self) -> Option<u64> {
        match self.last.load(Ordering::Relaxed) {
            0 => None,
            v => Some(v),
        }
    }

    pub fn idle_ms(&self) -> Option<u64> {
        self.last_mono_ms().map(|l| mono_ms().saturating_sub(l))
    }

    pub fn inflight(&self) -> u64 {
        self.inflight.load(Ordering::Relaxed)
    }

    pub fn requests_total(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    pub fn waiters(&self) -> usize {
        self.waiters.load(Ordering::Relaxed)
    }

    /// Reserve a waiter slot; `None` when `max` is already reached.
    pub fn try_wait(self: &Arc<Self>, max: usize) -> Option<WaiterGuard> {
        let mut cur = self.waiters.load(Ordering::Relaxed);
        loop {
            if cur >= max {
                return None;
            }
            match self.waiters.compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => return Some(WaiterGuard(Arc::clone(self))),
                Err(actual) => cur = actual,
            }
        }
    }

    pub fn begin_inflight(self: &Arc<Self>) -> InflightGuard {
        self.inflight.fetch_add(1, Ordering::AcqRel);
        self.mark();
        InflightGuard(Arc::clone(self))
    }
}

/// Decrements the in-flight counter and refreshes the idle timer when dropped, so a long
/// connection counts as activity until it closes.
#[derive(Debug)]
pub struct InflightGuard(Arc<Activity>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::AcqRel);
        self.0.mark();
    }
}

#[derive(Debug)]
pub struct WaiterGuard(Arc<Activity>);

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        self.0.waiters.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiter_slots_are_bounded_and_released() {
        let a = Arc::new(Activity::default());
        let g1 = a.try_wait(2).unwrap();
        let _g2 = a.try_wait(2).unwrap();
        assert!(a.try_wait(2).is_none());
        drop(g1);
        assert!(a.try_wait(2).is_some());
    }

    #[test]
    fn inflight_guard_counts_and_marks_activity() {
        let a = Arc::new(Activity::default());
        assert_eq!(a.last_mono_ms(), None);
        {
            let _g = a.begin_inflight();
            assert_eq!(a.inflight(), 1);
        }
        assert_eq!(a.inflight(), 0);
        assert!(a.last_mono_ms().is_some());
        assert_eq!(a.requests_total(), 0);
        a.touch();
        assert_eq!(a.requests_total(), 1);
    }
}
