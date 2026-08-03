//! The Mastodon REST calls Pubsplash makes.
//!
//! Verified against the Mastodon API docs: app registration, the OAuth token
//! endpoint, `verify_credentials`, posting a status, and token revocation. Every
//! one of these runs on the worker thread in [`super::net`] — nothing here may
//! be called from the UI thread, because they all block.

use super::net;
use crate::secret::Secret;
use serde::Deserialize;

/// Sent as the app name at registration, so a user can recognise the entry in
/// their account's "Authorized apps" list and revoke it there.
const CLIENT_NAME: &str = "Pubsplash";
const WEBSITE: &str = "https://github.com/ironcross32/pubsplash";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MastodonError {
    /// The server field is blank or does not look like a host.
    BadInstance(String),
    Network(String),
    /// A non-2xx answer, with whatever detail the body carried.
    Service {
        status: u16,
        detail: String,
    },
    /// The reply parsed but did not hold what it should have.
    Malformed(String),
    NotLinked,
    /// The user closed the browser, or the wait timed out.
    Cancelled,
    /// A post was refused by the flood gate. Never surfaced as a failure — it
    /// means the app tried to post twice in quick succession, which is a bug
    /// upstream, not something the user did.
    RateLimited,
}

impl std::fmt::Display for MastodonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MastodonError::BadInstance(text) => write!(
                f,
                "{text:?} does not look like a Mastodon server. \
                 Enter a host name such as mastodon.social."
            ),
            MastodonError::Network(detail) => write!(f, "Could not reach the server: {detail}"),
            MastodonError::Service { status, detail } => {
                write!(f, "The server answered {status}: {detail}")
            }
            MastodonError::Malformed(what) => {
                write!(f, "The server's reply did not contain {what}.")
            }
            MastodonError::NotLinked => {
                f.write_str("No Mastodon account is linked. Link one in Preferences, Mastodon.")
            }
            MastodonError::Cancelled => f.write_str("Authorization was cancelled."),
            MastodonError::RateLimited => {
                f.write_str("Skipped a Mastodon post that came too soon after the last one.")
            }
        }
    }
}

impl From<reqwest::Error> for MastodonError {
    fn from(error: reqwest::Error) -> Self {
        MastodonError::Network(error.to_string())
    }
}

/// Turns a user-typed server into a base URL with no trailing slash.
///
/// People type `mastodon.social`, `https://mastodon.social`, `@me@host`, and
/// everything in between; only the host matters.
pub fn normalize_instance(input: &str) -> Result<String, MastodonError> {
    let raw = input.trim();
    let bad = || MastodonError::BadInstance(raw.to_string());
    if raw.is_empty() {
        return Err(bad());
    }
    let mut host = raw
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    // `@user@host` and `user@host` both give up their host half.
    if let Some((_, after)) = host.rsplit_once('@') {
        host = after;
    }
    // Drop any path the user pasted along with the host.
    let host = host.split('/').next().unwrap_or("");
    if host.is_empty()
        || !host.contains('.')
        || host.starts_with('.')
        || host.ends_with('.')
        || host
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b':'))
    {
        return Err(bad());
    }
    Ok(format!("https://{host}"))
}

/// What `POST /api/v1/apps` hands back.
#[derive(Debug, Clone)]
pub struct AppCredentials {
    pub client_id: String,
    pub client_secret: Secret,
}

/// Everything a completed authorization produces, ready to store in the config.
#[derive(Debug, Clone)]
pub struct Link {
    pub instance: String,
    pub client_id: String,
    pub client_secret: Secret,
    pub access_token: Secret,
    /// `@user@host`, for the Preferences tab to show.
    pub account: String,
}

#[derive(Deserialize)]
struct AppReply {
    client_id: String,
    client_secret: String,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
}

#[derive(Deserialize)]
struct AccountReply {
    #[serde(default)]
    acct: String,
    #[serde(default)]
    username: String,
}

/// Registers Pubsplash with `instance` for this authorization attempt.
///
/// Registration happens per attempt rather than once, because the redirect URI
/// carries the loopback port and that port is only known after the listener
/// binds.
pub async fn register_app(
    instance: &str,
    redirect_uri: &str,
) -> Result<AppCredentials, MastodonError> {
    let response = net::client()
        .post(format!("{instance}/api/v1/apps"))
        .form(&[
            ("client_name", CLIENT_NAME),
            ("redirect_uris", redirect_uri),
            ("scopes", super::SCOPES),
            ("website", WEBSITE),
        ])
        .send()
        .await?;
    let reply: AppReply = json_body(response, "the app's client id").await?;
    Ok(AppCredentials {
        client_id: reply.client_id,
        client_secret: Secret::new(reply.client_secret),
    })
}

