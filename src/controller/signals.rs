//! Lossless, allocation-free signalling from request handlers to one group actor.
//!
//! Wake and sleep are idempotent, so they are flags, not queued messages: a thousand
//! requests during a start set the same bit once. The actor is poked through a
//! [`Notify`], which stores a permit when nobody is waiting, so a signal is never lost.
//! Compare the previous bounded `mpsc` with `try_send`, where a request burst could drop
//! a wake or, worse, a spec update.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[derive(Debug, Default)]
pub(super) struct Signals {
    wake: AtomicBool,
    /// Operator-initiated wake (API, TUI): bypasses the failed-retry pause.
    force_wake: AtomicBool,
    sleep: AtomicBool,
    notify: Notify,
}

/// What the actor found when it woke up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Pending {
    pub wake: bool,
    pub force_wake: bool,
    pub sleep: bool,
}

impl Signals {
    pub fn request_wake(&self) {
        self.wake.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    pub fn force_wake(&self) {
        self.force_wake.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    pub fn request_sleep(&self) {
        self.sleep.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    /// Wait until at least one signal is pending.
    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    /// Read and clear all pending signals.
    pub fn take(&self) -> Pending {
        Pending {
            wake: self.wake.swap(false, Ordering::AcqRel),
            force_wake: self.force_wake.swap(false, Ordering::AcqRel),
            sleep: self.sleep.swap(false, Ordering::AcqRel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signals_coalesce_and_are_never_lost() {
        let s = Signals::default();
        for _ in 0..1000 {
            s.request_wake();
        }
        s.request_sleep();
        // Permit was stored although nobody was waiting yet.
        tokio::time::timeout(std::time::Duration::from_millis(100), s.notified()).await.unwrap();
        assert_eq!(s.take(), Pending { wake: true, force_wake: false, sleep: true });
        assert_eq!(s.take(), Pending::default());
    }
}
