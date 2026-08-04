//! Semantic-version parsing and comparison, for deciding whether a release on
//! GitHub is newer than the running build.
//!
//! Deliberately tiny and hand-rolled rather than a `semver` dependency: release
//! tags are checked by the workflow against `^v[0-9]+\.[0-9]+\.[0-9]+$` (see
//! `.github/workflows/release.yml`), so the only shape that can ever reach here
//! is three numbers. Anything else is a malformed release and must compare as
//! "not newer" — never as an update, which would send a user to download
//! something the server does not really have.

/// A parsed `major.minor.patch`.
pub type Version = (u32, u32, u32);

/// Parses `0.1.5` or `v0.1.5`. Returns `None` for anything else, including
/// pre-release and build-metadata suffixes: we do not publish them, so one
/// appearing means something is wrong and the safe answer is to stand down.
pub fn parse(text: &str) -> Option<Version> {
    let text = text.trim();
    let text = text.strip_prefix('v').unwrap_or(text);
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    // A fourth component means this is not the shape we publish.
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Whether `latest` is a version worth offering over `current`.
///
/// Both sides must parse. An unparseable `latest` is a malformed release; an
/// unparseable `current` cannot happen (it is `CARGO_PKG_VERSION`), but the
/// answer is still no rather than a panic.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse(latest), parse(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// The version this build was compiled as.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_prefixed() {
        assert_eq!(parse("0.1.5"), Some((0, 1, 5)));
        assert_eq!(parse("v0.1.5"), Some((0, 1, 5)));
        assert_eq!(parse("  v10.20.30 "), Some((10, 20, 30)));
    }

    #[test]
    fn rejects_anything_that_is_not_three_numbers() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("v"), None);
        assert_eq!(parse("1.2"), None);
        assert_eq!(parse("1.2.3.4"), None);
        assert_eq!(parse("1.2.x"), None);
        assert_eq!(parse("1.2.3-beta"), None);
        assert_eq!(parse("1.2.3+build"), None);
        assert_eq!(parse("-1.2.3"), None);
    }

    #[test]
    fn compares_component_by_component() {
        assert!(is_newer("0.1.5", "0.1.4"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(is_newer("1.0.0", "0.99.99"));
        // 10 is newer than 9, which a string compare would get wrong.
        assert!(is_newer("0.1.10", "0.1.9"));
    }

    #[test]
    fn same_or_older_is_not_newer() {
        assert!(!is_newer("0.1.4", "0.1.4"));
        assert!(!is_newer("0.1.3", "0.1.4"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn unparseable_is_never_newer() {
        assert!(!is_newer("garbage", "0.1.4"));
        assert!(!is_newer("0.1.5", "garbage"));
        assert!(!is_newer("", ""));
    }

    #[test]
    fn our_own_version_parses() {
        assert!(parse(current()).is_some(), "CARGO_PKG_VERSION must parse");
    }
}
