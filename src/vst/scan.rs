//! The scan coordinator: a worker thread that enumerates candidates and runs
//! the `pubsplash-scan` helper process for each one, reporting progress to
//! the UI over a channel. The UI (not this thread) saves the resulting cache,
//! and only when the scan runs to completion — a cancelled scan stores
//! nothing.

use super::types::{PluginCache, PluginInfo, RejectedEntry, ScanOutput};
use super::{PluginFormat, discover, moduleinfo};
use crossbeam_channel::{Receiver, Sender};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    /// Scan everything, discarding the previous cache.
    RescanAll,
    /// Keep cache entries whose files are unchanged; scan only new files.
    NewOnly,
}

pub enum ScanEvent {
    /// Sent once, after enumeration: how many plugins will be scanned.
    Started {
        total: usize,
    },
    Progress {
        done: usize,
        total: usize,
        current: String,
    },
    Finished {
        cache: PluginCache,
        found: usize,
        rejected: usize,
        skipped_other_arch: usize,
        skipped_by_user: usize,
    },
    Cancelled,
}

pub struct ScanHandle {
    pub events: Receiver<ScanEvent>,
    pub cancel: Arc<AtomicBool>,
    /// Set by the UI's Skip button: abandon the plugin currently being
    /// probed and move on. Cleared by the worker for each new candidate.
    pub skip: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ScanHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn start_scan(
    folders: Vec<String>,
    mode: ScanMode,
    existing: PluginCache,
) -> Result<ScanHandle, String> {
    let helper = helper_path()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let skip = Arc::new(AtomicBool::new(false));
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel_for_thread = cancel.clone();
    let skip_for_thread = skip.clone();
    let thread = std::thread::Builder::new()
        .name("vst-scan".to_string())
        .spawn(move || {
            run_scan(
                &helper,
                &folders,
                mode,
                existing,
                &tx,
                &cancel_for_thread,
                &skip_for_thread,
            )
        })
        .map_err(|e| format!("Could not start the scan thread: {e}"))?;
    Ok(ScanHandle {
        events: rx,
        cancel,
        skip,
        thread: Some(thread),
    })
}

fn helper_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe failed: {e}"))?;
    let helper = exe.with_file_name("pubsplash-scan.exe");
    if helper.is_file() {
        Ok(helper)
    } else {
        Err(format!(
            "The plugin scanner ({}) is missing. Reinstall Pubsplash to restore it.",
            helper.display()
        ))
    }
}

/// (file size, mtime as unix seconds) — the cache's change-detection key.
fn stat(path: &Path) -> (u64, u64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (0, 0);
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (meta.len(), modified)
}