/// The URL the user's browser is sent to.
pub fn authorize_url(
    instance: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> String {
    use urlencoding::encode;
    format!(
        "{instance}/oauth/authorize?response_type=code&client_id={}&redirect_uri={}\
         &scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        encode(client_id),
        encode(redirect_uri),
        encode(super::SCOPES),
        encode(state),
        encode(challenge),
    )
}

pub async fn exchange_code(
    instance: &str,
    app: &AppCredentials,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<Secret, MastodonError> {
    let response = net::client()
        .post(format!("{instance}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", app.client_id.as_str()),
            ("client_secret", app.client_secret.as_str()),
            ("redirect_uri", redirect_uri),
            ("code", code),
            ("code_verifier", verifier),
            ("scope", super::SCOPES),
        ])
        .send()
        .await?;
    let reply: TokenReply = json_body(response, "an access token").await?;
    Ok(Secret::new(reply.access_token))
}

/// Reads the linked account's handle, as `@user@host`.
pub async fn verify_credentials(instance: &str, token: &Secret) -> Result<String, MastodonError> {
    let response = net::client()
        .get(format!("{instance}/api/v1/accounts/verify_credentials"))
        .bearer_auth(token.as_str())
        .send()
        .await?;
    let reply: AccountReply = json_body(response, "the account").await?;
    let name = if reply.acct.is_empty() {
        reply.username
    } else {
        reply.acct
    };
    if name.is_empty() {
        return Err(MastodonError::Malformed("the account name".into()));
    }
    // `acct` is bare for local accounts, so add the host back for a handle the
    // user would recognise.
    let host = instance.trim_start_matches("https://");
    if name.contains('@') {
        Ok(format!("@{name}"))
    } else {
        Ok(format!("@{name}@{host}"))
    }
}

/// Posts a status. **Not the entry point** — go through [`net::post`], which is
/// where the flood gate lives.
pub(super) async fn post_status(
    instance: &str,
    token: &Secret,
    status: &str,
    idempotency_key: &str,
) -> Result<(), MastodonError> {
    let response = net::client()
        .post(format!("{instance}/api/v1/statuses"))
        .bearer_auth(token.as_str())
        // Belt and braces alongside the interval gate: the server drops a
        // repeat of the same key for an hour, so even two identical posts that
        // somehow clear the gate collapse into one.
        .header("Idempotency-Key", idempotency_key)
        .form(&[("status", status), ("visibility", "public")])
        .send()
        .await?;
    check_status(response).await?;
    Ok(())
}

/// Invalidates the token server-side on Unlink. Best effort: the local copy is
/// cleared either way.
pub async fn revoke(
    instance: &str,
    client_id: &str,
    client_secret: &Secret,
    token: &Secret,
) -> Result<(), MastodonError> {
    let response = net::client()
        .post(format!("{instance}/oauth/revoke"))
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret.as_str()),
            ("token", token.as_str()),
        ])
        .send()
        .await?;
    check_status(response).await?;
    Ok(())
}

async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, MastodonError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let detail = response.text().await.unwrap_or_default();
    Err(MastodonError::Service {
        status: status.as_u16(),
        detail: summarize(&detail),
    })
}

async fn json_body<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    wanted: &str,
) -> Result<T, MastodonError> {
    let response = check_status(response).await?;
    let body = response.text().await?;
    serde_json::from_str(&body).map_err(|_| MastodonError::Malformed(wanted.to_string()))
}

/// Trims an error body to something a screen reader can get through, pulling
/// out Mastodon's `error_description`/`error` when it is there.
fn summarize(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "no detail given".into();
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for key in ["error_description", "error", "message", "detail"] {
            if let Some(text) = value
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|t| !t.is_empty())
            {
                return clip(text);
            }
        }
    }
    clip(trimmed)
}

fn clip(text: &str) -> String {
    const LIMIT: usize = 200;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    text.chars().take(LIMIT).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_normalize_to_a_base_url() {
        for input in [
            "mastodon.social",
            "  mastodon.social  ",
            "https://mastodon.social",
            "http://mastodon.social",
            "https://mastodon.social/",
            "mastodon.social/explore",
            "@me@mastodon.social",
            "me@mastodon.social",
        ] {
            assert_eq!(
                normalize_instance(input).unwrap(),
                "https://mastodon.social",
                "input {input:?}"
            );
        }
        // A port survives, for self-hosted setups.
        assert_eq!(
            normalize_instance("my.server.test:3000").unwrap(),
            "https://my.server.test:3000"
        );
    }

    #[test]
    fn nonsense_instances_are_refused_by_name() {
        for input in [
            "",
            "   ",
            "localhost",
            "no dots",
            "http://",
            "..",
            "a.",
            ".a",
        ] {
            let error = normalize_instance(input).unwrap_err();
            assert!(
                matches!(error, MastodonError::BadInstance(_)),
                "input {input:?} gave {error:?}"
            );
        }
        assert!(
            normalize_instance("bad host.social")
                .unwrap_err()
                .to_string()
                .contains("mastodon.social"),
            "the error should say what a good answer looks like"
        );
    }

    #[test]
    fn the_authorize_url_carries_pkce_and_escapes_its_parts() {
        let url = authorize_url(
            "https://mastodon.social",
            "abc",
            "http://127.0.0.1:5000/",
            "st ate",
            "chal+lenge",
        );
        assert!(url.starts_with("https://mastodon.social/oauth/authorize?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("code_challenge=chal%2Blenge"), "{url}");
        assert!(url.contains("state=st%20ate"), "{url}");
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2F"),
            "{url}"
        );
        assert!(
            url.contains("scope=read%3Aaccounts%20write%3Astatuses"),
            "{url}"
        );
    }

    #[test]
    fn error_bodies_are_reduced_to_their_message() {
        assert_eq!(
            summarize(r#"{"error":"invalid_grant","error_description":"The code expired"}"#),
            "The code expired"
        );
        assert_eq!(
            summarize(r#"{"error":"Validation failed"}"#),
            "Validation failed"
        );
        assert_eq!(summarize("  Forbidden\n"), "Forbidden");
        assert_eq!(summarize(""), "no detail given");
    }
}
