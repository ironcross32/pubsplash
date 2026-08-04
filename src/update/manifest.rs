//! `latest.json` — the release description Pubsplash reads to decide whether an
//! update exists.
//!
//! Published as a release asset by the workflow rather than read from the GitHub
//! API, for two reasons. `api.github.com` is rate-limited to 60 requests an hour
//! per *IP*, which a shared or NAT'd network burns through without the user
//! doing anything wrong, and it 403s a request with no `User-Agent`. The asset
//! download endpoint has neither problem. It also lets the schema carry the one
//! thing the API does not: a SHA-256 per artifact, so a truncated or mangled
//! download is caught before anything is run.
//!
//! Schema changes must stay additive. Serde ignores unknown fields by default,
//! which is deliberate here: a future release that adds a key must still be
//! readable by today's build, or the users who most need to update are exactly
//! the ones who cannot see it.

use super::install_kind::InstallKind;
use serde::Deserialize;

/// One downloadable file in a release.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Asset {
    /// The stable, version-free file name — what
    /// `releases/latest/download/<name>` resolves against.
    pub name: String,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: String,
    /// Expected size in bytes. Checked before the hash so an obviously wrong
    /// download is rejected without reading it twice.
    pub size: u64,
}

/// The contents of `latest.json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Manifest {
    /// `major.minor.patch`, with no `v`.
    pub version: String,
    /// The release page, for the "what changed" link and for install layouts
    /// that cannot update themselves.
    pub notes_url: String,
    pub installer: Asset,
    pub portable: Asset,
}

impl Manifest {
    /// The asset that suits this install layout.
    ///
    /// `Unknown` has no answer — it never self-updates — which is why this
    /// returns an `Option` rather than defaulting to one of them.
    pub fn asset_for(&self, kind: InstallKind) -> Option<&Asset> {
        match kind {
            InstallKind::Installed => Some(&self.installer),
            InstallKind::Portable => Some(&self.portable),
            InstallKind::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "version": "0.1.5",
        "notes_url": "https://github.com/ironcross32/pubsplash/releases/tag/v0.1.5",
        "installer": { "name": "pubsplash-setup.exe", "sha256": "abc123", "size": 10160085 },
        "portable":  { "name": "pubsplash-portable.zip", "sha256": "def456", "size": 9000000 }
    }"#;

    fn sample() -> Manifest {
        serde_json::from_str(SAMPLE).expect("the sample manifest parses")
    }

    #[test]
    fn parses_the_published_shape() {
        let manifest = sample();
        assert_eq!(manifest.version, "0.1.5");
        assert_eq!(manifest.installer.name, "pubsplash-setup.exe");
        assert_eq!(manifest.installer.size, 10_160_085);
        assert_eq!(manifest.portable.sha256, "def456");
    }

    /// A later release adding a key must not break today's build, or the users
    /// who need the update are the ones who cannot see it.
    #[test]
    fn unknown_fields_are_ignored() {
        let text = SAMPLE.replace(
            "\"version\": \"0.1.5\",",
            "\"version\": \"0.1.5\", \"minimum_version\": \"0.1.0\", \"channel\": \"stable\",",
        );
        let manifest: Manifest = serde_json::from_str(&text).expect("extra keys are tolerated");
        assert_eq!(manifest.version, "0.1.5");
    }

    #[test]
    fn a_missing_required_field_is_an_error() {
        let text = SAMPLE.replace("\"notes_url\"", "\"notesUrl\"");
        assert!(serde_json::from_str::<Manifest>(&text).is_err());
    }

    #[test]
    fn picks_the_asset_for_the_layout() {
        let manifest = sample();
        assert_eq!(
            manifest.asset_for(InstallKind::Installed).map(|a| &*a.name),
            Some("pubsplash-setup.exe")
        );
        assert_eq!(
            manifest.asset_for(InstallKind::Portable).map(|a| &*a.name),
            Some("pubsplash-portable.zip")
        );
        assert_eq!(manifest.asset_for(InstallKind::Unknown), None);
    }
}
