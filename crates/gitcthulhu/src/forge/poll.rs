//! The polling cadence of the forge controller (15 §3; 21 §2.10): one tick per minute, every 5th tick while the window
//! is inactive, back-off after failures, and a stop after errors that only the user can fix. Pure, so the cadence is
//! tested without a clock.

use std::time::Duration;

use crate::backend::forge::{Failure, FailureKind};

/// The clock of the cadence; tests shorten it through the [`TimingsGlobal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    /// The time between two ticks.
    pub tick: Duration,
    /// While the window is inactive only every n-th tick refreshes.
    pub inactive_every: u64,
    /// The time between two looks for the services and the repository while detection cannot start yet.
    pub detect_retry: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Timings {
            tick: Duration::from_secs(60),
            inactive_every: 5,
            detect_retry: Duration::from_millis(500),
        }
    }
}

/// An override of the timings (tests).
#[derive(Clone, Copy)]
pub struct TimingsGlobal(pub Timings);

impl gpui_kit::Global for TimingsGlobal {}

/// The most ticks a failing refresh waits before asking again (8 minutes at the default tick).
pub const MAX_BACKOFF_TICKS: u64 = 8;
/// Ticks to wait after a rate limit whose reset time is unknown.
pub const RATE_LIMIT_TICKS: u64 = 5;

/// Why the poller does not ask the forge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Halt {
    /// An error only the user can fix: no account, a revoked token, a missing permission.
    UserAction(String),
}

/// The state of the cadence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Poller {
    tick: u64,
    wake_at: u64,
    failures: u32,
    halt: Option<Halt>,
    offline: bool,
}

impl Poller {
    /// A tick of the timer: whether the forge is asked now. `window_active` is whether the app's window has the
    /// focus.
    pub fn due(&mut self, window_active: bool, timings: &Timings) -> bool {
        self.tick += 1;
        if self.halt.is_some() {
            return false;
        }
        if !window_active && !self.tick.is_multiple_of(timings.inactive_every.max(1)) {
            return false;
        }
        self.tick >= self.wake_at
    }

    /// The refresh worked.
    pub fn succeeded(&mut self) {
        self.failures = 0;
        self.wake_at = 0;
        self.offline = false;
    }

    /// The refresh failed: stops after errors the user must fix, waits longer after each failure that may pass.
    pub fn failed(&mut self, failure: &Failure, now_unix: i64, timings: &Timings) {
        self.failures = self.failures.saturating_add(1);
        match failure.kind {
            _ if failure.is_terminal() => {
                self.halt = Some(Halt::UserAction(failure.message.clone()));
            }
            FailureKind::RateLimited { reset_at } => {
                let ticks = reset_at
                    .map(|at| {
                        let secs = (at - now_unix).max(0) as u64;
                        secs.div_ceil(timings.tick.as_secs().max(1)).max(1)
                    })
                    .unwrap_or(RATE_LIMIT_TICKS);
                self.wake_at = self.tick + ticks;
            }
            _ => {
                self.offline = failure.may_pass();
                let shift = self.failures.min(4);
                self.wake_at = self.tick + (1u64 << shift).min(MAX_BACKOFF_TICKS);
            }
        }
    }

    /// The user did something about the account setup (or asked for a refresh): ask again at the next tick.
    pub fn user_acted(&mut self) {
        self.halt = None;
        self.failures = 0;
        self.wake_at = 0;
        self.offline = false;
    }

    /// Why polling stopped, if it did.
    pub fn halt(&self) -> Option<&Halt> {
        self.halt.as_ref()
    }

    /// The forge could not be reached at the last refresh.
    pub fn offline(&self) -> bool {
        self.offline
    }

    /// Consecutive failures.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Ticks seen so far.
    pub fn ticks(&self) -> u64 {
        self.tick
    }
}
