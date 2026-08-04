//! Logger setup with runtime level switching.
//!
//! Precedence: `PUBSPLASH_LOG_<LEVEL>` environment variable (e.g.
//! `PUBSPLASH_LOG_TRACE=1` or simply defining `PUBSPLASH_LOG_DEBUG`)
//! supersedes the level stored in the config file. Logs go to
//! `%LOCALAPPDATA%\pubsplash\logs\` and, in debug builds, to stderr.
//!
//! The handle lives in a process-global rather than in `main`, in the shape of
//! `tts::usage` and `tts::catalog`: the Preferences window changes the level and
//! captures the logs, and there is no route from the UI down to a local in
//! `main`. Every free function below is a no-op when the logger failed to start.

use flexi_logger::{FileSpec, LogSpecification, Logger, LoggerHandle};
use log::LevelFilter;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

/// The stored level's index in [`LEVELS`], which is the level picker's
/// selection. Anything unrecognized reads as `info`, the same fallback
/// [`init`] applies, so a hand-edited config file cannot leave the picker
/// showing nothing.
pub fn level_index(level: &str) -> usize {
    let level = level.to_ascii_lowercase();
    LEVELS
        .iter()
        .position(|candidate| *candidate == level)
        .unwrap_or(3)
}

/// A level as the picker shows it: `"off"` becomes `"Off"`. The stored and
/// spoken forms differ only in case, so there is no second table to keep in
/// step with [`LEVELS`].
pub fn level_label(level: &str) -> String {
    let mut chars = level.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn parse_level(s: &str) -> Option<LevelFilter> {
    match s.to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

/// Returns the level forced by a `PUBSPLASH_LOG_<LEVEL>` environment
/// variable, if one is set. The highest level wins if several are set.
pub fn env_override() -> Option<LevelFilter> {
    LEVELS
        .iter()
        .rev()
        .find(|level| std::env::var_os(format!("PUBSPLASH_LOG_{}", level.to_uppercase())).is_some())
        .and_then(|level| parse_level(level))
}

/// The log spec for `level`.
///
/// `vst3_host` is pinned a notch more verbose than everything else, never below
/// debug. Its module loader logs the `InitDll`/`ExitDll`/`FreeLibrary` steps at
/// debug, and those are the only breadcrumbs there are for a plugin that faults
/// the whole process while being torn down — at info the log just stops. The
/// cost is a burst of lines per plugin load, once, at startup.
fn spec(level: LevelFilter) -> LogSpecification {
    let mut builder = LogSpecification::builder();
    builder.default(level);
    if level != LevelFilter::Off {
        builder.module("vst3_host", level.max(LevelFilter::Debug));
    }
    builder.build()
}

struct LogHandle {
    handle: LoggerHandle,
    /// True when an env var pinned the level; UI changes are ignored then.
    env_pinned: bool,
}

/// Set once by [`init`]. `Mutex` rather than a bare handle because
/// `push_temp_spec`/`pop_temp_spec` want `&mut`, and because the callers are
/// not all the same thread.
static HANDLE: OnceLock<Mutex<LogHandle>> = OnceLock::new();

/// Runs `f` with the logger handle, if there is one. Poisoning is recovered:
/// a panic elsewhere must not take logging down with it.
fn with_handle<T>(f: impl FnOnce(&mut LogHandle) -> T) -> Option<T> {
    let mutex = HANDLE.get()?;
    let mut guard = mutex.lock().unwrap_or_else(|e| e.into_inner());
    Some(f(&mut guard))
}

/// Applies a new level from the in-app setting. No-op when the level is
/// pinned by the environment variable, which strictly supersedes it.
pub fn set_level(level: &str) {
    with_handle(|h| {
        if h.env_pinned {
            log::info!("Ignoring in-app log level change; PUBSPLASH_LOG_* is set");
            return;
        }
        if let Some(filter) = parse_level(level) {
            h.handle.set_new_spec(spec(filter));
            log::info!("Log level set to {level}");
        } else {
            log::warn!("Unknown log level {level:?}");
        }
    });
}

/// True when `PUBSPLASH_LOG_*` fixed the level for this run. The Preferences
/// tab disables its level picker on this, since [`set_level`] would be ignored.
pub fn env_pinned() -> bool {
    with_handle(|h| h.env_pinned).unwrap_or(false)
}

/// Flushes and shuts the writers down at the end of `main`.
///
/// Anything that needs a flush without the shutdown — the panic hook, the crash
/// filter — calls `log::logger().flush()`, which reaches the same writers
/// without needing this module at all.
///
/// The handle is static and so is never dropped, and dropping it is what used
/// to flush the tail of the session: `WriteMode::Async` hands each record to a
/// background thread, so without this the last seconds of every run — which
/// includes whatever went wrong on the way out — never reach the file.
pub fn shutdown() {
    with_handle(|h| {
        h.handle.flush();
        h.handle.shutdown();
    });
}

/// `%LOCALAPPDATA%\pubsplash\logs`
pub fn logs_dir() -> PathBuf {
    crate::config::config_dir().join("logs")
}

/// `%LOCALAPPDATA%\pubsplash\crashes` — the minidumps `crash.rs` writes.
pub fn crash_dir() -> PathBuf {
    crate::config::config_dir().join("crashes")
}

/// Routes panics into the log file.
///
/// This matters more than it looks. wxdragon wraps every event-handler callback
/// in `catch_unwind` and discards the error, so a panic inside a UI handler does
/// not abort, does not print, and does not reach the log — the rest of that
/// handler simply never runs and the control appears to do nothing. For a
/// screen-reader app that is silent in every sense of the word. Logging panics
/// is the only way such a bug leaves a trace at all.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_string());
        // The backtrace is forced rather than left to RUST_BACKTRACE: a panic
        // here is rare and is exactly the moment the trace is needed.
        log::error!(
            "PANIC at {location}: {}\n{}",
            info.payload_as_str().unwrap_or("<non-string payload>"),
            std::backtrace::Backtrace::force_capture()
        );
        // Log writes are buffered and written on a background thread, and this
        // process may not survive long enough for that to happen on its own.
        log::logger().flush();
        // Debug builds still print to the console as usual.
        previous(info);
    }));
}

