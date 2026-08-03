//! Networking: the Audiopub web API, the live-events SSE consumer, and the
//! Icecast source connection. Everything here runs on a tokio runtime living
//! on a background thread; the UI talks to it over channels.

pub mod audiopub;
pub mod icecast;
pub mod sse;

use crate::secret::Secret;
use audiopub::{AudioPubClient, EventsStream, StreamIdentity};
use icecast::{IcecastConnection, IcecastError, IcecastTarget};
use sse::{LiveEvent, SseParser};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
        password: Secret,
    },
    Icecast {
        id: String,
        nickname: String,
        server: String,
        port: u16,
        mount: String,
        username: String,
        password: Secret,
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
    /// Drop the live-events connection and open a new one, leaving the stream
    /// itself alone. The user's escape hatch when chat has gone quiet.
    ReconnectChat,
    Shutdown,
}

/// What the outgoing audio (Icecast source) connection is doing.
///
/// The sibling of [`ChatFeedState`] for the other half of a broadcast, and it
/// follows the same rule for the same reason: only *transitions* are reported,
/// so a four-minute outage puts two or three lines in the log rather than one
/// per attempt.
#[derive(Debug)]
pub enum AudioLinkState {
    /// The source connection dropped; we are reconnecting. Once per outage.
    Interrupted { reason: String },
    /// A reconnect succeeded. The same stream, not a new one.
    Restored { gap_seconds: u64 },
    /// Halfway through the retry budget and still trying, so a long outage is
    /// not silent between its opening line and its last one.
    StillRetrying { remaining_seconds: u64 },
}

