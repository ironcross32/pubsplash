//! HTTP plumbing for the updater: a client, the GitHub URLs, and a hashed,
//! cancellable download.
//!
//! A client of its own rather than `tts::net::client`, for one reason that
//! matters: that one sets a client-wide `.timeout(30s)`, which in reqwest covers
//! the *whole* request including the streamed body. A ten-megabyte installer on
//! a slow line would be guillotined mid-download and look like a network fault.
//! Only the connect phase is bounded here; a download that is making progress is
//! allowed to take as long as it takes, and the user's Cancel button is what
//! stops one that is not.
//!
//! Redirects are left at reqwest's default of "follow", unlike
//! `AudioPubClient`'s `Policy::none()`: `releases/latest/download/<name>` is a
//! 302 to `objects.githubusercontent.com`, so refusing to follow it would break
//! every download.

use super::manifest::{Asset, Manifest};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The repository every URL is built from. One constant so a fork needs one
/// edit, and so nothing can drift into pointing at a different repo.
pub const REPO: &str = "ironcross32/pubsplash";

/// The manifest asset name. Must match what the release workflow writes.
pub const MANIFEST_ASSET: &str = "latest.json";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How often a download reports progress. The UI reads this once a second at
/// most, and a screen reader should not be handed a new number faster than it
/// can say the last one.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("pubsplash/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default()
    })
}

/// The stable download URL for a release asset.
///
/// `releases/latest/download/<name>` is GitHub's own "newest release" redirect,
/// which is why the workflow uploads every updater-facing artifact under a
/// version-free name as well as its versioned one.
pub fn asset_url(name: &str) -> String {
    format!("https://github.com/{REPO}/releases/latest/download/{name}")
}

/// Fetches and parses `latest.json`.
pub async fn fetch_manifest() -> Result<Manifest, String> {
    let url = asset_url(MANIFEST_ASSET);
    let response = client()
        .get(&url)
        .send()
        .await
        .map_err(|e| describe(&e))?;
    let status = response.status();
    if !status.is_success() {
        // A 404 is the ordinary case where a tag exists but its assets have not
        // finished uploading, so it gets its own wording rather than a number.
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err("GitHub has no release information yet. Try again shortly.".to_string());
        }
        return Err(format!("GitHub answered {status}."));
    }
    let text = response.text().await.map_err(|e| describe(&e))?;
    parse_manifest(&text)
}

/// Parses the manifest body.
///
/// The leading byte-order mark is stripped first. `pwsh`'s `Out-File -Encoding
/// utf8` does not write one, but Windows PowerShell 5.1's does, and serde
/// rejects a BOM as a syntax error at position 0 — an easy way for a workflow
/// edit to break updates for everyone at once, and cheap to be immune to.
fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let text = text.trim_start_matches('\u{feff}');
    serde_json::from_str(text)
        .map_err(|e| format!("GitHub's release information could not be read: {e}"))
}

/// What a download reported while it ran.
pub enum Progress {
    /// Bytes received so far, and the total if the release said one.
    Bytes { done: u64, total: u64 },
}

/// Streams `asset` to `destination`, hashing as it goes.
///
/// The hash is computed from the bytes as they arrive rather than by re-reading
/// the file afterwards, so what is verified is what was written. Size is checked
/// first because a wrong length is the common failure (a truncated transfer, or
/// an HTML error page served in place of the file) and says so more clearly than
/// a hash mismatch would.
///
/// `cancel` is polled per chunk; a cancelled download removes its partial file.
pub async fn download(
    asset: &Asset,
    destination: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(Progress),
) -> Result<(), String> {
    let url = asset_url(&asset.name);
    let response = client()
        .get(&url)
        .send()
        .await
        .map_err(|e| describe(&e))?;
    if !response.status().is_success() {
        return Err(format!(
            "Downloading {} failed: GitHub answered {}.",
            asset.name,
            response.status()
        ));
    }

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    let mut file = std::fs::File::create(destination)
        .map_err(|e| format!("Could not create {}: {e}", destination.display()))?;

    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut last_report = Instant::now();
    let mut body = response.bytes_stream();

    // Any early exit past this point leaves a partial file behind, so every one
    // of them goes through `abandon`.
    while let Some(chunk) = body.next().await {
        if cancel.load(Ordering::Relaxed) {
            abandon(&mut file, destination);
            return Err(CANCELLED.to_string());
        }
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => {
                abandon(&mut file, destination);
                return Err(describe(&e));
            }
        };
        if let Err(e) = file.write_all(&chunk) {
            let message = format!("Could not write {}: {e}", destination.display());
            abandon(&mut file, destination);
            return Err(message);
        }
        hasher.update(&chunk);
        done += chunk.len() as u64;
        // Guard against a release that over-runs its stated size, which would
        // mean the manifest and the asset disagree.
        if done > asset.size {
            abandon(&mut file, destination);
            return Err(format!(
                "The download of {} is larger than the release says it should be.",
                asset.name
            ));
        }
        if last_report.elapsed() >= PROGRESS_INTERVAL {
            last_report = Instant::now();
            on_progress(Progress::Bytes {
                done,
                total: asset.size,
            });
        }
    }

    if let Err(e) = file.flush() {
        let message = format!("Could not write {}: {e}", destination.display());
        abandon(&mut file, destination);
        return Err(message);
    }
    drop(file);

    if done != asset.size {
        let _ = std::fs::remove_file(destination);
        return Err(format!(
            "The download of {} is incomplete ({done} bytes of {}).",
            asset.name, asset.size
        ));
    }
    let digest = format!("{:x}", hasher.finalize());
    if !digest.eq_ignore_ascii_case(&asset.sha256) {
        let _ = std::fs::remove_file(destination);
        return Err(format!(
            "The download of {} does not match the release's checksum, so it has not been used.",
            asset.name
        ));
    }
    on_progress(Progress::Bytes {
        done,
        total: asset.size,
    });
    Ok(())
}

