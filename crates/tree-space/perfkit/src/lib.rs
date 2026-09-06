//! `perfkit` — a generic, dependency-free performance-analysis framework for
//! embedded databases and data pipelines (portable beyond this workspace).
//!
//! Two orthogonal primitives:
//!
//! 1. **Structural work counters** ([`PerfCapture`] / [`record`]) measure work
//!    amplification (rows scanned, columns validated, IPC round-trips) rather
//!    than wall-clock time, so debug builds can assert regressions
//!    deterministically. Each host declares its own counter struct (business
//!    semantics are intentionally *not* shared); the mechanism is identical
//!    everywhere.
//! 2. **Phase timers** ([`PhaseTimer`]) report elapsed time to stderr under a
//!    host-chosen namespace prefix, gated by the `PERFKIT_PERF` environment
//!    variable or an explicit [`enable`].
//!
//! # Counter adoption
//!
//! A host declares a work-amplification struct (any `Default + Clone + 'static`
//! type) and drives it through [`PerfCapture::start`] + [`record`]:
//!
//! ```ignore
//! #[derive(Clone, Default)]
//! struct MyCounters { pub rows_scanned: u64, pub writes: u64 }
//!
//! let capture = perfkit::PerfCapture::<MyCounters>::start();
//! perfkit::record::<MyCounters>(|c| c.rows_scanned += 1);
//! assert_eq!(capture.snapshot().rows_scanned, 1);
//! ```

use std::any::Any;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Types that can back a structural work-counter capture.
///
/// Blanket-implemented for every `Default + Clone + 'static` type, so a crate
/// only has to derive those three for its counter struct.
pub trait PerfCounter: Default + Clone + 'static {}
impl<T: Default + Clone + 'static> PerfCounter for T {}

thread_local! {
    /// Stack of active captures; `record` targets the innermost matching type.
    static ACTIVE: RefCell<Vec<Rc<dyn Any>>> = const { RefCell::new(Vec::new()) };
}

/// A scoped capture of structural work counters on the current thread.
///
/// Nested captures (including of *different* counter types) are supported:
/// dropping a capture restores the previous stack for the current thread, and
/// [`record`] targets the innermost capture whose type matches.
pub struct PerfCapture<C: PerfCounter> {
    index: usize,
    marker: PhantomData<C>,
}

impl<C: PerfCounter> PerfCapture<C> {
    /// Starts capturing counters of type `C`.
    pub fn start() -> Self {
        let cell = Rc::new(RefCell::new(C::default()));
        let index = ACTIVE.with(|active| {
            let mut stack = active.borrow_mut();
            stack.push(cell as Rc<dyn Any>);
            stack.len() - 1
        });
        Self {
            index,
            marker: PhantomData,
        }
    }

    /// Returns a clone of the captured counters.
    pub fn snapshot(&self) -> C {
        ACTIVE.with(|active| {
            let stack = active.borrow();
            let cell = stack[self.index]
                .downcast_ref::<RefCell<C>>()
                .expect("capture type");
            cell.borrow().clone()
        })
    }
}

impl<C: PerfCounter> Drop for PerfCapture<C> {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.borrow_mut().truncate(self.index));
    }
}

/// Applies `update` to the innermost active counter of type `C`, if any.
///
/// No-op without an active capture, so call sites can be unconditional.
pub fn record<C: PerfCounter>(update: impl FnOnce(&mut C)) {
    ACTIVE.with(|active| {
        let stack = active.borrow();
        for slot in stack.iter().rev() {
            if let Some(cell) = slot.downcast_ref::<RefCell<C>>() {
                update(&mut cell.borrow_mut());
                return;
            }
        }
    });
}

/// Converts a row/byte count to the counter representation without panicking.
pub fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Adds a counter value without panicking on pathological inputs.
pub fn saturating_add(target: &mut u64, amount: u64) {
    *target = target.saturating_add(amount);
}

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turns on phase reporting for the current process.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// Enables phase reporting if the named environment variable is truthy.
pub fn init_from_env(name: &str) {
    if !ENABLED.load(Ordering::Relaxed)
        && std::env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0")
    {
        enable();
    }
}

/// Enables phase reporting from the shared `PERFKIT_PERF` switch.
pub fn init() {
    init_from_env("PERFKIT_PERF");
}

/// A scoped phase timer that logs `[<prefix>-profile] <label> = <elapsed>` to
/// stderr on drop when phase reporting is enabled.
pub struct PhaseTimer {
    prefix: &'static str,
    label: &'static str,
    started: Instant,
}

impl PhaseTimer {
    /// Starts a phase timer under a namespace prefix.
    pub fn start(prefix: &'static str, label: &'static str) -> Self {
        init();
        Self {
            prefix,
            label,
            started: Instant::now(),
        }
    }

    /// Elapsed time since the timer started.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

impl Drop for PhaseTimer {
    fn drop(&mut self) {
        if ENABLED.load(Ordering::Relaxed) {
            eprintln!(
                "[{}-profile] {} = {:?}",
                self.prefix,
                self.label,
                self.started.elapsed()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct A {
        hits: u64,
    }
    #[derive(Clone, Default)]
    struct B {
        hits: u64,
    }

    #[test]
    fn record_is_noop_without_capture() {
        record::<A>(|c| c.hits += 1);
    }

    #[test]
    fn nested_captures_restore_the_previous_sink() {
        let outer = PerfCapture::<A>::start();
        record::<A>(|c| c.hits += 1);

        {
            let inner = PerfCapture::<A>::start();
            record::<A>(|c| c.hits += 7);
            assert_eq!(inner.snapshot().hits, 7);
            assert_eq!(outer.snapshot().hits, 1);
        }

        record::<A>(|c| c.hits += 2);
        assert_eq!(outer.snapshot().hits, 3);
    }

    #[test]
    fn different_types_target_the_innermost_matching_capture() {
        let a = PerfCapture::<A>::start();
        let b = PerfCapture::<B>::start();
        record::<A>(|c| c.hits += 1);
        record::<B>(|c| c.hits += 5);
        assert_eq!(a.snapshot().hits, 1);
        assert_eq!(b.snapshot().hits, 5);
    }

    #[test]
    fn count_and_saturating_add_are_bounded() {
        assert_eq!(count(usize::MAX), u64::MAX);
        let mut v = u64::MAX - 3;
        saturating_add(&mut v, 10);
        assert_eq!(v, u64::MAX);
    }
}