/// What the live-events (chat) connection is doing, for the log.
///
/// Only *transitions* are reported — a stream can be gone for an hour, and the
/// user does not need to be told once a minute.
#[derive(Debug)]
pub enum ChatFeedState {
    /// The feed dropped; we are retrying. Sent once per outage.
    Interrupted { reason: String },
    /// A reconnect succeeded after an interruption.
    Restored,
    /// The server has no live stream under this id any more (`204`).
    StreamGone,
    /// The server archived this stream.
    Archived,
    /// The server's own view of the stream, when it is not `active`.
    ServerState { state: String },
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
    ChatFeed(ChatFeedState),
    AudioLink(AudioLinkState),
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
    /// The error is boxed because `NetEvent` is large and every send returns
    /// this type, hot path included; only the failure needs to carry the event
    /// back, and no caller looks at it.
    pub fn send(&self, event: NetEvent) -> Result<(), Box<crossbeam_channel::SendError<NetEvent>>> {
        let result = self.0.send(event);
        wxdragon::wake_up_idle();
        result.map_err(Box::new)
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
        /// Shared because the chat feed task reconnects on its own and so needs
        /// the cookie store to outlive any one request.
        client: Arc<AudioPubClient>,
        identity: StreamIdentity,
        site_url: String,
    },
    Icecast {
        server: String,
        port: u16,
        mount: String,
        username: String,
        password: Secret,
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
    /// Rings the chat feed task to abandon what it is doing and reconnect now.
    ///
    /// A channel rather than a `Notify` because delivery has to be durable: a
    /// `Notified` future that is dropped after being notified — which is what
    /// the losing branch of a `select!` does — can swallow the notification,
    /// and a button press that silently does nothing is the worst outcome for
    /// a control whose whole purpose is reassurance. A queued message survives
    /// the dropped future.
    chat_reconnect: tokio_mpsc::UnboundedSender<()>,
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
    if let Some(client) = connection.and_then(audiopub_client)
        && let Err(e) = client.end_stream(&active.stream_id).await
    {
        log::warn!("Ending stream reported: {e}");
    }
}

async fn net_loop(mut commands: tokio_mpsc::UnboundedReceiver<NetCommand>, events: EventSender) {
    let mut connection: Option<Connection> = None;
    let mut stream: Option<ActiveStream> = None;

    while let Some(command) = commands.recv().await {
        match command {
            NetCommand::Connect { profile } => {
                // Before `connection` is replaced, not after: `end_active_stream`
                // ends the server-side row through the client that started it,
                // and every later cleanup path (`Disconnect`, `StopStream`)
                // reads whatever `connection` holds *then*. Connecting over a
                // live stream used to hand the old stream's id to the new
                // client — or to no client at all, when the new service was a
                // direct Icecast target with no API — leaving the old row live
                // until the server expired it. `StartStream` has had this guard
                // for the same reason.
                if let Some(previous) = stream.take() {
                    log::warn!(
                        "Connecting to a service while a stream is live; ending the old one"
                    );
                    end_active_stream(&previous, connection.as_ref()).await;
                    let _ = events.send(NetEvent::StreamEnded);
                }
                match profile {
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
                        match client.login(&email, password.as_str()).await {
                            Ok(()) => match client.stream_identity().await {
                                Ok(identity) => {
                                    let display_name = nickname;
                                    connection = Some(Connection::Audiopub {
                                        client: Arc::new(client),
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
                }
            }
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
                if connection.is_none() {
                    let _ = events.send(NetEvent::StreamError {
                        message: "not connected to a streaming service".into(),
                    });
                    continue;
                }
                // Starting over the top of a live stream used to overwrite the
                // `ActiveStream` and leak it: its sender and chat tasks kept
                // running and the server-side row was never ended, leaving two
                // sources racing one mount — where the second is guaranteed
                // `403 Mountpoint in use`. Only reachable now that a streaming
                // failure no longer forces the UI back to Idle behind our back.
                if let Some(previous) = stream.take() {
                    log::warn!("Starting a stream while one is live; ending the old one first");
                    end_active_stream(&previous, connection.as_ref()).await;
                }
                let Some(conn) = &connection else {
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
            NetCommand::ReconnectChat => {
                let (Some(conn), Some(active)) = (&connection, &stream) else {
                    let _ = events.send(NetEvent::ChatFeed(ChatFeedState::Interrupted {
                        reason: "not streaming".into(),
                    }));
                    continue;
                };
                if audiopub_client(conn).is_none() {
                    let _ = events.send(NetEvent::ChatFeed(ChatFeedState::Interrupted {
                        reason: "chat is only available for Audiopub services".into(),
                    }));
                    continue;
                }
                log::info!("Chat feed: reconnect requested by the user");
                let _ = active.chat_reconnect.send(());
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

/// How long the chat feed may be silent before we treat it as dead.
///
/// The server sends `: keepalive\n\n` every 30 seconds
/// (`src/routes/live/[id]/events/+server.ts` in audiopub-sv), so three missed
/// keepalives is a connection that is not coming back. A timeout is the only
/// way to catch the *half-open* case: when a NAT gateway or router silently
/// drops an idle flow there is no FIN and no RST, so the read below never
/// resolves and never errors. Without this, that connection wedges forever and
/// chat simply stops with no error anywhere.
const CHAT_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Backoff between reconnect attempts, in seconds; the last value repeats.
const CHAT_BACKOFF: [u64; 6] = [1, 2, 4, 8, 15, 30];

/// How long to wait before reconnect attempt `attempt` (0-based). Saturates at
/// the last step rather than growing without bound: a stream can be live for
/// hours, and a user who fixes their network should not then wait an hour for
/// chat to notice.
fn chat_backoff(attempt: usize) -> Duration {
    Duration::from_secs(CHAT_BACKOFF[attempt.min(CHAT_BACKOFF.len() - 1)])
}

/// Backoff between Icecast reconnect attempts, in seconds; the last repeats.
///
/// Starts at 2 rather than the chat feed's 1 because the *expected* first answer
/// after a source drops is `403 Mountpoint in use`: stock Icecast holds the
/// mount for `<source-timeout>` (default 10 s) after the source dies, so the
/// mount we want is still nominally ours. The ladder puts attempts at t+2, t+7,
/// t+17, t+32, t+52, t+82…, clearing a 10 s hold by the third attempt without
/// hammering a server that is telling us to wait.
const AUDIO_BACKOFF: [u64; 6] = [2, 5, 10, 15, 20, 30];

fn audio_backoff(attempt: usize) -> Duration {
    Duration::from_secs(AUDIO_BACKOFF[attempt.min(AUDIO_BACKOFF.len() - 1)])
}

/// How long we keep trying before giving up and ending the broadcast.
///
/// Budgeted against the server, not against patience. When a source drops,
/// audiopub-sv's `mount_remove` hook sets the stream row to `disconnected` and
/// stamps `disconnectedAt`; `StreamingService`'s poller runs every 5 minutes and
/// finishes any `disconnected` stream older than that cutoff. So the grace
/// window is 5 to 10 minutes, and `sourceConnected()` explicitly accepts a prior
/// state of `disconnected` — meaning a reconnect inside it resumes the *same*
/// stream id, chat history, listener counts and archive file.
///
/// Four minutes stops us before the earliest moment the server could expire the
/// row, so we never reconnect into a stream that has already been finished, and
/// the give-up message can say honestly that the broadcast is over.
const AUDIO_RECONNECT_BUDGET: Duration = Duration::from_secs(240);

/// When to say "still trying" — once, halfway through the budget.
const AUDIO_HALFWAY_NOTICE: Duration = Duration::from_secs(120);

/// What the supervisor should do about a failed attempt.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Wait this long, then reconnect.
    Retry(Duration),
    /// Stop. The broadcast is over.
    GiveUp { reason: String },
}

/// The retry policy, as a pure function so the ladder, the budget and the
/// terminal/retryable split are all testable without a socket or a runtime.
fn plan_retry(error: &IcecastError, attempt: usize, since_first_failure: Duration) -> Step {
    // A wrong stream key or a banned account will answer the same way in four
    // minutes' time, so those give up at once rather than burning the budget.
    if !error.retryable() {
        return Step::GiveUp {
            reason: error.explain(),
        };
    }
    let Some(left) = AUDIO_RECONNECT_BUDGET.checked_sub(since_first_failure) else {
        return Step::GiveUp {
            reason: error.explain(),
        };
    };
    if left.is_zero() {
        return Step::GiveUp {
            reason: error.explain(),
        };
    }
    // Never sleep past the budget: the last wait should land us on the deadline,
    // not well beyond it, so the give-up is announced when promised.
    Step::Retry(audio_backoff(attempt).min(left))
}

/// Poll interval once the server has said the stream is gone. It is not coming
/// back on its own, so this only exists to notice a server that changes its
/// mind, and to keep the task alive for the reconnect button.
const CHAT_GONE_POLL: Duration = Duration::from_secs(60);

/// Why [`read_chat_feed`] returned.
enum FeedExit {
    /// `finish` arrived: the stream is over. Stop for good.
    Finished,
    /// The connection ended, errored, or went silent past the watchdog.
    Dropped { reason: String },
    /// The user pressed the reconnect button.
    Forced,
    /// The stream is being torn down. Stop for good.
    Closed,
}

/// What ended a wait.
#[derive(PartialEq, Eq)]
enum Wake {
    /// The delay elapsed.
    Timer,
    /// The user pressed the reconnect button.
    User,
    /// Every sender is gone: the stream is being torn down.
    Closed,
}

/// Sleeps, unless the user asks for a reconnect first. Distinguishes the two,
/// so a press gets an answer rather than being folded into an outage that was
/// already reported — and so a closed channel ends the task instead of
/// spinning, since `recv` on one returns immediately and forever.
async fn wait_or_reconnect(
    reconnect: &mut tokio_mpsc::UnboundedReceiver<()>,
    delay: Duration,
) -> Wake {
    tokio::select! {
        request = reconnect.recv() => match request {
            Some(()) => {
                drain(reconnect);
                Wake::User
            }
            None => Wake::Closed,
        },
        _ = tokio::time::sleep(delay) => Wake::Timer,
    }
}

/// Discards reconnect requests that piled up, so several impatient presses
/// cost one reconnect rather than one each.
fn drain(reconnect: &mut tokio_mpsc::UnboundedReceiver<()>) {
    while reconnect.try_recv().is_ok() {}
}

/// Owns the live-events connection for one stream: opens it, reads it, and
/// reopens it for as long as the stream lives.
///
/// The retry loop lives in the task rather than in [`net_loop`] because that
/// loop only ever wakes on a command, so it cannot observe a task dying without
/// growing a `select!` around its receiver.
fn spawn_chat_feed(
    client: Arc<AudioPubClient>,
    stream_id: String,
    events: EventSender,
    mut reconnect: tokio_mpsc::UnboundedReceiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // True while an outage has already been reported, so the log gets one
        // line per outage rather than one per attempt: a stream can be gone for
        // an hour, and saying so once a minute would bury everything else.
        let mut reported = false;
        // Set when the user pressed the button. They are owed an answer even if
        // the answer is the same one already on screen, so this overrides
        // `reported` exactly once.
        let mut owed_an_answer = false;
        let mut attempt: usize = 0;

        loop {
            match client.open_events(&stream_id).await {
                Ok(EventsStream::Gone) => {
                    if !reported || owed_an_answer {
                        reported = true;
                        // No need to clear `owed_an_answer` here: the wait
                        // below overwrites it before anything can read it.
                        log::warn!(
                            "Chat feed: server reports stream {stream_id} no longer exists (204)"
                        );
                        let _ = events.send(NetEvent::ChatFeed(ChatFeedState::StreamGone));
                    }
                    // No backoff ladder: the stream is not coming back, so this
                    // settles to a slow poll rather than hammering.
                    match wait_or_reconnect(&mut reconnect, CHAT_GONE_POLL).await {
                        Wake::Closed => return,
                        wake => owed_an_answer = wake == Wake::User,
                    }
                    continue;
                }
                Ok(EventsStream::Open(response)) => {
                    if reported || owed_an_answer {
                        log::info!("Chat feed: reconnected to stream {stream_id}");
                        let _ = events.send(NetEvent::ChatFeed(ChatFeedState::Restored));
                    } else {
                        log::info!("Chat feed: connected to stream {stream_id}");
                    }
                    reported = false;
                    owed_an_answer = false;
                    attempt = 0;
                    match read_chat_feed(response, &events, &mut reconnect).await {
                        FeedExit::Finished => {
                            log::info!("Chat feed: stream {stream_id} finished");
                            return;
                        }
                        FeedExit::Forced => {
                            log::info!("Chat feed: reconnecting at the user's request");
                            owed_an_answer = true;
                            continue;
                        }
                        FeedExit::Closed => return,
                        FeedExit::Dropped { reason } => {
                            log::warn!("Chat feed: connection lost: {reason}");
                            reported = true;
                            let _ = events
                                .send(NetEvent::ChatFeed(ChatFeedState::Interrupted { reason }));
                        }
                    }
                }
                Err(e) => {
                    let reason = e.to_string();
                    log::warn!("Chat feed: could not open the events stream: {reason}");
                    if !reported || owed_an_answer {
                        reported = true;
                        owed_an_answer = false;
                        let _ =
                            events.send(NetEvent::ChatFeed(ChatFeedState::Interrupted { reason }));
                    }
                }
            }

            let wait = chat_backoff(attempt);
            attempt = attempt.saturating_add(1);
            log::info!(
                "Chat feed: retrying in {}s (attempt {attempt})",
                wait.as_secs()
            );
            match wait_or_reconnect(&mut reconnect, wait).await {
                Wake::Closed => return,
                Wake::User => owed_an_answer = true,
                Wake::Timer => {}
            }
        }
    })
}

/// Reads one live-events connection until it ends.
///
/// The reconnect doorbell is checked here as well as in the backoff, because a
/// wedged half-open connection is exactly the case the button exists for and
/// the read below would otherwise never return.
async fn read_chat_feed(
    response: reqwest::Response,
    events: &EventSender,
    reconnect: &mut tokio_mpsc::UnboundedReceiver<()>,
) -> FeedExit {
    use futures_util::StreamExt;

    let mut parser = SseParser::new();
    let mut body = response.bytes_stream();

    loop {
        // The watchdog resets on any *chunk*, not on any parsed event:
        // `SseParser::feed` drops `: keepalive` comment lines internally, so
        // keepalives yield no `SseEvent` and are invisible above the parser.
        // Bytes arriving is the only true liveness signal.
        //
        // `biased` so a waiting reconnect request wins over a chunk that is
        // already available: the button's whole purpose is to abandon this
        // connection, and on a wedged half-open socket the read never returns
        // at all.
        let next = tokio::select! {
            biased;
            request = reconnect.recv() => match request {
                Some(()) => {
                    drain(reconnect);
                    return FeedExit::Forced;
                }
                None => return FeedExit::Closed,
            },
            next = tokio::time::timeout(CHAT_IDLE_TIMEOUT, body.next()) => next,
        };
        let chunk = match next {
            Err(_) => {
                return FeedExit::Dropped {
                    reason: format!("no data for {} seconds", CHAT_IDLE_TIMEOUT.as_secs()),
                };
            }
            Ok(None) => {
                return FeedExit::Dropped {
                    reason: "the server closed the connection".to_string(),
                };
            }
            // Previously discarded with `let Ok(chunk) = chunk else { break }`,
            // which is why no failure here ever reached the log.
            Ok(Some(Err(e))) => {
                return FeedExit::Dropped {
                    reason: e.to_string(),
                };
            }
            Ok(Some(Ok(chunk))) => chunk,
        };

        for raw in parser.feed(&chunk) {
            match LiveEvent::from_sse(&raw) {
                Some(LiveEvent::Chat(message)) => {
                    let _ = events.send(NetEvent::Chat(message));
                }
                Some(LiveEvent::Listeners { active, peak }) => {
                    // We are one of the listeners (this SSE connection);
                    // do not count ourselves.
                    let _ = events.send(NetEvent::Listeners {
                        active: active.saturating_sub(1),
                        peak: peak.saturating_sub(1),
                    });
                }
                // The server sends `state` immediately on every connect, so
                // `active` would put a line in the log on every reconnect.
                // The others are worth saying: `disconnected` means
                // the server has lost the source link and will finish the
                // stream within minutes, even while our Icecast socket looks
                // healthy.
                Some(LiveEvent::State { state }) if state != "active" => {
                    log::warn!("Chat feed: server reports stream state {state:?}");
                    let _ = events.send(NetEvent::ChatFeed(ChatFeedState::ServerState { state }));
                }
                Some(LiveEvent::Archived) => {
                    log::info!("Chat feed: the server archived this stream");
                    let _ = events.send(NetEvent::ChatFeed(ChatFeedState::Archived));
                }
                Some(LiveEvent::Finish) => {
                    let _ = events.send(NetEvent::StreamEnded);
                    return FeedExit::Finished;
                }
                // Deleting a row would break the append-only invariant the chat
                // list relies on (`chat::append_new_messages` maps list indices
                // onto `run.chat`); it needs a full refresh, which is its own
                // change.
                Some(LiveEvent::State { .. }) | Some(LiveEvent::ChatDeleted { .. }) | None => {}
            }
        }
    }
}

/// Why [`pump_audio`] returned.
enum SendExit {
    /// The encoder channel closed: the stream is being stopped normally.
    Ended,
    /// A write failed or timed out.
    Dropped { error: IcecastError },
}

/// Sends encoded audio down one connection until it ends.
async fn pump_audio(
    conn: &mut IcecastConnection,
    audio: &mut tokio_mpsc::Receiver<Vec<u8>>,
) -> SendExit {
    while let Some(chunk) = audio.recv().await {
        if let Err(error) = conn.send(&chunk).await {
            return SendExit::Dropped { error };
        }
    }
    SendExit::Ended
}

/// Owns the Icecast source connection for one stream: sends down it, and
/// reopens it for as long as the stream lives or the retry budget allows.
///
/// Mirrors [`spawn_chat_feed`], including the `reported` flag that gives the
/// log one line per outage rather than one per attempt. The retry loop
/// lives in the task for the same reason it does there: [`net_loop`] only wakes
/// on a command, so it cannot observe a task dying.
///
/// The first connection is handed in already open rather than being made here,
/// so that a wrong stream key still fails `Start streaming` loudly and at once
/// instead of becoming four minutes of quiet retrying behind a UI that claims
/// to be live.
///
/// **`audio` must not be dropped except on the terminal path.** Dropping the
/// receiver closes the channel, which the engine reads as `TrySendError::Closed`
/// and answers by stopping encoding for good — after which a successful
/// reconnect would have nothing left to send. Holding it open through an outage
/// is exactly what makes the engine's drop-newest policy do its job.
fn spawn_icecast_sender(
    target: IcecastTarget,
    first: IcecastConnection,
    mut audio: tokio_mpsc::Receiver<Vec<u8>>,
    events: EventSender,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut conn = first;
        let mut said_halfway = false;
        let mut first_failure: Option<Instant> = None;
        let mut attempt: usize = 0;

        loop {
            let mut error = match pump_audio(&mut conn, &mut audio).await {
                SendExit::Ended => {
                    conn.close().await;
                    return;
                }
                SendExit::Dropped { error } => error,
            };
            log::warn!("Icecast source: {error}");

            let started = *first_failure.get_or_insert_with(Instant::now);
            // Exactly once per outage, because reaching here is what an outage
            // *is*: the only way back to pumping is a successful reconnect at
            // the bottom of the inner loop. A four-minute outage is two or three
            // lines in the log, not eighty.
            let _ = events.send(NetEvent::AudioLink(AudioLinkState::Interrupted {
                reason: error.explain(),
            }));

            // Reconnect until we are back, or until the budget says the
            // broadcast is over. Every failure — the one that dropped us and
            // every failed attempt after it — is judged in this one place.
            loop {
                match plan_retry(&error, attempt, started.elapsed()) {
                    Step::GiveUp { reason } => {
                        log::error!("Icecast source: giving up ({reason})");
                        let _ = events.send(NetEvent::StreamError {
                            message: format!(
                                "The audio connection could not be restored: {reason}. \
                                 The broadcast has ended."
                            ),
                        });
                        return;
                    }
                    Step::Retry(wait) => {
                        log::info!(
                            "Icecast source: reconnecting in {}s (attempt {attempt})",
                            wait.as_secs()
                        );
                        tokio::time::sleep(wait).await;
                        attempt += 1;
                    }
                }

                if !said_halfway && started.elapsed() >= AUDIO_HALFWAY_NOTICE {
                    said_halfway = true;
                    let _ = events.send(NetEvent::AudioLink(AudioLinkState::StillRetrying {
                        remaining_seconds: AUDIO_RECONNECT_BUDGET
                            .saturating_sub(started.elapsed())
                            .as_secs(),
                    }));
                }

                // Discard whatever the engine queued while we were down, and do
                // it *before* connecting so the discard covers the connect
                // latency too. This is a live stream: delivering the backlog
                // would push every listener permanently further behind live —
                // Icecast paces listeners by the rate the source delivers, so
                // the debt is never repaid — and would replay a stale fragment
                // of speech out of context. Resuming at live is the only
                // sensible answer; the gap is the honest cost of the outage.
                while audio.try_recv().is_ok() {}

                match IcecastConnection::connect(&target).await {
                    Ok(fresh) => {
                        conn = fresh;
                        let gap_seconds = started.elapsed().as_secs();
                        log::info!("Icecast source: reconnected after {gap_seconds}s");
                        let _ = events.send(NetEvent::AudioLink(AudioLinkState::Restored {
                            gap_seconds,
                        }));
                        said_halfway = false;
                        first_failure = None;
                        attempt = 0;
                        break;
                    }
                    Err(e) => {
                        log::warn!("Icecast source: reconnect failed ({e})");
                        error = e;
                    }
                }
            }
        }
    })
}

async fn start_stream(
    conn: &Connection,
    title: &str,
    description: &str,
    archive: bool,
    content_type: &str,
    audio: tokio_mpsc::Receiver<Vec<u8>>,
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
            // The first connect stays here, and inline, so a wrong stream key or
            // a banned account fails `Start streaming` at once with a reason.
            // Every *later* connect happens inside the sender task.
            let icecast = IcecastConnection::connect(&target)
                .await
                .map_err(|e| e.to_string())?;
            let icecast_task = spawn_icecast_sender(target, icecast, audio, events.clone());

            // The feed opens itself. Chat failing is not a reason to refuse to
            // broadcast, so unlike the Icecast connection above, a live-events
            // endpoint that will not open is reported in the log and retried
            // rather than failing the whole stream start.
            let (chat_reconnect, reconnect_rx) = tokio_mpsc::unbounded_channel();
            let sse_task = spawn_chat_feed(
                client.clone(),
                stream_id.clone(),
                events.clone(),
                reconnect_rx,
            );

            Ok(ActiveStream {
                stream_id,
                sse_task: Some(sse_task),
                icecast_task,
                chat_reconnect,
            })
        }
        Connection::Icecast { mount, .. } => {
            let target = direct_icecast_target(conn, content_type)?;
            let icecast = IcecastConnection::connect(&target)
                .await
                .map_err(|e| e.to_string())?;
            let icecast_task = spawn_icecast_sender(target, icecast, audio, events.clone());

            Ok(ActiveStream {
                stream_id: format!("icecast:{}", normalize_mount(mount)),
                sse_task: None,
                // Never rung: a direct Icecast mount has no chat feed at all.
                chat_reconnect: tokio_mpsc::unbounded_channel().0,
                icecast_task,
            })
        }
    }
}

#[cfg(test)]
mod chat_feed_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// The whole point of the feature: a source whose connection dies mid-send
    /// reopens it and carries on, rather than ending the broadcast.
    ///
    /// Time is paused so the backoff ladder costs nothing; the sockets are real,
    /// so the handshake, the drop and the second connect are the genuine ones.
    #[tokio::test(start_paused = true)]
    async fn sender_reconnects_after_the_server_drops_the_source() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // The second connection is reported over a channel rather than by
        // returning, so the server task can hold that socket open afterwards.
        // Letting it close would be a second, genuine outage, and the
        // one-line-per-outage assertion below would be measuring the test's own
        // teardown instead of the sender's behaviour.
        let (report_tx, report_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            // First source connection: handshake, take some audio, then vanish
            // the way a dropped link does.
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .unwrap();
            let _ = sock.read(&mut buf).await;
            drop(sock);

            // Second: the reconnect.
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut head = vec![0u8; 4096];
            let n = sock.read(&mut head).await.unwrap();
            let request = String::from_utf8_lossy(&head[..n]).to_string();
            sock.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .unwrap();
            let mut audio = vec![0u8; 4];
            sock.read_exact(&mut audio).await.unwrap();
            let _ = report_tx.send((request, audio));

            // Hold it open, and keep draining so the sender never stalls.
            loop {
                if sock.read(&mut buf).await.unwrap_or(0) == 0 {
                    std::future::pending::<()>().await;
                }
            }
        });

        let target = IcecastTarget {
            host: addr.to_string(),
            mount: "user-123".into(),
            username: "source".into(),
            password: Secret::new("key"),
            content_type: "audio/mpeg".into(),
        };
        let first = IcecastConnection::connect(&target).await.unwrap();

        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (audio_tx, audio_rx) = tokio_mpsc::channel(200);
        let task = spawn_icecast_sender(target, first, audio_rx, EventSender(event_tx));

        // Keep offering audio. The first blocks go into the doomed connection;
        // once it fails, the sender must open a new one and send what follows.
        let feeder = tokio::spawn(async move {
            for _ in 0..200 {
                if audio_tx.send(b"MP3!".to_vec()).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let (request, audio) = report_rx.await.unwrap();
        assert!(request.starts_with("PUT /user-123 HTTP/1.1\r\n"));
        // The reconnect re-authenticates with the same stored key, which is why
        // no re-login is needed: the stream key is a long-lived per-user value.
        assert!(request.contains("Authorization: Basic c291cmNlOmtleQ=="));
        assert_eq!(&audio, b"MP3!");

        feeder.abort();
        task.abort();
        server.abort();

        let events: Vec<_> = event_rx.try_iter().collect();
        let interrupted = events
            .iter()
            .filter(|e| matches!(e, NetEvent::AudioLink(AudioLinkState::Interrupted { .. })))
            .count();
        let restored = events
            .iter()
            .filter(|e| matches!(e, NetEvent::AudioLink(AudioLinkState::Restored { .. })))
            .count();
        assert_eq!(interrupted, 1, "one line per outage, not one per attempt");
        assert_eq!(restored, 1, "the recovery must be announced");
        // The broadcast must not have been declared over.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, NetEvent::StreamError { .. })),
            "a recoverable blip must never end the stream"
        );
    }

    /// A terminal rejection must end the broadcast at once rather than retry
    /// for four minutes: a revoked stream key or an expired stream row will
    /// answer exactly the same way every time.
    #[tokio::test(start_paused = true)]
    async fn a_terminal_rejection_ends_the_stream_without_retrying() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = attempts.clone();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .await
                .unwrap();
            let _ = sock.read(&mut buf).await;
            drop(sock);
            // Every reconnect is refused with a reason that cannot change.
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(
                        b"HTTP/1.0 401 Unauthorized\r\n\
                          icecast-auth-message: No active stream found\r\n\r\n",
                    )
                    .await;
            }
        });