/// The message a cancelled download reports, so callers can tell "the user
/// stopped this" from a real failure without a second channel.
pub const CANCELLED: &str = "The update was cancelled.";

/// Closes and deletes a partial download. Best-effort: a file we cannot remove
/// is cleaned up by `cleanup_leftovers` on the next start.
fn abandon(file: &mut std::fs::File, path: &Path) {
    let _ = file.flush();
    let _ = std::fs::remove_file(path);
}

/// Turns a reqwest error into something worth showing a user.
///
/// `reqwest::Error`'s own `Display` reads as network debugging — a URL, a chain
/// of source errors, sometimes a socket address. These lines end up in a message
/// box and in the log a user is asked to quote.
fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "GitHub did not answer in time.".to_string()
    } else if error.is_connect() {
        "Could not reach GitHub. Check your internet connection.".to_string()
    } else if error.is_body() || error.is_decode() {
        "The connection to GitHub was interrupted.".to_string()
    } else {
        format!("Could not reach GitHub: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_urls_are_the_stable_latest_form() {
        assert_eq!(
            asset_url("pubsplash-setup.exe"),
            "https://github.com/ironcross32/pubsplash/releases/latest/download/pubsplash-setup.exe"
        );
        assert_eq!(
            asset_url(MANIFEST_ASSET),
            "https://github.com/ironcross32/pubsplash/releases/latest/download/latest.json"
        );
    }

    /// Byte-for-byte what the release workflow's "Write latest.json" step
    /// produced when run against stand-in files, padded spacing and all. This is
    /// the contract between the workflow and the app: if someone rewrites that
    /// step and this stops parsing, updates break silently for every user at
    /// once, and the only symptom is a check that always reports a failure.
    const WORKFLOW_OUTPUT: &str = r#"{
    "version":  "0.1.5",
    "notes_url":  "https://github.com/ironcross32/pubsplash/releases/tag/v0.1.5",
    "installer":  {
                      "name":  "pubsplash-setup.exe",
                      "sha256":  "7d0c7a9c2355f573af56d9c2e32bc05be3d991556a378ad5a8f62b308e615f3c",
                      "size":  19
                  },
    "portable":  {
                     "name":  "pubsplash-portable.zip",
                     "sha256":  "9d0dc2bce91e55a7349032d13746a2ffe791588603032422583816c5cf050d95",
                     "size":  13
                 }
}"#;

    #[test]
    fn parses_what_the_workflow_actually_writes() {
        let manifest = parse_manifest(WORKFLOW_OUTPUT).expect("the workflow's output parses");
        assert_eq!(manifest.version, "0.1.5");
        assert_eq!(manifest.installer.name, "pubsplash-setup.exe");
        assert_eq!(manifest.portable.name, "pubsplash-portable.zip");
        assert_eq!(manifest.installer.size, 19);
        // The version must carry no `v`: it is compared against
        // CARGO_PKG_VERSION, which never has one.
        assert!(!manifest.version.starts_with('v'));
    }

    #[test]
    fn a_byte_order_mark_does_not_break_the_manifest() {
        let with_bom = format!("\u{feff}{WORKFLOW_OUTPUT}");
        assert!(parse_manifest(&with_bom).is_ok());
    }

    #[test]
    fn a_manifest_that_is_not_json_is_reported_not_ignored() {
        // What a captive portal or a proxy error page looks like.
        assert!(parse_manifest("<!DOCTYPE html><html>Sign in</html>").is_err());
    }
}
