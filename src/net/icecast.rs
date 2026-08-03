//! Icecast source-protocol client.
//!
//! Icecast source connections use plain TCP:
//! `PUT http://<host>:<port>/<mount>` with Basic auth,
//! `Expect: 100-continue`, then an endless body of encoded audio frames.

use crate::secret::Secret;
use std::io::ErrorKind;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Bounds a connect, so a black-holed route cannot park the caller. Matches
/// `mastodon::net::CONNECT_TIMEOUT` and `tts::net::CONNECT_TIMEOUT`.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds the handshake round-trip. More generous than the connect because
/// Icecast's source auth is not a local decision: its auth hook calls the Audio
/// Pub web app, which hits the database to check the stream key and the stream
/// row. Fifteen seconds covers a loaded site without letting a wedged one hold
/// up the network thread's command loop.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Bounds one `write_all` of encoded audio — the fast stall detector, and the
/// difference between noticing an outage in seconds and noticing it in minutes.
///
/// Without it a blip does not error at all: `write_all` fills the kernel send
/// buffer and then pends through Windows' TCP retransmit budget (tens of
/// seconds to ~2 minutes) while the UI reports a perfectly healthy broadcast.
///
/// Five seconds, because at the default 128 kbps the encoder emits ~16 KB/s and
/// Windows' auto-tuned send buffer is typically 64 KB — so a socket that stops
/// draining accepts ~4 s of audio before `write_all` pends at all. A write still
/// outstanding 5 s later means roughly nine seconds of audio has not moved,
/// which is far outside congestion jitter. It is also already past the point of
/// user harm: the engine's outgoing channel holds ~2 s, so the mixer has been
/// dropping audio for ~3 s and listeners have heard a gap. Detecting later buys
/// nothing; detecting sooner would tear down a merely congested socket.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Bounds the shutdown handshake. This one is on the app-exit path, where
/// `NetHandle::drop` joins the network thread, so a half-open socket here holds
/// up the whole teardown.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct IcecastTarget {
    /// Host and port, e.g. `live.audiopub.site:8000`.
    pub host: String,
    /// Mount without leading slash (the Audio Pub user id).
    pub mount: String,
    /// Source username, usually `source`.
    pub username: String,
    /// Source password or stream key.
    pub password: Secret,
    /// e.g. `audio/mpeg`.
    pub content_type: String,
}

#[derive(Debug)]
pub struct IcecastConnection {
    stream: TcpStream,
    /// Set by any failed [`IcecastConnection::send`].
    ///
    /// A `write_all` cancelled by its timeout may have written a *partial* MP3
    /// frame, so the connection is unusable afterwards: the next write would
    /// splice the tail of one frame onto the head of another and hand every
    /// listener's decoder garbage. The sender task always reconnects after a
    /// failure, and this makes "always" structural rather than a convention.
    poisoned: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum IcecastError {
    #[error("connection failed: {0}")]
    Io(#[from] std::io::Error),
    /// Timed out doing `what` — "connecting to the streaming server",
    /// "waiting for the server to accept the stream", "sending audio".
    #[error("timed out {what}")]
    Timeout { what: &'static str },
    /// The server answered the handshake with a status we cannot stream over.
    #[error("server rejected the stream: {status_line}{}",
        .message.as_deref().map(|m| format!(" ({m})")).unwrap_or_default())]
    Rejected {
        status: u16,
        /// The full status line, e.g. `HTTP/1.0 403 Mountpoint in use`.
        status_line: String,
        /// The `icecast-auth-message` header, when there is one. This is where
        /// Audio Pub says *why*: `Invalid stream key`, `User is banned`,
        /// `User is not trusted`, `No active stream found`.
        message: Option<String>,
    },
}

impl IcecastError {
    /// Whether reconnecting could plausibly succeed.
    ///
    /// The interesting case is 403, which Icecast uses for three unrelated
    /// things of which only one is temporary. `Mountpoint in use` is our *own*
    /// previous socket: stock Icecast holds a mount for `<source-timeout>`
    /// (default 10 s) after a source dies, so it is the *expected* answer to a
    /// fast reconnect and must be retried rather than reported as fatal.
    pub fn retryable(&self) -> bool {
        match self {
            IcecastError::Io(_) | IcecastError::Timeout { .. } => true,
            IcecastError::Rejected {
                status,
                status_line,
                ..
            } => match status {
                // Every 401 is terminal: a bad or revoked stream key, a banned
                // or untrusted user, or a stream row the server has already
                // expired. None of those change by trying again.
                401 => false,
                403 => {
                    let reason = status_line.to_ascii_lowercase();
                    reason.contains("in use") || reason.contains("too many sources")
                }
                // A restarting or briefly overloaded server.
                500..=599 => true,
                _ => false,
            },
        }
    }

