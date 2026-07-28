//! Networking: the Audio Pub web API, the live-events SSE consumer, and the
//! Icecast source connection. Everything here runs on a tokio runtime living
//! on a background thread; the UI talks to it over channels.

pub mod audiopub;
pub mod icecast;
pub mod sse;

use audiopub::{AudioPubClient, StreamIdentity};
use icecast::{IcecastConnection, IcecastTarget};
use sse::{LiveEvent, SseParser};
use tokio::sync::mpsc as tokio_mpsc;

/// Commands from the UI to the network runtime.
pub enum NetCommand {
    Connect {
        site_url: String,
        email: String,
        password: String,
    },
    Disconnect,
    StartStream {
        title: String,
        description: String,
        archive: bool,
        content_type: String,
        /// Encoded audio produced by the audio engine.
        audio: tokio_mpsc::Receiver<Vec<u8>>,
    },
    StopStream,
    SendChat(String),
    Shutdown,
}

/// Events from the network runtime to the UI (polled on the UI pump timer).
#[derive(Debug)]
pub enum NetEvent {
    Connected { site_url: String },
    ConnectFailed { message: String },
    Disconnected,
    StreamStarted { stream_id: String },
    StreamEnded,
    StreamError { message: String },
    Chat(sse::ChatMessage),
    Listeners { active: u32, peak: u32 },
    ChatSent,
    ChatSendFailed { message: String },
}

/// The network thread's end of the event channel.
///
/// Sending also wakes the UI's idle loop, which is what drains these. Doing it
/// here rather than at each call site means a new event can never be added
/// without the doorbell — and a missed doorbell is a message that sits unread
/// until something else happens to wake the loop.
///
/// `wxWakeUpIdle` is thread-safe and carries nothing; the events themselves
/// travel by channel. (It has to be a doorbell: wxdragon's `call_after` needs
/// `Send`, and `App` is an `Rc` of `RefCell`s.)
#[derive(Clone)]
pub struct EventSender(crossbeam_channel::Sender<NetEvent>);

impl EventSender {
    pub fn send(&self, event: NetEvent) -> Result<(), crossbeam_channel::SendError<NetEvent>> {
        let result = self.0.send(event);
        wxdragon::wake_up_idle();
        result
    }
}

pub struct NetHandle {
    commands: tokio_mpsc::UnboundedSender<NetCommand>,
    pub events: crossbeam_channel::Receiver<NetEvent>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NetHandle {
    pub fn start() -> Self {
        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("net".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("building tokio runtime");
                runtime.block_on(net_loop(cmd_rx, EventSender(event_tx)));
            })
            .expect("spawning net thread");
        Self {
            commands: cmd_tx,
            events: event_rx,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: NetCommand) {
        let _ = self.commands.send(command);
    }
}

impl Drop for NetHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(NetCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Connection {
    client: AudioPubClient,
    identity: StreamIdentity,
    site_url: String,
}

struct ActiveStream {
    stream_id: String,
    sse_task: tokio::task::JoinHandle<()>,
    icecast_task: tokio::task::JoinHandle<()>,
}

impl ActiveStream {
    fn abort(&self) {
        self.sse_task.abort();
        self.icecast_task.abort();
    }
}

/// Derives the Icecast host from a site URL: Audio Pub convention is the
/// `live.` subdomain on port 8000 (matches audiopub.site's published
/// connection details).
fn icecast_host_for(site_url: &str) -> String {
    let host = site_url
        .trim_end_matches('/')
        .rsplit("//")
        .next()
        .unwrap_or(site_url);
    format!("live.{host}:8000")
}

async fn net_loop(mut commands: tokio_mpsc::UnboundedReceiver<NetCommand>, events: EventSender) {
    let mut connection: Option<Connection> = None;
    let mut stream: Option<ActiveStream> = None;

    while let Some(command) = commands.recv().await {
        match command {
            NetCommand::Connect {
                site_url,
                email,
                password,
            } => {
                let client = match AudioPubClient::new(&site_url) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = events.send(NetEvent::ConnectFailed {
                            message: e.to_string(),
                        });
                        continue;
                    }
                };
                match client.login(&email, &password).await {
                    Ok(()) => match client.stream_identity().await {
                        Ok(identity) => {
                            connection = Some(Connection {
                                client,
                                identity,
                                site_url: site_url.clone(),
                            });
                            let _ = events.send(NetEvent::Connected { site_url });
                        }
                        Err(e) => {
                            let _ = events.send(NetEvent::ConnectFailed {
                                message: format!("logged in, but no stream access: {e}"),
                            });
                        }
                    },
                    Err(e) => {
                        let _ = events.send(NetEvent::ConnectFailed {
                            message: e.to_string(),
                        });
                    }
                }
            }
            NetCommand::Disconnect => {
                if let Some(active) = stream.take() {
                    active.abort();
                    if let Some(conn) = &connection {
                        let _ = conn.client.end_stream(&active.stream_id).await;
                    }
                }
                connection = None;
                let _ = events.send(NetEvent::Disconnected);
            }
            NetCommand::StartStream {
                title,
                description,
                archive,
                content_type,
                audio,
            } => {
                let Some(conn) = &connection else {
                    let _ = events.send(NetEvent::StreamError {
                        message: "not connected to a site".into(),
                    });
                    continue;
                };
                match start_stream(
                    conn,
                    &title,
                    &description,
                    archive,
                    &content_type,
                    audio,
                    events.clone(),
                )
                .await
                {
                    Ok(active) => {
                        let _ = events.send(NetEvent::StreamStarted {
                            stream_id: active.stream_id.clone(),
                        });
                        stream = Some(active);
                    }
                    Err(message) => {
                        let _ = events.send(NetEvent::StreamError { message });
                    }
                }
            }
            NetCommand::StopStream => {
                if let Some(active) = stream.take() {
                    active.abort();
                    if let Some(conn) = &connection {
                        if let Err(e) = conn.client.end_stream(&active.stream_id).await {
                            log::warn!("Ending stream reported: {e}");
                        }
                    }
                }
                let _ = events.send(NetEvent::StreamEnded);
            }
            NetCommand::SendChat(content) => {
                let (Some(conn), Some(active)) = (&connection, &stream) else {
                    let _ = events.send(NetEvent::ChatSendFailed {
                        message: "not streaming".into(),
                    });
                    continue;
                };
                match conn.client.send_chat(&active.stream_id, &content).await {
                    Ok(_) => {
                        let _ = events.send(NetEvent::ChatSent);
                    }
                    Err(e) => {
                        let _ = events.send(NetEvent::ChatSendFailed {
                            message: e.to_string(),
                        });
                    }
                }
            }
            NetCommand::Shutdown => {
                if let Some(active) = stream.take() {
                    active.abort();
                    if let Some(conn) = &connection {
                        let _ = conn.client.end_stream(&active.stream_id).await;
                    }
                }
                return;
            }
        }
    }
}

