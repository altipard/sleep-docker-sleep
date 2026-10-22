use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Milliseconds since the process started (monotonic). Never 0 after the first call.
pub fn mono_ms() -> u64 {
    let epoch = *EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX).max(1)
}

/// Milliseconds since the Unix epoch (wall clock).
pub fn epoch_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)).unwrap_or(0)
}

/// Convert a monotonic timestamp from [`mono_ms`] to wall-clock epoch milliseconds.
pub fn mono_to_epoch_ms(mono: u64) -> u64 {
    let age = mono_ms().saturating_sub(mono);
    epoch_ms().saturating_sub(age)
}