    /// One clause for the log line, in the user's terms rather than the
    /// protocol's. Reads as the tail of "Audio connection lost (...)".
    pub fn explain(&self) -> String {
        match self {
            IcecastError::Timeout { .. } => "the connection timed out".to_string(),
            IcecastError::Io(e) => match e.kind() {
                ErrorKind::ConnectionRefused => "the server refused the connection".to_string(),
                ErrorKind::TimedOut => "the connection timed out".to_string(),
                _ => "the network dropped the connection".to_string(),
            },
            // The server's own words when it gave any, since they are far more
            // use than a status code: "No active stream found" tells the user
            // their stream expired, which nothing else here can say.
            IcecastError::Rejected {
                message: Some(m), ..
            } => format!("the server said: {m}"),
            IcecastError::Rejected { status: 403, .. } => "the server is busy".to_string(),
            IcecastError::Rejected { status_line, .. } => {
                format!("the server refused the stream: {status_line}")
            }
        }
    }
}

/// Case-insensitive header lookup over a raw HTTP response head.
fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .find(|(field, _)| field.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().to_string())
}

/// Interprets an Icecast handshake response head.
///
/// Pure, so every status the server can answer with is testable without a
/// socket — which matters because the retryable/terminal split below is what
/// decides whether a dropped broadcast comes back.
fn parse_handshake(head: &str) -> Result<(), IcecastError> {
    let status_line = head.lines().next().unwrap_or_default().trim();
    // Icecast answers `100 Continue` and then streams; some builds answer 200.
    if status_line.contains(" 100 ") || status_line.contains(" 200 ") {
        return Ok(());
    }
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    Err(IcecastError::Rejected {
        status,
        status_line: status_line.to_string(),
        message: header_value(head, "icecast-auth-message"),
    })
}

/// Reads an HTTP response head (up to the blank line) one byte at a time.
///
/// Byte-at-a-time because the body that follows is ours to write, not to read:
/// over-reading here would consume nothing useful and complicate nothing well.
async fn read_head(stream: &mut TcpStream) -> Result<String, IcecastError> {
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Err(IcecastError::Io(std::io::Error::new(
                ErrorKind::UnexpectedEof,
                "server closed connection during handshake",
            )));
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 16 * 1024 {
            return Err(IcecastError::Rejected {
                status: 0,
                status_line: "oversized handshake response".into(),
                message: None,
            });
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// Standard-library base64 does not exist; this avoids pulling in a crate for
// one header. RFC 4648, no padding edge cases beyond the usual.
fn authorization_header(username: &str, password: &str) -> String {
    base64(format!("{username}:{password}").as_bytes())
}

fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        let chars = [
            ALPHABET[(n >> 18 & 63) as usize],
            ALPHABET[(n >> 12 & 63) as usize],
            ALPHABET[(n >> 6 & 63) as usize],
            ALPHABET[(n & 63) as usize],
        ];
        let keep = chunk.len() + 1;
        for (i, c) in chars.into_iter().enumerate() {
            out.push(if i < keep { c as char } else { '=' });
        }
    }
    out
}

impl IcecastConnection {
    /// Connects and performs the source handshake. Returns once the server
    /// has accepted the stream (HTTP 100/200).
    pub async fn connect(target: &IcecastTarget) -> Result<Self, IcecastError> {
        let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(&target.host))
            .await
            .map_err(|_| IcecastError::Timeout {
                what: "connecting to the streaming server",
            })??;
        stream.set_nodelay(true)?;

