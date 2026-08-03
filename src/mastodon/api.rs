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
    /// The host answered, but it is not running the Mastodon API. `why` is a
    /// lower-case fragment naming what gave it away.
    NotMastodon {
        host: String,
        why: String,
    },
    /// Nothing answered at all: no such name, refused, timed out, bad TLS.
    /// Separate from [`MastodonError::Network`] because the wording is ours
    /// rather than `reqwest`'s.
    Unreachable {
        host: String,
        why: String,
    },
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
            MastodonError::NotMastodon { host, why } => write!(
                f,
                "{host} answered, but it is not a Mastodon server ({why}). \
                 Check the address, or ask the server's admin."
            ),
            MastodonError::Unreachable { host, why } => {
                write!(f, "Could not reach {host}: {why}.")
            }
            MastodonError::Service { status, detail } => match *status {
                401 | 403 => write!(f, "The server refused the request ({status}): {detail}"),
                404 | 410 => write!(f, "The server has no such address ({status}): {detail}"),
                429 => write!(f, "The server is rate-limiting Pubsplash: {detail}"),
                500..=599 => write!(f, "The server had trouble ({status}): {detail}"),
                _ => write!(f, "The server answered {status}: {detail}"),
            },
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

/// The host half of a normalized instance URL, for messages the user reads.
fn host_of(instance: &str) -> String {
    instance
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_string()
}

/// What one probe of an instance-description endpoint concluded.
enum Probe {
    /// It is a Mastodon-compatible server, or close enough that only the real
    /// calls can tell.
    Usable,
    /// This endpoint is not there. Worth trying the other version. `web_page`
    /// records whether the answer was HTML, since that is what a host with no
    /// API at all sends.
    Absent { web_page: bool },
    /// Conclusive. Stop and report this.
    Failed(MastodonError),
}

/// Asks a host whether it speaks the Mastodon API, before anything else does.
///
/// Without this the first contact is `POST /api/v1/apps`, so a typo or a plain
/// web site answers with its own 404 page and the user is read a screenful of
/// HTML. Run from [`super::oauth::run`] before a port is bound, an app is
/// registered, or a browser window opens.
///
/// The check is deliberately loose. Pleroma, Akkoma, GoToSocial and Firefish
/// all implement this API and its OAuth flow; refusing them would break setups
/// that work today.
pub async fn check_instance(instance: &str) -> Result<(), MastodonError> {
    let host = host_of(instance);
    let mut web_page = false;
    // `/api/v2/instance` arrived in Mastodon 4.0; older servers and some forks
    // only serve v1, so a 404 on the first is not an answer.
    for path in ["/api/v2/instance", "/api/v1/instance"] {
        match probe(instance, path, &host).await {
            Probe::Usable => return Ok(()),
            Probe::Absent { web_page: seen } => web_page |= seen,
            Probe::Failed(error) => return Err(error),
        }
    }
    Err(MastodonError::NotMastodon {
        host,
        why: if web_page {
            "it answered with a web page instead of API data"
        } else {
            "it has no Mastodon API"
        }
        .into(),
    })
}

async fn probe(instance: &str, path: &str, host: &str) -> Probe {
    let response = match net::client().get(format!("{instance}{path}")).send().await {
        Ok(response) => response,
        Err(error) => {
            return Probe::Failed(MastodonError::Unreachable {
                host: host.to_string(),
                why: transport_reason(&error),
            });
        }
    };
    let status = response.status();
    let html = is_html(&response);
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let code = status.as_u16();
        return match code {
            // A private or allowlisted instance can refuse an anonymous read of
            // its own description while still accepting a registration. The
            // probe cannot tell, so it defers to the calls that follow rather
            // than failing a setup that would have worked.
            401 | 403 => Probe::Usable,
            500..=599 => Probe::Failed(MastodonError::NotMastodon {
                host: host.to_string(),
                why: format!("it answered {code}; it may be down or restarting"),
            }),
            _ => Probe::Absent {
                web_page: html || looks_like_html(&body),
            },
        };
    }
    match instance_reply(&body, html) {
        Ok(()) => Probe::Usable,
        Err(why) => Probe::Failed(MastodonError::NotMastodon {
            host: host.to_string(),
            why: why.to_string(),
        }),
    }
}

