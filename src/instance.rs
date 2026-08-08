//! One Pubsplash at a time, per Windows user.
//!
//! Two copies of the app fight over everything that is effectively exclusive:
//! the WASAPI capture threads and the desktop loopback client, the rotating log
//! file, the settings file (last writer wins, so one copy silently discards the
//! other's scene edits), the DPAPI-backed credentials, and worst of all the
//! Icecast source connection, where the second copy's mount attempt either takes
//! the stream from the first or is refused with `403 Mountpoint in use`.
//!
//! The guard is a kernel named mutex rather than a lock file beside the
//! executable, for two reasons. It is keyed on a name and nothing else, so an
//! installed copy under `%LOCALAPPDATA%\Programs` and a portable copy in a
//! folder of the user's own collide exactly as they should — a path-derived lock
//! would miss the case entirely. And the object dies with the last handle to it,
//! which the kernel closes on *any* process exit including a crash or a kill
//! from Task Manager, so there is no stale lock to clear.
//!
//! This module is reachable only from `main.rs`'s module list, so none of the
//! other binaries — `pubsplash-scan` (many concurrent invocations per plugin
//! scan), `pubsplash-soundpack`, `pubsplash-update` (which must run while
//! Pubsplash is exiting), `soundpack`, `gen-help` — is affected by it.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONINFORMATION, MB_OK, MB_SYSTEMMODAL, MessageBoxW,
};
use windows_core::PCWSTR;

/// `Local\` is the session namespace, so two people signed in to one machine
/// (fast user switching, or RDP) each get their own — which matches the fact
/// that config, logs and credentials are all per-user. The name carries no
/// version and no path on purpose: that is what makes every install of
/// Pubsplash contend for the same object.
const MUTEX_NAME: &str = r"Local\Pubsplash-SingleInstance";

/// How long to keep trying before deciding another instance really is running.
///
/// This exists for exactly one flow. After an update, `pubsplash-update.exe`
/// relaunches the new `pubsplash.exe` as soon as it believes the old one is
/// gone — and its `wait_for_exit` reports "already gone" when `OpenProcess`
/// fails, gives up after a 90 s timeout, and on the installed path hands off to
/// NSIS, whose "run now" is not under our control at all. Without this window a
/// well-behaved update would sometimes end in an "already running" box and no
/// app. Do not remove it as dead weight; nothing else needs it, and the cost is
/// a short pause in front of a message box nobody wanted anyway.
const GRACE: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(100);

/// A held claim on the single-instance mutex, released when it is dropped.
pub struct InstanceGuard {
    handle: HANDLE,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

pub enum Acquired {
    Held(InstanceGuard),
    AlreadyRunning,
}

/// Claims the right to be the one running Pubsplash for this user.
pub fn acquire() -> Acquired {
    acquire_named(MUTEX_NAME, GRACE)
}

/// The body of [`acquire`], with the name and grace window as arguments so the
/// tests can contend for an object of their own.
fn acquire_named(name: &str, grace: Duration) -> Acquired {
    let deadline = Instant::now() + grace;
    let wide_name = wide(name);
    loop {
        // A valid handle comes back whether or not the object already existed,
        // so the answer is in the last error and the handle has to be closed
        // again before the next attempt.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide_name.as_ptr())) };
        let handle = match handle {
            Ok(handle) => handle,
            // Nothing a user can act on, and refusing to start over it would be
            // worse than the duplicate it was meant to prevent.
            Err(_) => return Acquired::AlreadyRunning,
        };
        if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            return Acquired::Held(InstanceGuard { handle });
        }
        unsafe {
            let _ = CloseHandle(handle);
        }
        if Instant::now() >= deadline {
            return Acquired::AlreadyRunning;
        }
        std::thread::sleep(POLL);
    }
}

/// Tells the user why this copy is not starting, and stops.
///
/// A raw `MessageBoxW` because there is no wx app yet — this runs before
/// anything in `main` — and system-modal because there is no other surface for
/// it to sit on. It also goes to stderr, which costs nothing and is the only way
/// to read it from a console; release builds are `windows_subsystem = "windows"`
/// and have no console for it to reach, so no user sees it twice.
pub fn reject_and_exit() -> ! {
    const MESSAGE: &str = "Pubsplash is already running.";
    eprintln!("pubsplash: {MESSAGE}");
    let text = wide(MESSAGE);
    let caption = wide("Pubsplash");
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONINFORMATION | MB_SYSTEMMODAL,
        );
    }
    std::process::exit(0);
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Tests run in parallel threads of one process, and the mutex namespace is
    /// shared by both, so every test needs a name no other test can collide
    /// with.
    fn unique_name() -> String {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        format!(
            r"Local\Pubsplash-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn is_held(acquired: &Acquired) -> bool {
        matches!(acquired, Acquired::Held(_))
    }

    #[test]
    fn second_acquire_is_rejected() {
        let name = unique_name();
        let first = acquire_named(&name, Duration::ZERO);
        assert!(is_held(&first), "the first instance should be let in");
        let second = acquire_named(&name, Duration::ZERO);
        assert!(
            !is_held(&second),
            "the second instance should be turned away"
        );
    }

    /// The property the post-update relaunch depends on: the claim is gone the
    /// moment the holder is.
    #[test]
    fn releasing_lets_the_next_one_in() {
        let name = unique_name();
        let first = acquire_named(&name, Duration::ZERO);
        assert!(is_held(&first));
        drop(first);
        let second = acquire_named(&name, Duration::ZERO);
        assert!(is_held(&second), "the claim should have been released");
    }

    #[test]
    fn grace_window_is_waited_out() {
        let name = unique_name();
        let holder = acquire_named(&name, Duration::ZERO);
        assert!(is_held(&holder));
        // The waiter runs on the other thread rather than the holder, because
        // `HANDLE` is not `Send` and the guard is deliberately not made so for
        // the sake of a test — the app only ever holds it on the main thread.
        let waiting = std::thread::spawn({
            let name = name.clone();
            move || is_held(&acquire_named(&name, Duration::from_secs(2)))
        });
        std::thread::sleep(Duration::from_millis(200));
        drop(holder);
        assert!(
            waiting.join().expect("the waiting thread"),
            "the grace window should have covered the wait"
        );
    }

    /// Two different names are two different claims — this is what keeps the
    /// guard from reaching the other binaries or another user's session.
    #[test]
    fn different_names_do_not_contend() {
        let first = acquire_named(&unique_name(), Duration::ZERO);
        let second = acquire_named(&unique_name(), Duration::ZERO);
        assert!(is_held(&first) && is_held(&second));
    }
}