fn run_scan(
    helper: &Path,
    folders: &[String],
    mode: ScanMode,
    existing: PluginCache,
    events: &Sender<ScanEvent>,
    cancel: &AtomicBool,
    skip: &AtomicBool,
) {
    let discovery = discover::discover(folders, cancel);
    if cancel.load(Ordering::Relaxed) {
        let _ = events.send(ScanEvent::Cancelled);
        return;
    }

    // For "scan for new", carry over entries whose file is unchanged.
    let (kept_plugins, kept_rejected) = match mode {
        ScanMode::RescanAll => (Vec::new(), Vec::new()),
        ScanMode::NewOnly => {
            let unchanged = |path: &str, size: u64, modified: u64| {
                let (now_size, now_modified) = stat(Path::new(path));
                now_size == size && now_modified == modified && now_size != 0
            };
            (
                existing
                    .plugins
                    .into_iter()
                    .filter(|p| unchanged(&p.path, p.file_size, p.modified))
                    .collect::<Vec<_>>(),
                existing
                    .rejected
                    .into_iter()
                    .filter(|r| unchanged(&r.path, r.file_size, r.modified))
                    .collect::<Vec<_>>(),
            )
        }
    };
    let known: std::collections::HashSet<String> = kept_plugins
        .iter()
        .map(|p| p.path.to_lowercase())
        .chain(kept_rejected.iter().map(|r| r.path.to_lowercase()))
        .collect();

    let to_scan: Vec<&discover::Candidate> = discovery
        .candidates
        .iter()
        .filter(|c| !known.contains(&c.path.to_string_lossy().to_lowercase()))
        .collect();

    let total = to_scan.len();
    let _ = events.send(ScanEvent::Started { total });

    let mut plugins = kept_plugins;
    let mut rejected = kept_rejected;
    let mut found = 0usize;
    let mut newly_rejected = 0usize;
    let mut skipped_by_user = 0usize;

    for (index, candidate) in to_scan.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let _ = events.send(ScanEvent::Cancelled);
            return;
        }
        skip.store(false, Ordering::Relaxed);
        let (file_size, modified) = stat(&candidate.path);

        // VST3 bundles with moduleinfo.json are identified without loading.
        let shortcut = candidate
            .bundle
            .as_deref()
            .and_then(moduleinfo::scan_bundle);
        let result = match shortcut {
            Some(result) => result,
            None => match run_helper(helper, candidate, cancel, skip) {
                HelperResult::Ok(out) => Ok(out),
                HelperResult::Failed(reason) => Err(reason),
                HelperResult::Skipped => {
                    log::info!("Skipped by user during scan: {}", candidate.path.display());
                    skipped_by_user += 1;
                    rejected.push(RejectedEntry {
                        path: candidate.path.to_string_lossy().to_string(),
                        file_size,
                        modified,
                        reason: "skipped by user during scan".to_string(),
                    });
                    let _ = events.send(ScanEvent::Progress {
                        done: index + 1,
                        total,
                        current: candidate.display.clone(),
                    });
                    continue;
                }
                HelperResult::Cancelled => {
                    let _ = events.send(ScanEvent::Cancelled);
                    return;
                }
            },
        };

        match result {
            Ok(out) => {
                found += 1;
                plugins.push(PluginInfo {
                    path: candidate.path.to_string_lossy().to_string(),
                    format: candidate.format,
                    name: if out.name.is_empty() {
                        candidate.display.clone()
                    } else {
                        out.name
                    },
                    vendor: out.vendor,
                    version: out.version,
                    unique_id: out.unique_id,
                    class_ids: out.class_ids,
                    file_size,
                    modified,
                });
            }
            Err(reason) => {
                log::info!(
                    "Not a usable {} plugin: {} ({reason})",
                    candidate.format.display_name(),
                    candidate.path.display()
                );
                newly_rejected += 1;
                rejected.push(RejectedEntry {
                    path: candidate.path.to_string_lossy().to_string(),
                    file_size,
                    modified,
                    reason,
                });
            }
        }
        let _ = events.send(ScanEvent::Progress {
            done: index + 1,
            total,
            current: candidate.display.clone(),
        });
    }

    plugins.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    rejected.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
    let _ = events.send(ScanEvent::Finished {
        cache: PluginCache {
            version: super::types::CACHE_VERSION,
            plugins,
            rejected,
        },
        found,
        rejected: newly_rejected,
        skipped_other_arch: discovery.skipped_other_arch,
        skipped_by_user,
    });
}

enum HelperResult {
    Ok(ScanOutput),
    Failed(String),
    Skipped,
    Cancelled,
}

/// Runs the helper for one plugin. There is deliberately no timeout: plugins
/// may show activation dialogs and legitimately wait on the user. Cancel
/// kills the helper and abandons the scan; Skip kills the helper and moves
/// on to the next plugin.
fn run_helper(
    helper: &Path,
    candidate: &discover::Candidate,
    cancel: &AtomicBool,
    skip: &AtomicBool,
) -> HelperResult {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let flag = match candidate.format {
        PluginFormat::Vst2 => "--vst2",
        PluginFormat::Vst3 => "--vst3",
    };
    let mut child = match std::process::Command::new(helper)
        .arg(flag)
        .arg(&candidate.path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return HelperResult::Failed(format!("could not run scan helper: {e}")),
    };

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return HelperResult::Cancelled;
                }
                if skip.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return HelperResult::Skipped;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return HelperResult::Failed(format!("scan helper failed: {e}")),
        }
    };

    let mut stdout = String::new();
    let mut stderr = String::new();
    use std::io::Read;
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }

    if status.success() {
        match serde_json::from_str::<ScanOutput>(stdout.trim()) {
            Ok(out) => HelperResult::Ok(out),
            Err(e) => HelperResult::Failed(format!("unreadable scan helper output: {e}")),
        }
    } else if let Ok(out) = serde_json::from_str::<ScanOutput>(stdout.trim()) {
        // The probe finished and printed its result before the crash — the
        // plugin only blew up during process teardown. That's the host's
        // problem to survive, not a reason to reject the plugin.
        log::info!(
            "{} crashed during scan helper teardown after a successful probe ({status}); keeping it",
            candidate.path.display()
        );
        HelperResult::Ok(out)
    } else {
        let reason = stderr.trim();
        if reason.is_empty() {
            HelperResult::Failed(format!("plugin crashed while loading ({status})"))
        } else {
            HelperResult::Failed(reason.to_string())
        }
    }
}