        let target = IcecastTarget {
            host: addr.to_string(),
            mount: "u".into(),
            username: "source".into(),
            password: Secret::new("key"),
            content_type: "audio/mpeg".into(),
        };
        let first = IcecastConnection::connect(&target).await.unwrap();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (audio_tx, audio_rx) = tokio_mpsc::channel(200);
        let task = spawn_icecast_sender(target, first, audio_rx, EventSender(event_tx));

        let feeder = tokio::spawn(async move {
            for _ in 0..200 {
                if audio_tx.send(b"MP3!".to_vec()).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        // The task returns of its own accord once it gives up.
        tokio::time::timeout(Duration::from_secs(600), task)
            .await
            .expect("the sender must give up rather than retry for ever")
            .unwrap();
        feeder.abort();
        server.abort();

        let events: Vec<_> = event_rx.try_iter().collect();
        let message = events
            .iter()
            .find_map(|e| match e {
                NetEvent::StreamError { message } => Some(message.clone()),
                _ => None,
            })
            .expect("the broadcast must be reported as over");
        // The server's own words reach the user, so they know it is their
        // stream that expired and not their network that failed.
        assert!(message.contains("No active stream found"), "{message}");
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a terminal rejection must not be retried"
        );
    }

    #[test]
    fn audio_backoff_climbs_then_saturates() {
        assert_eq!(audio_backoff(0), Duration::from_secs(2));
        assert_eq!(audio_backoff(2), Duration::from_secs(10));
        assert_eq!(audio_backoff(5), Duration::from_secs(30));
        assert_eq!(audio_backoff(500), Duration::from_secs(30));
    }

    /// A fast reconnect races our own dying socket: stock Icecast holds the
    /// mount for `<source-timeout>` (default 10 s) after a source drops, so the
    /// ladder has to reach past that without hammering a server saying "wait".
    #[test]
    fn audio_backoff_clears_the_mount_hold_by_the_third_attempt() {
        let elapsed: u64 = (0..3).map(|a| audio_backoff(a).as_secs()).sum();
        assert!(elapsed >= 10, "third attempt lands at t+{elapsed}s");
    }

    /// The budget exists to stop *before* the server can finish the stream:
    /// `StreamingService`'s poller runs every 5 minutes and ends any stream
    /// that has been `disconnected` longer than that, so retrying past the
    /// 5-minute floor would reconnect into a stream that no longer exists.
    #[test]
    fn retry_budget_stops_before_the_servers_poller() {
        assert!(AUDIO_RECONNECT_BUDGET < Duration::from_secs(5 * 60));
        assert!(AUDIO_HALFWAY_NOTICE < AUDIO_RECONNECT_BUDGET);
    }

    fn rejected(status: u16, line: &str) -> IcecastError {
        IcecastError::Rejected {
            status,
            status_line: line.into(),
            message: None,
        }
    }

    #[test]
    fn terminal_errors_give_up_without_burning_the_budget() {
        // A wrong stream key answers the same way in four minutes' time.
        let step = plan_retry(
            &rejected(401, "HTTP/1.0 401 Unauthorized"),
            0,
            Duration::ZERO,
        );
        assert!(matches!(step, Step::GiveUp { .. }));
    }

    #[test]
    fn retryable_errors_inside_the_budget_are_retried() {
        let step = plan_retry(
            &rejected(403, "HTTP/1.0 403 Mountpoint in use"),
            0,
            Duration::from_secs(3),
        );
        assert_eq!(step, Step::Retry(Duration::from_secs(2)));
    }

    #[test]
    fn retryable_errors_past_the_budget_give_up() {
        let err = IcecastError::Timeout {
            what: "sending audio",
        };
        assert!(matches!(
            plan_retry(&err, 9, AUDIO_RECONNECT_BUDGET),
            Step::GiveUp { .. }
        ));
        assert!(matches!(
            plan_retry(&err, 9, AUDIO_RECONNECT_BUDGET + Duration::from_secs(60)),
            Step::GiveUp { .. }
        ));
    }

    /// The last wait must land on the deadline, not well past it, so the
    /// give-up is announced when the message says it will be.
    #[test]
    fn a_retry_never_sleeps_past_the_budget() {
        let err = IcecastError::Timeout {
            what: "sending audio",
        };
        let left = Duration::from_secs(3);
        let step = plan_retry(&err, 5, AUDIO_RECONNECT_BUDGET - left);
        assert_eq!(step, Step::Retry(left));
    }

    #[test]
    fn backoff_climbs_then_saturates() {
        assert_eq!(chat_backoff(0), Duration::from_secs(1));
        assert_eq!(chat_backoff(3), Duration::from_secs(8));
        assert_eq!(chat_backoff(5), Duration::from_secs(30));
        // A stream can be live for hours; the wait must not grow with it.
        assert_eq!(chat_backoff(500), Duration::from_secs(30));
    }

    /// Three missed keepalives. The server's interval is 30s, hard-coded in
    /// `src/routes/live/[id]/events/+server.ts`, so this is a measured bound
    /// rather than a guess — but it must stay comfortably above 30s or a merely
    /// quiet stream would reconnect on every keepalive gap.
    #[test]
    fn idle_timeout_clears_the_servers_keepalive_interval() {
        assert!(CHAT_IDLE_TIMEOUT >= Duration::from_secs(90));
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
            stream_key: Secret::new("stream-key"),
        };
        let target = audiopub_target_for("https://example.org/", &identity, "audio/mpeg");
        assert_eq!(target.host, "live.example.org:8000");
        assert_eq!(target.mount, "user-123");
        assert_eq!(target.username, "source");
        assert_eq!(target.password.as_str(), "stream-key");
        assert_eq!(target.content_type, "audio/mpeg");
    }

    #[test]
    fn direct_icecast_target_uses_profile_fields() {
        let conn = Connection::Icecast {
            server: "ice.example.org".to_string(),
            port: 9000,
            mount: "/live".to_string(),
            username: "dj".to_string(),
            password: Secret::new("secret"),
        };
        let target = direct_icecast_target(&conn, "audio/aac").unwrap();
        assert_eq!(target.host, "ice.example.org:9000");
        assert_eq!(target.mount, "live");
        assert_eq!(target.username, "dj");
        assert_eq!(target.password.as_str(), "secret");
        assert_eq!(target.content_type, "audio/aac");
    }

    #[test]
    fn direct_icecast_target_defaults_blank_username() {
        let conn = Connection::Icecast {
            server: "ice.example.org".to_string(),
            port: 8000,
            mount: "live".to_string(),
            username: String::new(),
            password: Secret::new("secret"),
        };
        let target = direct_icecast_target(&conn, "audio/mpeg").unwrap();
        assert_eq!(target.username, "source");
    }
}
