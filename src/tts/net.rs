//! Shared HTTP plumbing for the network speech engines.
//!
//! [`SpeechEngine::synth`](super::engine::SpeechEngine::synth) is a blocking
//! call — the worker thread that drives it has nothing else to do — but
//! `reqwest` is async, so engines bridge through [`block_on`].
//!
//! The runtime here is `current_thread`, matching the network side's (see the
//! note on `tokio` in `Cargo.toml`). Two callers do exist: the `tts-net`
//! worker and, briefly, the UI's voice-fetch thread. A current-thread runtime
//! serializes them, which is the behaviour we want anyway — one speech request
//! at a time, in the order they were queued.

use super::engine::TtsError;
use std::sync::OnceLock;
use std::time::Duration;

/// How long a whole synthesis request may take. Long enough for a slow neural
/// voice on a long message, short enough that one wedged request doesn't hold
/// up every chat message queued behind it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Runs `future` to completion, blocking the calling thread.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let runtime = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("building the speech runtime")
    });
    runtime.block_on(future)
}

/// The shared HTTP client. One connection pool across every engine, so a
/// stream of chat messages reuses its TLS session instead of renegotiating.
pub fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .unwrap_or_default()
    })
}

impl From<reqwest::Error> for TtsError {
    fn from(error: reqwest::Error) -> Self {
        TtsError::Network(error.to_string())
    }
}

/// Turns a non-2xx response into a [`TtsError::Service`] carrying the body.
///
/// The body is where the useful part lives — "Invalid API key", "quota
/// exceeded", "voice not found" — and it is the difference between a user
/// fixing their setup and a user filing a bug about silent TTS. It is capped
/// because some services answer with an HTML error page.
pub async fn check_status(
    service: &'static str,
    response: reqwest::Response,
) -> Result<reqwest::Response, TtsError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let detail = response.text().await.unwrap_or_default();
    Err(TtsError::Service {
        service,
        status: status.as_u16(),
        detail: summarize(&detail),
    })
}

/// Checks the status and reads the body as bytes.
pub async fn body_bytes(
    service: &'static str,
    response: reqwest::Response,
) -> Result<Vec<u8>, TtsError> {
    let response = check_status(service, response).await?;
    Ok(response.bytes().await?.to_vec())
}

/// Checks the status and parses the body as JSON.
pub async fn body_json<T: serde::de::DeserializeOwned>(
    service: &'static str,
    response: reqwest::Response,
) -> Result<T, TtsError> {
    let response = check_status(service, response).await?;
    response.json().await.map_err(TtsError::from)
}

/// Trims an error body to something a screen reader can get through.
fn summarize(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "no detail given".into();
    }
    // Most of these are JSON; pull the message out when it's obvious, so the
    // user hears "Invalid API key" rather than a wall of braces.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for path in [
            &["error", "message"][..],
            &["message"][..],
            &["detail"][..],
            &["error"][..],
        ] {
            let mut node = &value;
            for key in path {
                let Some(next) = node.get(key) else {
                    node = &serde_json::Value::Null;
                    break;
                };
                node = next;
            }
            if let Some(text) = node.as_str().filter(|t| !t.is_empty()) {
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

/// Fails with [`TtsError::NotConfigured`] when a required credential is blank.
pub fn require<'a>(value: &'a str, what: &str) -> Result<&'a str, TtsError> {
    if value.trim().is_empty() {
        Err(TtsError::NotConfigured(format!(
            "{what} is not set. Enter it in Preferences, Speech."
        )))
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_bodies_are_reduced_to_their_message() {
        assert_eq!(
            summarize(r#"{"error":{"message":"Invalid API key","type":"auth"}}"#),
            "Invalid API key"
        );
        assert_eq!(
            summarize(r#"{"detail":"Voice not found"}"#),
            "Voice not found"
        );
        assert_eq!(
            summarize(r#"{"message":"quota exceeded"}"#),
            "quota exceeded"
        );
        // A plain string error object, not an object with a message.
        assert_eq!(summarize(r#"{"error":"bad request"}"#), "bad request");
    }

    #[test]
    fn non_json_bodies_pass_through_trimmed_and_clipped() {
        assert_eq!(summarize("  Forbidden\n"), "Forbidden");
        assert_eq!(summarize(""), "no detail given");
        let long = "x".repeat(500);
        let summary = summarize(&long);
        assert_eq!(summary.chars().count(), 201, "clipped plus the ellipsis");
    }

    #[test]
    fn require_names_the_missing_setting() {
        let error = require("   ", "The OpenAI API key").unwrap_err();
        assert!(
            error.to_string().contains("OpenAI API key")
                && error.to_string().contains("Preferences"),
            "{error}"
        );
        assert_eq!(require("sk-abc", "key").unwrap(), "sk-abc");
    }
}
