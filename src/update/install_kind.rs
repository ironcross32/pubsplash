//! Telling an installed Pubsplash from a portable one, which decides how an
//! update is applied.
//!
//! The two shipped layouts are otherwise identical — the installer drops
//! everything straight into `$INSTDIR` and the portable ZIP holds the same files
//! flat — so `readme.html`, the helper exes and the docs cannot discriminate.
//! Two files can:
//!
//! - `portable.txt`, written into the ZIP by the release workflow's
//!   "Assemble portable ZIP" step. This is the deliberate marker and is checked
//!   first, so a portable copy is never mistaken for an install.
//! - `uninstall.exe`, which NSIS writes into `$INSTDIR` and which nothing else
//!   produces (verified by listing the built installer: it is the last entry in
//!   the archive, alongside the four binaries and the two HTML docs). This
//!   covers portable ZIPs cut before the marker existed, where the *absence* of
//!   an uninstaller is what identifies them.
//!
//! Anything else — a source checkout running out of `target\debug`, or a build
//! someone has copied a couple of files out of — is [`InstallKind::Unknown`] and
//! never updates itself in place. Guessing wrong there means overwriting a
//! working tree, so the app reports the new version and opens the release page
//! instead.
//!
//! Resolved from `current_exe`, never the working directory, which a shortcut's
//! "Start in" can point anywhere — the same rule `find_doc` and
//! `vst::scan::helper_path` follow.

use std::path::{Path, PathBuf};

/// The marker the release workflow writes into the portable ZIP.
pub const PORTABLE_MARKER: &str = "portable.txt";
/// What NSIS leaves in `$INSTDIR`.
pub const UNINSTALLER: &str = "uninstall.exe";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// Installed by the NSIS installer, per-user or per-machine. Updated by
    /// running the new installer.
    Installed,
    /// Unpacked from the portable ZIP. Updated by overwriting the folder.
    Portable,
    /// Neither — a source build, or a layout we do not recognise. Never
    /// updated in place.
    Unknown,
}

/// Where this copy of Pubsplash lives, and how it got there.
#[derive(Debug, Clone)]
pub struct Install {
    /// The directory holding `pubsplash.exe` and its siblings.
    pub dir: PathBuf,
    pub kind: InstallKind,
}

impl Install {
    /// Whether Pubsplash can apply an update to itself in place.
    pub fn can_self_update(&self) -> bool {
        !matches!(self.kind, InstallKind::Unknown)
    }
}

/// Resolves the running copy. `None` only if `current_exe` fails, which in
/// practice means the process is being torn down.
pub fn detect() -> Option<Install> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    let kind = classify(&dir);
    Some(Install { dir, kind })
}

/// The pure half of [`detect`], split out so the precedence is testable without
/// a real install. Order matters: the deliberate marker wins over the inferred
/// signal, so a portable ZIP that somehow also carries an `uninstall.exe` still
/// reads as portable.
pub fn classify(dir: &Path) -> InstallKind {
    if dir.join(PORTABLE_MARKER).is_file() {
        InstallKind::Portable
    } else if dir.join(UNINSTALLER).is_file() {
        InstallKind::Installed
    } else {
        InstallKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that cleans itself up, so these tests leave nothing
    /// in the temp folder even when one fails.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("pubsplash-install-kind-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("creating the scratch directory");
            Scratch(dir)
        }

        fn touch(&self, name: &str) {
            std::fs::write(self.0.join(name), b"").expect("writing a scratch file");
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bare_directory_is_unknown() {
        let scratch = Scratch::new("bare");
        assert_eq!(classify(&scratch.0), InstallKind::Unknown);
    }

    #[test]
    fn uninstaller_means_installed() {
        let scratch = Scratch::new("installed");
        scratch.touch(UNINSTALLER);
        assert_eq!(classify(&scratch.0), InstallKind::Installed);
    }

    #[test]
    fn marker_means_portable() {
        let scratch = Scratch::new("portable");
        scratch.touch(PORTABLE_MARKER);
        assert_eq!(classify(&scratch.0), InstallKind::Portable);
    }

    #[test]
    fn marker_wins_over_uninstaller() {
        let scratch = Scratch::new("both");
        scratch.touch(PORTABLE_MARKER);
        scratch.touch(UNINSTALLER);
        assert_eq!(classify(&scratch.0), InstallKind::Portable);
    }

    #[test]
    fn the_docs_alone_do_not_discriminate() {
        // Both shipped layouts carry these, which is why they are not a signal.
        let scratch = Scratch::new("docs");
        scratch.touch("readme.html");
        scratch.touch("changelog.html");
        scratch.touch("pubsplash-scan.exe");
        assert_eq!(classify(&scratch.0), InstallKind::Unknown);
    }

    #[test]
    fn unknown_never_self_updates() {
        for (kind, expected) in [
            (InstallKind::Installed, true),
            (InstallKind::Portable, true),
            (InstallKind::Unknown, false),
        ] {
            let install = Install {
                dir: PathBuf::from("."),
                kind,
            };
            assert_eq!(install.can_self_update(), expected, "{kind:?}");
        }
    }
}
