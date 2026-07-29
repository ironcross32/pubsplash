//! Networking: the Audiopub web API, the live-events SSE consumer, and the
//! Icecast source connection. Everything here runs on a tokio runtime living
//! on a background thread; the UI talks to it over channels.

pub mod audiopub;
pub mod icecast;
pub mod sse;

use audiopub::{AudioPubClient, StreamIdentity};
use icecast::{IcecastConnection, IcecastTarget};
use sse::{LiveEvent, SseParser};
use tokio::sync::mpsc as tokio_mpsc;

/// A configured streaming service, snapshotted by the UI before it is sent to
/// the network thread.
#[derive(Debug, Clone)]
pub enum ServiceProfile {
    Audiopub {
        id: String,
        nickname: String,
        site_url: String,
        email: String,
        password: String,
    },
    Icecast {
        id: String,
        nickname: String,
        server: String,
        port: u16,
        mount: String,
        username: String,
        password: String,
    },
}

/// Commands from the UI to the network runtime.
pub enum NetCommand {
    Connect {
        profile: ServiceProfile,
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
    Connected {
        service_id: String,
        display_name: String,
    },
    ConnectFailed {
        message: String,
    },
    Disconnected,
    StreamStarted {
        stream_id: String,
    },
    StreamEnded,
    StreamError {
        message: String,
    },
    Chat(sse::ChatMessage),
    Listeners {
        active: u32,
        peak: u32,
    },
    ChatSent,
    ChatSendFailed {
        message: String,
    },
}

/// The network thread's end of the event channel.
///
/// Sending also wakes the UI's idle loop, which is what drains these. Doing it
/// here rather than at each call site means a new event can never be added
/// without the doorbell - and a missed doorbell is a message that sits unread
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

enum Connection {
    Audiopub {
        client: AudioPubClient,
        identity: StreamIdentity,
        site_url: String,
    },
    Icecast {
        server: String,
        port: u16,
        mount: String,
        username: String,
        password: String,
    },
}

fn audiopub_client(connection: &Connection) -> Option<&AudioPubClient> {
    match connection {
        Connection::Audiopub { client, .. } => Some(client),
        Connection::Icecast { .. } => None,
    }
}

struct ActiveStream {
    stream_id: String,
    sse_task: Option<tokio::task::JoinHandle<()>>,
    icecast_task: tokio::task::JoinHandle<()>,
}

impl ActiveStream {
    fn abort(&self) {
        if let Some(task) = &self.sse_task {
            task.abort();
        }
        self.icecast_task.abort();
    }
}

/// Derives the Icecast host from a site URL: Audiopub convention is the
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

fn normalize_mount(mount: &str) -> String {
    mount.trim().trim_start_matches('/').to_string()
}

fn audiopub_target_for(
    site_url: &str,
    identity: &StreamIdentity,
    content_type: &str,
) -> IcecastTarget {
    IcecastTarget {
        host: icecast_host_for(site_url),
        mount: identity.user_id.clone(),
        username: "source".to_string(),
        password: identity.stream_key.clone(),
        content_type: content_type.to_string(),
    }
}

fn direct_icecast_target(
    profile: &Connection,
    content_type: &str,
) -> Result<IcecastTarget, String> {
    let Connection::Icecast {
        server,
        port,
        mount,
        username,
        password,
        ..
    } = profile
    else {
        return Err("not an Icecast service".to_string());
    };
    let server = server.trim();
    let mount = normalize_mount(mount);
    if server.is_empty() {
        return Err("Enter the Icecast server.".to_string());
    }
    if *port == 0 {
        return Err("Enter a valid Icecast port.".to_string());
    }
    if mount.is_empty() {
        return Err("Enter the Icecast mount point.".to_string());
    }
    if password.is_empty() {
        return Err("Enter the Icecast password.".to_string());
    }
    let username = if username.trim().is_empty() {
        "source".to_string()
    } else {
        username.trim().to_string()
    };
    Ok(IcecastTarget {
        host: format!("{server}:{port}"),
        mount,
        username,
        password: password.clone(),
        content_type: content_type.to_string(),
    })
}

async fn end_active_stream(active: &ActiveStream, connection: Option<&Connection>) {
    active.abort();
    if let Some(client) = connection.and_then(audiopub_client) {
        if let Err(e) = client.end_stream(&active.stream_id).await {
            log::warn!("Ending stream reported: {e}");
        }
    }
}

async fn net_loop(mut commands: tokio_mpsc::UnboundedReceiver<NetCommand>, events: EventSender) {
    let mut connection: Option<Connection> = None;
    let mut stream: Option<ActiveStream> = None;

    while let Some(command) = commands.recv().await {
        match command {
            NetCommand::Connect { profile } => match profile {
                ServiceProfile::Audiopub {
                    id,
                    nickname,
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
                                let display_name = nickname;
                                connection = Some(Connection::Audiopub {
                                    client,
                                    identity,
                                    site_url,
                                });
                                let _ = events.send(NetEvent::Connected {
                                    service_id: id,
                                    display_name,
                                });
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
                ServiceProfile::Icecast {
                    id,
                    nickname,
                    server,
                    port,
                    mount,
                    username,
                    password,
                } => {
                    let armed = Connection::Icecast {
                        server,
                        port,
                        mount,
                        username,
                        password,
                    };
                    match direct_icecast_target(&armed, "audio/mpeg") {
                        Ok(_) => {
                            connection = Some(armed);
                            let _ = events.send(NetEvent::Connected {
                                service_id: id,
                                display_name: nickname,
                            });
                        }
                        Err(message) => {
                            let _ = events.send(NetEvent::ConnectFailed { message });
                        }
                    }
                }
            },
            NetCommand::Disconnect => {
                if let Some(active) = stream.take() {
                    end_active_stream(&active, connection.as_ref()).await;
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
                        message: "not connected to a streaming service".into(),
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
                    end_active_stream(&active, connection.as_ref()).await;
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
                let Some(client) = audiopub_client(conn) else {
                    let _ = events.send(NetEvent::ChatSendFailed {
                        message: "chat is only available for Audiopub services".into(),
                    });
                    continue;
                };
                match client.send_chat(&active.stream_id, &content).await {
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
                    end_active_stream(&active, connection.as_ref()).await;
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
    match conn {
        Connection::Audiopub {
            client,
            identity,
            site_url,
            ..
        } => {
            let stream_id = client
                .create_stream(title, description, archive)
                .await
                .map_err(|e| e.to_string())?;

            let target = audiopub_target_for(site_url, identity, content_type);
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

            let response = client
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
                sse_task: Some(sse_task),
                icecast_task,
            })
        }
        Connection::Icecast { mount, .. } => {
            let target = direct_icecast_target(conn, content_type)?;
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

            Ok(ActiveStream {
                stream_id: format!("icecast:{}", normalize_mount(mount)),
                sse_task: None,
                icecast_task,
            })
        }
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;

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

    #[test]
    fn audiopub_target_uses_site_identity() {
        let identity = StreamIdentity {
            user_id: "user-123".to_string(),
            stream_key: "stream-key".to_string(),
        };
        let target = audiopub_target_for("https://example.org/", &identity, "audio/mpeg");
        assert_eq!(target.host, "live.example.org:8000");
        assert_eq!(target.mount, "user-123");
        assert_eq!(target.username, "source");
        assert_eq!(target.password, "stream-key");
        assert_eq!(target.content_type, "audio/mpeg");
    }

    #[test]
    fn direct_icecast_target_uses_profile_fields() {
        let conn = Connection::Icecast {
            server: "ice.example.org".to_string(),
            port: 9000,
            mount: "/live".to_string(),
            username: "dj".to_string(),
            password: "secret".to_string(),
        };
        let target = direct_icecast_target(&conn, "audio/aac").unwrap();
        assert_eq!(target.host, "ice.example.org:9000");
        assert_eq!(target.mount, "live");
        assert_eq!(target.username, "dj");
        assert_eq!(target.password, "secret");
        assert_eq!(target.content_type, "audio/aac");
    }

    #[test]
    fn direct_icecast_target_defaults_blank_username() {
        let conn = Connection::Icecast {
            server: "ice.example.org".to_string(),
            port: 8000,
            mount: "live".to_string(),
            username: String::new(),
            password: "secret".to_string(),
        };
        let target = direct_icecast_target(&conn, "audio/mpeg").unwrap();
        assert_eq!(target.username, "source");
    }
}