/// Initializes logging. `configured_level` comes from the config file.
///
/// The handle is stashed in [`HANDLE`]; calling this twice leaves the first
/// logger in place, which is the only sane answer since `log` itself can only
/// be initialized once.
pub fn init(configured_level: &str) {
    init_in(&logs_dir(), configured_level);
}

/// The body of [`init`], with the log directory as an argument so the capture
/// test can run the real logger against a scratch folder instead of the user's
/// own logs.
fn init_in(directory: &Path, configured_level: &str) {
    let env = env_override();
    let level = env
        .or_else(|| parse_level(configured_level))
        .unwrap_or(LevelFilter::Info);

    let logger = Logger::with(spec(level))
        .log_to_file(FileSpec::default().directory(directory).basename("pubsplash"))
        // Buffer and write on a background thread: the audio engine logs from
        // inside the 10 ms mix loop, and the default mode makes each of those a
        // mutex-guarded synchronous file write on that thread.
        .write_mode(flexi_logger::WriteMode::Async)
        .rotate(
            flexi_logger::Criterion::Size(5 * 1024 * 1024),
            flexi_logger::Naming::Numbers,
            flexi_logger::Cleanup::KeepLogFiles(5),
        )
        .format(flexi_logger::detailed_format);

    #[cfg(debug_assertions)]
    let logger = logger.duplicate_to_stderr(flexi_logger::Duplicate::All);

    match logger.start() {
        Ok(handle) => {
            let _ = HANDLE.set(Mutex::new(LogHandle {
                handle,
                env_pinned: env.is_some(),
            }));
        }
        Err(e) => eprintln!("Failed to initialize logger: {e}"),
    }
}

