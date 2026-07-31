//! Icecast source-protocol client.
//!
//! Icecast source connections use plain TCP:
//! `PUT http://<host>:<port>/<mount>` with Basic auth,
//! `Expect: 100-continue`, then an endless body of encoded audio frames.

use crate::secret::Secret;
use std::io::ErrorKind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

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
}

#[derive(Debug, thiserror::Error)]
pub enum IcecastError {
    #[error("connection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("server rejected the stream: {0}")]
    Rejected(String),
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
        let mut stream = TcpStream::connect(&target.host).await?;
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
        stream.write_all(request.as_bytes()).await?;

        // Read the interim/final status line(s). Icecast answers with
        // `HTTP/1.1 100 Continue` then streams; on auth failure it sends 401.
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
                return Err(IcecastError::Rejected(
                    "oversized handshake response".into(),
                ));
            }
        }
        let response = String::from_utf8_lossy(&buf);
        let status_line = response.lines().next().unwrap_or_default();
        let ok = status_line.contains(" 100 ") || status_line.contains(" 200 ");
        if !ok {
            return Err(IcecastError::Rejected(status_line.to_string()));
        }
        log::info!(
            "Icecast source connected to {}/{}",
            target.host,
            target.mount
        );
        Ok(Self { stream })
    }

    /// Sends a block of encoded audio.
    pub async fn send(&mut self, data: &[u8]) -> Result<(), IcecastError> {
        self.stream.write_all(data).await?;
        Ok(())
    }

    /// Cleanly shuts down the source connection.
    pub async fn close(mut self) {
        let _ = self.stream.shutdown().await;
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
            Err(IcecastError::Rejected(line)) => assert!(line.contains("401")),
            other => panic!("expected rejection, got {other:?}"),
        }
    }
}