        let username = if target.username.trim().is_empty() {
            "source"
        } else {
            target.username.trim()
        };
        let auth = authorization_header(username, target.password.as_str());
        let request = format!(
            "PUT /{mount} HTTP/1.1\r\n\
             Host: {host}\r\n\
             Authorization: Basic {auth}\r\n\
             User-Agent: pubsplash/{version}\r\n\
             Content-Type: {ctype}\r\n\
             Expect: 100-continue\r\n\
             Ice-Public: 0\r\n\
             \r\n",
            mount = target.mount,
            host = target.host,
            auth = auth,
            version = env!("CARGO_PKG_VERSION"),
            ctype = target.content_type,
        );
        // One timeout around the request *and* the response, because the budget
        // the user cares about is the total time to "accepted", not either half.
        // Icecast answers `HTTP/1.1 100 Continue` and then streams; on auth
        // failure it sends 401 with an `icecast-auth-message` saying why.
        let head = timeout(HANDSHAKE_TIMEOUT, async {
            stream.write_all(request.as_bytes()).await?;
            read_head(&mut stream).await
        })
        .await
        .map_err(|_| IcecastError::Timeout {
            what: "waiting for the server to accept the stream",
        })??;

        parse_handshake(&head)?;
        log::info!(
            "Icecast source connected to {}/{}",
            target.host,
            target.mount
        );
        Ok(Self {
            stream,
            poisoned: false,
        })
    }

    /// Sends a block of encoded audio.
    ///
    /// Bounded by [`WRITE_TIMEOUT`], which is the whole of this app's fast
    /// outage detection: an unbounded `write_all` reports a dead link only once
    /// the OS gives up retransmitting, minutes later.
    pub async fn send(&mut self, data: &[u8]) -> Result<(), IcecastError> {
        if self.poisoned {
            return Err(IcecastError::Io(std::io::Error::new(
                ErrorKind::BrokenPipe,
                "connection already failed",
            )));
        }
        match timeout(WRITE_TIMEOUT, self.stream.write_all(data)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                self.poisoned = true;
                Err(e.into())
            }
            Err(_) => {
                self.poisoned = true;
                Err(IcecastError::Timeout {
                    what: "sending audio",
                })
            }
        }
    }

    /// Cleanly shuts down the source connection.
    ///
    /// Bounded, because this runs on the app-exit path: a half-open socket here
    /// would otherwise hold the network thread that `NetHandle::drop` joins.
    pub async fn close(mut self) {
        let _ = timeout(CLOSE_TIMEOUT, self.stream.shutdown()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"source:abc123"), "c291cmNlOmFiYzEyMw==");
    }

    #[test]
    fn authorization_uses_configured_username() {
        assert_eq!(authorization_header("source", "key"), "c291cmNlOmtleQ==");
        assert_eq!(authorization_header("dj", "key"), "ZGo6a2V5");
    }

    #[tokio::test]
    async fn handshake_against_fake_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            sock.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .unwrap();
            let mut audio = vec![0u8; 4];
            sock.read_exact(&mut audio).await.unwrap();
            (req, audio)
        });

        let target = IcecastTarget {
            host: addr.to_string(),
            mount: "user-123".into(),
            username: "source".into(),
            password: Secret::new("key"),
            content_type: "audio/mpeg".into(),
        };
        let mut conn = IcecastConnection::connect(&target).await.unwrap();
        conn.send(b"MP3!").await.unwrap();
        conn.close().await;

        let (req, audio) = server.await.unwrap();
        assert!(req.starts_with("PUT /user-123 HTTP/1.1\r\n"));
        assert!(req.contains("Authorization: Basic c291cmNlOmtleQ=="));
        assert!(req.contains("Content-Type: audio/mpeg"));
        assert_eq!(audio, b"MP3!");
    }

    #[tokio::test]
    async fn rejection_is_reported() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let target = IcecastTarget {
            host: addr.to_string(),
            mount: "u".into(),
            username: "source".into(),
            password: Secret::new("bad"),
            content_type: "audio/mpeg".into(),
        };
        match IcecastConnection::connect(&target).await {
            Err(e @ IcecastError::Rejected { status: 401, .. }) => {
                assert!(e.to_string().contains("401"));
                assert!(!e.retryable(), "401 must be terminal");
            }
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    #[test]
    fn handshake_accepts_continue_and_ok() {
        assert!(parse_handshake("HTTP/1.1 100 Continue\r\n\r\n").is_ok());
        assert!(parse_handshake("HTTP/1.0 200 OK\r\n\r\n").is_ok());
    }

    /// The single most load-bearing classification in the reconnect feature: a
    /// fast reconnect races our own dying socket, which Icecast holds for
    /// `<source-timeout>`. If this regresses, every reconnect fails for good.
    #[test]
    fn mountpoint_in_use_is_retryable() {
        let err = parse_handshake("HTTP/1.0 403 Mountpoint in use\r\n\r\n").unwrap_err();
        assert!(err.retryable());
    }

    #[test]
    fn other_403s_are_terminal() {
        let err = parse_handshake("HTTP/1.0 403 Content-type audio/ogg not supported\r\n\r\n")
            .unwrap_err();
        assert!(!err.retryable());
    }

    #[test]
    fn server_side_errors_are_retryable() {
        assert!(
            parse_handshake("HTTP/1.0 503 Service Unavailable\r\n\r\n")
                .unwrap_err()
                .retryable()
        );
    }

    #[test]
    fn auth_message_header_is_captured_and_explained() {
        let err = parse_handshake(
            "HTTP/1.0 401 Unauthorized\r\nIcecast-Auth-Message: No active stream found\r\n\r\n",
        )
        .unwrap_err();
        let IcecastError::Rejected {
            status,
            ref message,
            ..
        } = err
        else {
            panic!("expected a rejection, got {err:?}");
        };
        assert_eq!(status, 401);
        assert_eq!(message.as_deref(), Some("No active stream found"));
        assert!(!err.retryable());
        // The user is told why, not just that.
        assert!(err.explain().contains("No active stream found"));
        assert!(err.to_string().contains("No active stream found"));
    }

    #[test]
    fn header_lookup_is_case_insensitive_and_tolerates_absence() {
        let head = "HTTP/1.0 401 Unauthorized\r\nICECAST-AUTH-MESSAGE:  User is banned \r\n\r\n";
        assert_eq!(
            header_value(head, "icecast-auth-message").as_deref(),
            Some("User is banned")
        );
        assert_eq!(header_value(head, "content-type"), None);
        // The status line must never be mistaken for a header.
        assert_eq!(header_value(head, "HTTP/1.0 401 Unauthorized"), None);
    }

    #[test]
    fn transport_failures_are_retryable() {
        let err = IcecastError::Io(std::io::Error::new(ErrorKind::ConnectionReset, "reset"));
        assert!(err.retryable());
        assert!(
            IcecastError::Timeout {
                what: "sending audio"
            }
            .retryable()
        );
    }

    /// A blip must surface in seconds. Time is paused, so this asserts the
    /// timeout is wired up without actually waiting five seconds.
    #[tokio::test(start_paused = true)]
    async fn write_timeout_fires_when_the_peer_stops_reading() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .unwrap();
            // Never read again, and hold the socket open so there is no RST:
            // this is the half-open case a bare `write_all` cannot detect.
            std::future::pending::<()>().await;
        });

        let target = IcecastTarget {
            host: addr.to_string(),
            mount: "u".into(),
            username: "source".into(),
            password: Secret::new("key"),
            content_type: "audio/mpeg".into(),
        };
        let mut conn = IcecastConnection::connect(&target).await.unwrap();

        // Fill the send buffer until a write cannot complete.
        let block = vec![0u8; 64 * 1024];
        let err = loop {
            if let Err(e) = conn.send(&block).await {
                break e;
            }
        };
        assert!(matches!(err, IcecastError::Timeout { .. }), "got {err:?}");
        assert!(err.retryable());

        // Poisoned: a timed-out write may have left a partial MP3 frame on the
        // wire, so the connection must refuse further use rather than splice.
        assert!(conn.send(b"more").await.is_err());
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_times_out_when_the_server_says_nothing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            // Accept, then never answer.
            std::future::pending::<()>().await;
            drop(sock);
        });

        let target = IcecastTarget {
            host: addr.to_string(),
            mount: "u".into(),
            username: "source".into(),
            password: Secret::new("key"),
            content_type: "audio/mpeg".into(),
        };
        match IcecastConnection::connect(&target).await {
            Err(e @ IcecastError::Timeout { .. }) => assert!(e.retryable()),
            other => panic!("expected a timeout, got {other:?}"),
        }
        server.abort();
    }
}