/// Decides whether a 200 from an instance-description endpoint really is one.
///
/// Any JSON object carrying one of the fields every implementation sets counts;
/// see the note on [`check_instance`] about why this is not stricter.
fn instance_reply(body: &str, html: bool) -> Result<(), &'static str> {
    if html || looks_like_html(body) {
        return Err("it answered with a web page instead of API data");
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body.trim()) else {
        return Err("its reply was not API data");
    };
    let wrong_shape = "its reply was not a Mastodon instance description";
    let Some(object) = value.as_object() else {
        return Err(wrong_shape);
    };
    if ["domain", "uri", "title", "version"]
        .iter()
        .any(|key| object.contains_key(*key))
    {
        Ok(())
    } else {
        Err(wrong_shape)
    }
}

/// Says why nothing answered, in the user's words rather than `reqwest`'s.
///
/// `reqwest`'s own `Display` is a debug chain ("error sending request for url
/// (…): error trying to connect: dns error: …") — accurate, and useless read
/// aloud. The cause is only in the innermost source, so the chain is walked.
fn transport_reason(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "it did not answer in time".into();
    }
    let mut deepest: &dyn std::error::Error = error;
    while let Some(next) = deepest.source() {
        deepest = next;
    }
    let text = deepest.to_string().to_ascii_lowercase();
    if text.contains("dns") || text.contains("lookup") || text.contains("name or service") {
        "that host name could not be found".into()
    } else if ["certificate", "tls", "ssl", "handshake"]
        .iter()
        .any(|hint| text.contains(hint))
    {
        "the secure connection could not be set up".into()
    } else if error.is_connect() {
        "the connection was refused".into()
    } else if error.is_request() {
        "the request could not be sent".into()
    } else {
        "the connection failed".into()
    }
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
    let reply: AppReply = json_body(response, "the app's client id")
        .await
        // A host that cleared [`check_instance`] and still has no registration
        // endpoint is serving a partial API; that is worth naming rather than
        // quoting a status code at the user.
        .map_err(|error| match error {
            MastodonError::Service {
                status: 404 | 410, ..
            } => MastodonError::NotMastodon {
                host: host_of(instance),
                why: "it does not accept app registrations".into(),
            },
            other => other,
        })?;
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
    let html = is_html(&response);
    let body = response.text().await.unwrap_or_default();
    Err(MastodonError::Service {
        status: status.as_u16(),
        detail: if html {
            WEB_PAGE.into()
        } else {
            summarize(&body)
        },
    })
}

/// Does the answer claim to be HTML? Checked before the body is consumed,
/// because [`reqwest::Response::text`] takes the response by value.
fn is_html(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
}

/// The content-type header is advisory; plenty of error pages are served as
/// `text/plain` or with no type at all, so the body is sniffed too.
fn looks_like_html(body: &str) -> bool {
    let head = body.trim_start_matches('\u{feff}').trim_start();
    // A leading `<` is enough: no JSON or plain-text message an API sends
    // starts with one, and a bare `<h1>Not Found</h1>` is as unreadable as a
    // full document.
    if head.starts_with('<') {
        return true;
    }
    let start: String = head
        .chars()
        .take(200)
        .collect::<String>()
        .to_ascii_lowercase();
    start.contains("<!doctype") || start.contains("<html")
}

async fn json_body<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    wanted: &str,
) -> Result<T, MastodonError> {
    let response = check_status(response).await?;
    let body = response.text().await?;
    serde_json::from_str(&body).map_err(|_| MastodonError::Malformed(wanted.to_string()))
}

/// What a body is reduced to when it turns out to be markup. Quoting 200
/// characters of a 404 page is what this whole path exists to prevent: a screen
/// reader reads every angle bracket, and none of it says what went wrong.
const WEB_PAGE: &str = "the server sent a web page instead of an error message";

