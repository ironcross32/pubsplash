//! Logger setup with runtime level switching.
//!
//! Precedence: `PUBSPLASH_LOG_<LEVEL>` environment variable (e.g.
//! `PUBSPLASH_LOG_TRACE=1` or simply defining `PUBSPLASH_LOG_DEBUG`)
//! supersedes the level stored in the config file. Logs go to
//! `%LOCALAPPDATA%\pubsplash\logs\` and, in debug builds, to stderr.

use flexi_logger::{FileSpec, LogSpecification, Logger, LoggerHandle};
use log::LevelFilter;

pub const LEVELS: [&str; 6] = ["off", "error", "warn", "info", "debug", "trace"];

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

pub struct LogHandle {
    handle: LoggerHandle,
    /// True when an env var pinned the level; UI changes are ignored then.
    env_pinned: bool,
}

impl LogHandle {
    /// Applies a new level from the in-app setting. No-op when the level is
    /// pinned by the environment variable, which strictly supersedes it.
    pub fn set_level(&mut self, level: &str) {
        if self.env_pinned {
            log::info!("Ignoring in-app log level change; PUBSPLASH_LOG_* is set");
            return;
        }
        if let Some(filter) = parse_level(level) {
            self.handle
                .set_new_spec(LogSpecification::builder().default(filter).build());
        } else {
            log::warn!("Unknown log level {level:?}");
        }
    }

    /// The logging UI (upcoming) disables the level picker when pinned.
    #[allow(dead_code)]
    pub fn env_pinned(&self) -> bool {
        self.env_pinned
    }
}

/// Initializes logging. `configured_level` comes from the config file.
pub fn init(configured_level: &str) -> Option<LogHandle> {
    let env = env_override();
    let level = env
        .or_else(|| parse_level(configured_level))
        .unwrap_or(LevelFilter::Info);

    let logger = Logger::with(LogSpecification::builder().default(level).build())
        .log_to_file(
            FileSpec::default()
                .directory(crate::config::config_dir().join("logs"))
                .basename("pubsplash"),
        )
        .rotate(
            flexi_logger::Criterion::Size(5 * 1024 * 1024),
            flexi_logger::Naming::Numbers,
            flexi_logger::Cleanup::KeepLogFiles(5),
        )
        .format(flexi_logger::detailed_format);

    #[cfg(debug_assertions)]
    let logger = logger.duplicate_to_stderr(flexi_logger::Duplicate::All);

    match logger.start() {
        Ok(handle) => Some(LogHandle {
            handle,
            env_pinned: env.is_some(),
        }),
        Err(e) => {
            eprintln!("Failed to initialize logger: {e}");
            None
        }
    }
}
