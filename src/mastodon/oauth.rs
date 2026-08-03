//! The OAuth authorization-code flow, with PKCE, run against one Mastodon
//! server.
//!
//! The user should not have to copy anything. So the normal path binds a
//! loopback listener on a port the OS picks, registers the app with
//! `http://127.0.0.1:{port}/` as its redirect URI, and reads the code straight
//! out of the browser's redirect. Mastodon ships `force_ssl_in_redirect_uri
//! false`, so a plain-http loopback URI is accepted.
//!
//! If the listener cannot bind — a firewall policy, a locked-down machine — the
//! flow falls back to Mastodon's out-of-band redirect, where the browser shows a
//! code and the UI asks the user to paste it. That path is slower and needs
//! copy-and-paste, which is exactly why it is the fallback and not the default.
//!
//! PKCE (`S256`, the only method Mastodon supports, added in 4.3.0) and a random
//! `state` are used on both paths. `state` is checked before the code is
//! touched: without it, anything that can reach the loopback port could feed us
//! a code.

use super::api::{self, AppCredentials, Link, MastodonError};
use super::net::{self, AuthEvent};
use crossbeam_channel::Sender;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Mastodon's out-of-band redirect: the browser displays the code instead of
/// redirecting anywhere.
const OOB: &str = "urn:ietf:wg:oauth:2.0:oob";

/// How long to wait for the browser to come back before giving up. Long enough
/// to find a password and work through a consent screen with a screen reader.
const WAIT: Duration = Duration::from_secs(180);
/// How often the accept loop checks the cancel flag and the deadline.
const POLL: Duration = Duration::from_millis(150);

/// Shared with the waiting dialog's Cancel button.
pub type Cancel = Arc<AtomicBool>;

pub fn cancel_flag() -> Cancel {
    Arc::new(AtomicBool::new(false))
}

/// Runs the whole flow. Called on the `mastodon-auth` thread by
/// [`net::authorize`].
pub fn run(
    instance: &str,
    reply: &Sender<AuthEvent>,
    cancel: &Cancel,
) -> Result<Link, MastodonError> {
    let instance = api::normalize_instance(instance)?;
    // Ask the host whether it speaks the Mastodon API before anything is spent
    // on it. A wrong address costs no port, no app registration and no browser
    // window, and the user is told what is wrong instead of being read the
    // host's own 404 page.
    net::block_on(api::check_instance(&instance))?;
    let verifier = random_token();
    let challenge = challenge_for(&verifier);
    let state = random_token();

    // Bind before registering: the redirect URI has to carry the real port, and
    // the port is not ours until the listener exists.
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => match listener.local_addr() {
            Ok(addr) => Some((listener, addr.port())),
            Err(e) => {
                log::warn!("Could not read the loopback port: {e}");
                None
            }
        },
        Err(e) => {
            log::warn!("Could not bind a loopback listener for Mastodon authorization: {e}");
            None
        }
    };

    let redirect_uri = match &listener {
        Some((_, port)) => format!("http://127.0.0.1:{port}/"),
        None => OOB.to_string(),
    };
    let app = net::block_on(api::register_app(&instance, &redirect_uri))?;
    let url = api::authorize_url(&instance, &app.client_id, &redirect_uri, &state, &challenge);
    if reply.send(AuthEvent::Open(url)).is_err() {
        return Err(MastodonError::Cancelled);
    }

    let code = match listener {
        Some((listener, _)) => wait_for_redirect(&listener, &state, cancel)?,
        None => ask_for_code(reply)?,
    };
    finish(&instance, &app, &redirect_uri, &code, &verifier)
}

fn finish(
    instance: &str,
    app: &AppCredentials,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<Link, MastodonError> {
    net::block_on(async {
        let token = api::exchange_code(instance, app, redirect_uri, code, verifier).await?;
        let account = api::verify_credentials(instance, &token).await?;
        Ok(Link {
            instance: instance.to_string(),
            client_id: app.client_id.clone(),
            client_secret: app.client_secret.clone(),
            access_token: token,
            account,
        })
    })
}

/// Waits for the browser to hit the loopback port and hands back the code.
fn wait_for_redirect(
    listener: &TcpListener,
    state: &str,
    cancel: &Cancel,
) -> Result<String, MastodonError> {
    listener
        .set_nonblocking(true)
        .map_err(|e| MastodonError::Network(e.to_string()))?;
    let deadline = Instant::now() + WAIT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(MastodonError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(MastodonError::Network(
                "the browser did not come back within three minutes".into(),
            ));
        }
        match listener.accept() {
            Ok((stream, _)) => match read_redirect(stream, state) {
                // A stray request on the port (a probe, a favicon fetch) is not
                // the redirect; keep waiting rather than failing the whole flow.
                Ok(None) => continue,
                Ok(Some(code)) => return Ok(code),
                Err(error) => return Err(error),
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(e) => return Err(MastodonError::Network(e.to_string())),
        }
    }
}

/// Reads one request, answers it, and returns the code if this was the redirect.
fn read_redirect(mut stream: TcpStream, state: &str) -> Result<Option<String>, MastodonError> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut buffer = [0u8; 4096];
    let read = stream.read(&mut buffer).unwrap_or(0);
    let request = String::from_utf8_lossy(&buffer[..read]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("")
        .to_string();
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let params = parse_query(query);

    if let Some(error) = params.iter().find(|(k, _)| k == "error") {
        answer(
            &mut stream,
            "Authorization was refused. You can close this tab.",
        );
        let detail = params
            .iter()
            .find(|(k, _)| k == "error_description")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| error.1.clone());
        return Err(MastodonError::Service {
            status: 400,
            // Through the same reducer as a response body: this arrives in a
            // query string the server controls, so it is neither short nor
            // trustworthy by default.
            detail: api::summarize(&detail),
        });
    }
    let Some((_, code)) = params.iter().find(|(k, _)| k == "code") else {
        // Not the redirect. Say nothing useful and keep listening.
        answer(&mut stream, "Waiting for Mastodon.");
        return Ok(None);
    };
    // Checked before the code is used, not after.
    let returned_state = params
        .iter()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    if returned_state != state {
        answer(
            &mut stream,
            "That did not come from Pubsplash. Nothing was linked.",
        );
        return Err(MastodonError::Malformed(
            "a matching state value, so the reply was not trusted".into(),
        ));
    }
    answer(
        &mut stream,
        "Pubsplash is linked to your Mastodon account. You can close this tab and go back to Pubsplash.",
    );
    Ok(Some(code.clone()))
}