/// Packages the log files and crash dumps into the ZIP at `dest`, returning how
/// many files went in.
///
/// Logging is genuinely halted for the duration rather than merely quiesced:
/// the spec goes to `off`, so nothing new is queued — including the audio
/// engine, which logs from inside the 10 ms mix loop — then the async writer's
/// backlog is flushed to disk and an extra rotation closes
/// `pubsplash_rCURRENT.log` and renames it to a numbered file. That rotation is
/// the whole point: without it the file being written is the one the user most
/// needs to send, and it would go into the archive with its tail missing.
///
/// The `off` spec is popped on every path, including the failures. A capture
/// that goes wrong must not leave the app silent for the rest of the session,
/// which is exactly the session someone is trying to diagnose.
pub fn capture(dest: &Path) -> Result<usize, String> {
    capture_from(dest, &[("logs", &logs_dir()), ("crashes", &crash_dir())])
}

/// The body of [`capture`], with the source directories as an argument so the
/// test can point it at a scratch folder.
fn capture_from(dest: &Path, sources: &[(&str, &Path)]) -> Result<usize, String> {
    let Some(mutex) = HANDLE.get() else {
        return Err("Logging is not running, so there is nothing to collect.".to_string());
    };
    let mut guard = mutex.lock().unwrap_or_else(|e| e.into_inner());

    guard.handle.push_temp_spec(LogSpecification::off());
    let result = (|| {
        guard.handle.flush();
        guard
            .handle
            .trigger_rotation()
            .map_err(|e| format!("Could not rotate the log files: {e}"))?;
        zip_dirs(dest, sources)
    })();
    guard.handle.pop_temp_spec();
    drop(guard);

    match &result {
        Ok(count) => log::info!("Collected {count} log files into {}", dest.display()),
        Err(e) => log::warn!("Could not collect the logs: {e}"),
    }
    result
}