/// Trims an error body to something a screen reader can get through, pulling
/// out Mastodon's `error_description`/`error` when it is there.
pub(super) fn summarize(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "no detail given".into();
    }
    if looks_like_html(trimmed) {
        return WEB_PAGE.into();
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

    /// The bug this file was changed for: `google.com` in the Server box put a
    /// web server's 404 page into a modal, one angle bracket at a time.
    #[test]
    fn a_web_page_is_never_quoted_back_at_the_user() {
        for body in [
            "<!DOCTYPE html>\n<html lang=en>\n  <meta charset=utf-8>\n  <title>Error 404</title>",
            "<html><body><h1>404 Not Found</h1></body></html>",
            "<h1>Not Found</h1>",
            "\u{feff}  <!doctype html><p>nope",
        ] {
            let detail = summarize(body);
            assert_eq!(detail, WEB_PAGE, "body {body:?}");
            assert!(!detail.contains('<'), "markup survived: {detail}");
        }
        // A plain-text or JSON message still comes through as itself.
        assert_eq!(summarize("Not Found"), "Not Found");
        assert_eq!(summarize(r#"{"error":"invalid_grant"}"#), "invalid_grant");
        assert!(!looks_like_html(r#"{"domain":"m.social"}"#));
    }

    #[test]
    fn only_an_instance_description_counts_as_a_mastodon_server() {
        // Loose on purpose: the forks all serve this with their own extras.
        for body in [
            r#"{"domain":"mastodon.social","title":"Mastodon"}"#,
            r#"{"uri":"pleroma.test"}"#,
            r#"{"title":"GoToSocial","version":"0.16.0"}"#,
            r#"{"version":"4.3.0","other":[1,2]}"#,
        ] {
            assert_eq!(instance_reply(body, false), Ok(()), "body {body:?}");
        }
        for body in ["[]", "{}", r#"{"ok":true}"#, "\"not json\"", "null"] {
            assert!(instance_reply(body, false).is_err(), "body {body:?}");
        }
        // A web page fails for its own reason, whether the header says so or
        // the body gives it away.
        assert_eq!(
            instance_reply("<!doctype html><html>", false),
            Err("it answered with a web page instead of API data")
        );
        assert_eq!(
            instance_reply(r#"{"domain":"x.test"}"#, true),
            Err("it answered with a web page instead of API data")
        );
        assert_eq!(
            instance_reply("not json at all", false),
            Err("its reply was not API data")
        );
    }

    #[test]
    fn the_new_failures_name_the_host_and_what_to_do() {
        let not_mastodon = MastodonError::NotMastodon {
            host: "google.com".into(),
            why: "it answered with a web page instead of API data".into(),
        }
        .to_string();
        assert!(
            not_mastodon.starts_with("google.com answered"),
            "{not_mastodon}"
        );
        assert!(
            not_mastodon.contains("not a Mastodon server"),
            "{not_mastodon}"
        );
        assert!(not_mastodon.contains("Check the address"), "{not_mastodon}");

        let unreachable = MastodonError::Unreachable {
            host: "nosuchhost.invalid".into(),
            why: "that host name could not be found".into(),
        }
        .to_string();
        assert_eq!(
            unreachable,
            "Could not reach nosuchhost.invalid: that host name could not be found."
        );
    }

    #[test]
    fn a_status_says_what_kind_of_refusal_it_was() {
        let say = |status, detail: &str| {
            MastodonError::Service {
                status,
                detail: detail.into(),
            }
            .to_string()
        };
        assert!(say(403, "banned").contains("refused the request (403)"));
        assert!(say(404, WEB_PAGE).contains("no such address (404)"));
        assert!(say(429, "slow down").contains("rate-limiting"));
        assert!(say(503, "maintenance").contains("had trouble (503)"));
        // Anything unclassified keeps the original wording.
        assert_eq!(say(418, "teapot"), "The server answered 418: teapot");
    }

    /// The whole point of the probe, against the real internet. Ignored by
    /// default like the other live tests; run with
    /// `cargo test the_probe -- --include-ignored`.
    #[test]
    #[ignore]
    fn the_probe_tells_a_web_site_from_a_mastodon_server() {
        assert_eq!(
            net::block_on(check_instance("https://mastodon.social")),
            Ok(())
        );

        let web_site = net::block_on(check_instance("https://google.com")).unwrap_err();
        let message = web_site.to_string();
        assert!(
            matches!(web_site, MastodonError::NotMastodon { .. }),
            "{message}"
        );
        assert!(!message.contains('<'), "markup reached the user: {message}");
        assert!(message.starts_with("google.com answered"), "{message}");
        assert!(
            message.contains("web page instead of API data"),
            "{message}"
        );

        let nowhere = net::block_on(check_instance("https://nosuchhost.invalid")).unwrap_err();
        assert!(
            matches!(nowhere, MastodonError::Unreachable { .. }),
            "{nowhere:?}"
        );
    }

    #[test]
    fn hosts_come_back_out_of_a_normalized_url() {
        assert_eq!(host_of("https://mastodon.social"), "mastodon.social");
        assert_eq!(
            host_of("https://my.server.test:3000"),
            "my.server.test:3000"
        );
        assert_eq!(
            host_of(&normalize_instance("@me@mastodon.social").unwrap()),
            "mastodon.social"
        );
    }
}
