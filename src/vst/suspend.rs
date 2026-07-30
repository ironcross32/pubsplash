//! Announcing to the engine that a plugin is about to become unavailable.
//!
//! Both plugin formats have the same problem: a UI operation needs exclusive
//! access to a plugin that the engine is handing a block to every 10 ms. The
//! engine must never block, and the plugin must never be yanked out of the
//! signal path with a step discontinuity. So the sequence is always:
//!
//! 1. the UI raises the flag and *waits* [`SETTLE`];
//! 2. the engine, still processing normally, sees the flag and reports
//!    [`Processed::Retiring`](crate::vst::Processed::Retiring), which makes
//!    `FxChain` fade the plugin out of circuit over real audio;
//! 3. only then does the UI take the plugin's lock, by which time the fade has
//!    finished and the engine's `try_lock` failures cost nothing.
//!
//! Dropping the guard lets the engine fade the plugin back in.
//!
//! This started life inside `host3.rs` for VST3, whose `IEditController` is
//! main-thread-only. VST2 needs exactly the same thing — its dispatcher
//! (chunks, the editor, mains-changed) is UI-thread-only while
//! `process_replacing` runs on the engine — so the machinery lives here and
//! both hosts use it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// How long a suspending UI operation waits before taking the plugin's lock,
/// giving the engine time to fade the plugin out of circuit first. Six 10 ms
/// mix blocks, comfortably more than the 50 ms fade.
pub const SETTLE: Duration = Duration::from_millis(60);

/// Non-zero while one or more UI operations are about to hold, or are holding,
/// a plugin for longer than a mix block.
#[derive(Default)]
pub struct SuspendFlag(AtomicUsize);

impl SuspendFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Engine thread. Whether the plugin should be faded out of circuit.
    pub fn raised(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }

    /// Announces a long lock hold to the engine and waits for it to fade the
    /// plugin out. **UI thread only — it sleeps.** Nested raises do not sleep
    /// again; the first one already bought the whole settle.
    pub fn raise(&self) -> Suspended<'_> {
        if self.0.fetch_add(1, Ordering::AcqRel) == 0 {
            std::thread::sleep(SETTLE);
        }
        Suspended(self)
    }
}

/// Raised for the lifetime of a UI operation that holds a plugin for longer
/// than one mix block. See [`SuspendFlag::raise`].
pub struct Suspended<'a>(&'a SuspendFlag);

impl Drop for Suspended<'_> {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_is_raised_for_the_guards_lifetime() {
        let flag = SuspendFlag::new();
        assert!(!flag.raised());
        {
            let _guard = flag.raise();
            assert!(flag.raised());
        }
        assert!(!flag.raised());
    }

    #[test]
    fn nested_holds_keep_the_flag_up_until_the_last_one_goes() {
        let flag = SuspendFlag::new();
        let outer = flag.raise();
        let inner = flag.raise();
        drop(inner);
        assert!(flag.raised(), "the outer hold still owns the plugin");
        drop(outer);
        assert!(!flag.raised());
    }

    #[test]
    fn only_the_first_raise_pays_the_settle() {
        let flag = SuspendFlag::new();
        let outer = flag.raise();
        // Nesting must be cheap: a `snapshot` inside an `editor_close` would
        // otherwise stack another 60 ms onto the UI thread for nothing.
        let start = std::time::Instant::now();
        let inner = flag.raise();
        assert!(start.elapsed() < SETTLE / 2, "the nested raise slept");
        drop(inner);
        drop(outer);
    }
}