/// Builds a deflate ZIP of every file directly inside each `(prefix, dir)`,
/// stored under that prefix. A directory that does not exist contributes
/// nothing rather than failing — a run that has never crashed has no `crashes`
/// folder, and that is the normal case.
///
/// Split out from [`capture`] so it can be tested without a live logger. Only
/// `Deflated` is available: see the `zip` dependency's feature list.
fn zip_dirs(dest: &Path, sources: &[(&str, &Path)]) -> Result<usize, String> {
    use std::io::Write;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    let file = std::fs::File::create(dest)
        .map_err(|e| format!("Could not create {}: {e}", dest.display()))?;
    let mut writer = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let options: zip::write::FileOptions<'_, ()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut count = 0;
    for (prefix, dir) in sources {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        // Sorted so an archive is reproducible and reads in a sensible order;
        // `read_dir` promises no ordering at all.
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        paths.sort();
        for path in paths {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // One unreadable file must not cost the user the whole capture:
            // a rotated log can still be held open by a cleanup thread.
            let body = match std::fs::read(&path) {
                Ok(body) => body,
                Err(e) => {
                    eprintln!("Skipping {}: {e}", path.display());
                    continue;
                }
            };
            writer
                .start_file(format!("{prefix}/{name}"), options)
                .map_err(|e| format!("Could not add {name} to the archive: {e}"))?;
            writer
                .write_all(&body)
                .map_err(|e| format!("Could not write {name} into the archive: {e}"))?;
            count += 1;
        }
    }
    writer
        .finish()
        .map_err(|e| format!("Could not finish the archive: {e}"))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree that cleans itself up, as in `update::mod`'s tests.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("pubsplash-logzip-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("creating the scratch directory");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entries(archive: &Path) -> Vec<String> {
        let file = std::fs::File::open(archive).expect("opening the archive");
        let mut zip = zip::ZipArchive::new(file).expect("reading the archive");
        (0..zip.len())
            .map(|i| zip.by_index(i).expect("an entry").name().to_string())
            .collect()
    }

    #[test]
    fn each_directory_keeps_its_own_prefix() {
        let scratch = Scratch::new("prefix");
        let logs = scratch.0.join("logs");
        let crashes = scratch.0.join("crashes");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::create_dir_all(&crashes).unwrap();
        // The same file name in both, which is what the prefixes are for.
        std::fs::write(logs.join("pubsplash_rCURRENT.log"), b"a log line").unwrap();
        std::fs::write(crashes.join("pubsplash_rCURRENT.log"), b"a dump").unwrap();

        let archive = scratch.0.join("out.zip");
        let count = zip_dirs(&archive, &[("logs", &logs), ("crashes", &crashes)])
            .expect("building the archive");

        assert_eq!(count, 2);
        assert_eq!(
            entries(&archive),
            vec![
                "logs/pubsplash_rCURRENT.log".to_string(),
                "crashes/pubsplash_rCURRENT.log".to_string()
            ]
        );
    }

    /// A user who has never had a crash has no `crashes` folder, and that is
    /// the normal case rather than a failed capture.
    #[test]
    fn a_missing_directory_is_skipped_not_an_error() {
        let scratch = Scratch::new("missing");
        let logs = scratch.0.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("pubsplash_r00001.log").as_path(), b"old").unwrap();

        let archive = scratch.0.join("out.zip");
        let count = zip_dirs(
            &archive,
            &[("logs", &logs), ("crashes", &scratch.0.join("crashes"))],
        )
        .expect("building the archive");

        assert_eq!(count, 1);
        assert_eq!(entries(&archive), vec!["logs/pubsplash_r00001.log"]);
    }

    /// Sub-directories are not descended into: the logs folder is flat, and a
    /// stray folder in it is not the user's log.
    #[test]
    fn only_files_go_in() {
        let scratch = Scratch::new("files");
        let logs = scratch.0.join("logs");
        std::fs::create_dir_all(logs.join("nested")).unwrap();
        std::fs::write(logs.join("pubsplash_rCURRENT.log"), b"kept").unwrap();
        std::fs::write(logs.join("nested").join("deep.log"), b"skipped").unwrap();

        let archive = scratch.0.join("out.zip");
        let count = zip_dirs(&archive, &[("logs", &logs)]).expect("building the archive");

        assert_eq!(count, 1);
        assert_eq!(entries(&archive), vec!["logs/pubsplash_rCURRENT.log"]);
    }

    /// The whole mechanism against the real logger: a line written now must be
    /// in the archive, which is only true because [`capture`] rotates the live
    /// file before reading it, and logging must still work afterwards.
    ///
    /// Ignored because it starts the process-wide logger — `log` allows that
    /// once — and so cannot share a process with any other test that logs.
    /// Run with `cargo test captures_the_current_session -- --include-ignored`.
    #[test]
    #[ignore]
    fn captures_the_current_session_and_keeps_logging_after() {
        let scratch = Scratch::new("live");
        let logs = scratch.0.join("logs");
        init_in(&logs, "info");

        log::info!("a line from before the capture");
        let archive = scratch.0.join("out.zip");
        let count = capture_from(&archive, &[("logs", &logs)]).expect("capturing");
        assert!(count >= 1, "the current log file should have gone in");

        let file = std::fs::File::open(&archive).expect("opening the archive");
        let mut zip = zip::ZipArchive::new(file).expect("reading the archive");
        let mut text = String::new();
        for i in 0..zip.len() {
            use std::io::Read;
            zip.by_index(i)
                .expect("an entry")
                .read_to_string(&mut text)
                .expect("an entry is text");
        }
        assert!(
            text.contains("a line from before the capture"),
            "the live file was not rotated into the archive: {text}"
        );

        // The `off` spec must have been popped, or the app is silent for the
        // rest of the session it is trying to report on.
        log::info!("a line from after the capture");
        shutdown();
        let after = std::fs::read_to_string(logs.join("pubsplash_rCURRENT.log")).unwrap_or_default();
        assert!(
            after.contains("a line from after the capture"),
            "logging did not resume: {after}"
        );
    }

    #[test]
    fn every_level_round_trips_through_the_picker() {
        for (index, level) in LEVELS.iter().enumerate() {
            assert_eq!(level_index(level), index);
            assert!(parse_level(level).is_some());
        }
        assert_eq!(level_index("INFO"), 3);
        // A hand-edited config file with nonsense in it shows the default.
        assert_eq!(level_index("chatty"), 3);
        assert_eq!(LEVELS[level_index("chatty")], "info");
        assert_eq!(level_label("warn"), "Warn");
    }
}