async fn start_stream(
    conn: &Connection,
    title: &str,
    description: &str,
    archive: bool,
    content_type: &str,
    mut audio: tokio_mpsc::Receiver<Vec<u8>>,
    events: EventSender,
) -> Result<ActiveStream, String> {
    let stream_id = conn
        .client
        .create_stream(title, description, archive)
        .await
        .map_err(|e| e.to_string())?;

    // Icecast source connection carrying the encoded audio.
    let target = IcecastTarget {
        host: icecast_host_for(&conn.site_url),
        mount: conn.identity.user_id.clone(),
        password: conn.identity.stream_key.clone(),
        content_type: content_type.to_string(),
    };
    let mut icecast = IcecastConnection::connect(&target)
        .await
        .map_err(|e| e.to_string())?;

    let icecast_events = events.clone();
    let icecast_task = tokio::spawn(async move {
        while let Some(chunk) = audio.recv().await {
            if let Err(e) = icecast.send(&chunk).await {
                let _ = icecast_events.send(NetEvent::StreamError {
                    message: format!("stream connection lost: {e}"),
                });
                return;
            }
        }
        icecast.close().await;
    });

    // Live events (chat, listeners, state).
    let response = conn
        .client
        .open_events(&stream_id)
        .await
        .map_err(|e| e.to_string())?;
    let sse_events = events.clone();
    let sse_task = tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut parser = SseParser::new();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let Ok(chunk) = chunk else { break };
            for raw in parser.feed(&chunk) {
                match LiveEvent::from_sse(&raw) {
                    Some(LiveEvent::Chat(message)) => {
                        let _ = sse_events.send(NetEvent::Chat(message));
                    }
                    Some(LiveEvent::Listeners { active, peak }) => {
                        // We are one of the listeners (this SSE connection);
                        // do not count ourselves.
                        let _ = sse_events.send(NetEvent::Listeners {
                            active: active.saturating_sub(1),
                            peak: peak.saturating_sub(1),
                        });
                    }
                    Some(LiveEvent::Finish) => {
                        let _ = sse_events.send(NetEvent::StreamEnded);
                        return;
                    }
                    _ => {}
                }
            }
        }
    });

    Ok(ActiveStream {
        stream_id,
        sse_task,
        icecast_task,
    })
}

#[cfg(test)]
mod host_tests {
    #[test]
    fn icecast_host_derivation() {
        assert_eq!(
            super::icecast_host_for("https://audiopub.site/"),
            "live.audiopub.site:8000"
        );
        assert_eq!(
            super::icecast_host_for("http://example.org"),
            "live.example.org:8000"
        );
    }
}