fn answer(stream: &mut TcpStream, message: &str) {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>Pubsplash</title></head><body><h1>Pubsplash</h1><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// The fallback: ask the UI to collect a pasted code.
fn ask_for_code(reply: &Sender<AuthEvent>) -> Result<String, MastodonError> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    if reply.send(AuthEvent::NeedCode(tx)).is_err() {
        return Err(MastodonError::Cancelled);
    }
    match rx.recv() {
        Ok(Some(code)) if !code.trim().is_empty() => Ok(code.trim().to_string()),
        _ => Err(MastodonError::Cancelled),
    }
}

/// `a=1&b=2` into pairs, percent-decoded.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
        .collect()
}

fn decode(text: &str) -> String {
    urlencoding::decode(&text.replace('+', " "))
        .map(|cow| cow.into_owned())
        .unwrap_or_else(|_| text.to_string())
}

/// 32 random bytes as base64url. Used for both the PKCE verifier and `state`.
fn random_token() -> String {
    use rand::Rng;
    let bytes: [u8; 32] = rand::thread_rng().r#gen();
    base64url(&bytes)
}

/// The PKCE `S256` challenge for a verifier.
fn challenge_for(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// Base64 with the URL-safe alphabet and no padding, which is what RFC 7636
/// requires. `b64::encode` is the standard alphabet, so `+`, `/` and `=` are
/// translated here rather than adding a second encoder to `b64`.
fn base64url(bytes: &[u8]) -> String {
    crate::b64::encode(bytes)
        .replace('+', "-")
        .replace('/', "_")
        .replace('=', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_split_and_decoded() {
        let params = parse_query("code=abc%2Fdef&state=x+y");
        assert_eq!(params[0], ("code".into(), "abc/def".into()));
        assert_eq!(params[1], ("state".into(), "x y".into()));
        assert!(parse_query("").is_empty());
    }

    #[test]
    fn tokens_are_url_safe_unpadded_and_unique() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b, "two tokens in a row must not collide");
        for token in [&a, &b] {
            assert!(!token.is_empty());
            assert!(
                token
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
                "{token} is not url-safe"
            );
        }
    }

    /// The RFC 7636 appendix B vector, which pins both the hash and the
    /// url-safe unpadded encoding.
    #[test]
    fn the_pkce_challenge_matches_the_rfc_vector() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn base64url_drops_padding_and_swaps_the_alphabet() {
        // 0xfb 0xff is "+/8=" in the standard alphabet.
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(base64url(b""), "");
    }

    /// A request without a `code` is somebody else knocking, not a failure.
    #[test]
    fn a_redirect_without_a_code_is_ignored_rather_than_fatal() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n")
                .unwrap();
            let mut out = String::new();
            let _ = stream.read_to_string(&mut out);
            out
        });
        let (stream, _) = listener.accept().unwrap();
        assert_eq!(read_redirect(stream, "expected").unwrap(), None);
        assert!(probe.join().unwrap().starts_with("HTTP/1.1 200"));
    }

    /// A code arriving with the wrong `state` must not be exchanged.
    #[test]
    fn a_mismatched_state_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"GET /?code=stolen&state=wrong HTTP/1.1\r\n\r\n")
                .unwrap();
            let mut out = String::new();
            let _ = stream.read_to_string(&mut out);
        });
        let (stream, _) = listener.accept().unwrap();
        let error = read_redirect(stream, "expected").unwrap_err();
        assert!(matches!(error, MastodonError::Malformed(_)), "{error:?}");
        probe.join().unwrap();
    }

    #[test]
    fn a_good_redirect_yields_the_code() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"GET /?code=good%2Dcode&state=expected HTTP/1.1\r\n\r\n")
                .unwrap();
            let mut out = String::new();
            let _ = stream.read_to_string(&mut out);
            out
        });
        let (stream, _) = listener.accept().unwrap();
        assert_eq!(
            read_redirect(stream, "expected").unwrap().as_deref(),
            Some("good-code")
        );
        assert!(probe.join().unwrap().contains("You can close this tab"));
    }

    #[test]
    fn waiting_stops_when_cancelled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let cancel = cancel_flag();
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            wait_for_redirect(&listener, "s", &cancel),
            Err(MastodonError::Cancelled)
        );
    }
}
