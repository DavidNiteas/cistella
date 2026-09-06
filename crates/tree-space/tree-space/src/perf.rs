//! Opt-in name/elapsed profiling for storage operations.
//!
//! This module now delegates the timer mechanism to the shared `perfkit`
//! framework while preserving the original `tree-space` name/prefix and
//! environment switch. Timers emit `[tree-space-profile] <phase> = <elapsed>`
//! to stderr only when profiling is enabled (via `TREESPACE_PROFILE=1`, the
//! shared `PERFKIT_PERF=1`, or [`enable`]).

use std::time::Duration;

/// A scoped timer that logs `label` when dropped if profiling is enabled.
pub struct ProfileTimer {
    inner: perfkit::PhaseTimer,
}

impl ProfileTimer {
    /// Starts a phase timer; the label is reported on drop.
    pub fn start(label: &'static str) -> Self {
        init_once();
        Self {
            inner: perfkit::PhaseTimer::start("tree-space", label),
        }
    }

    /// Elapsed time since the timer started.
    pub fn elapsed(&self) -> Duration {
        self.inner.elapsed()
    }
}

/// Turns on name/elapsed phase reporting for the current process.
pub fn enable() {
    perfkit::enable();
}

/// Reads the `TREESPACE_PROFILE` and shared `PERFKIT_PERF` switches on first use.
pub fn init_once() {
    perfkit::init_from_env("TREESPACE_PROFILE");
    perfkit::init();
}
