//! The wxDragon UI: main frame, tabs, menu bar, status bar, and the pump
//! timer that carries events from the audio/network threads onto the UI
//! thread.

mod buses;
mod chat;
mod connect_dialog;
mod fx;
mod fx_editor;
mod fx_params;
mod home;
mod preferences;
mod scenes;
mod sends;
mod slider_uia;
mod stream_info_dialog;

use crate::audio::{AudioEngine, EngineCommand, FeedKind, SourceSpec, capture::CaptureKind};
use crate::config::{Config, SourceKindConfig};
use crate::net::{NetCommand, NetEvent, NetHandle};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use wxdragon::prelude::*;

// wxWidgets key codes (not exported by wxdragon).
pub const WXK_TAB: i32 = 9;
pub const WXK_ESCAPE: i32 = 27;
pub const WXK_DELETE: i32 = 127;
pub const WXK_PAGEUP: i32 = 366;
pub const WXK_PAGEDOWN: i32 = 367;
pub const WXK_END: i32 = 312;
pub const WXK_HOME: i32 = 313;
pub const WXK_UP: i32 = 315;
pub const WXK_DOWN: i32 = 317;

const ID_MENU_CONFIGURE: i32 = 2001;
const ID_MENU_PREFERENCES: i32 = 2002;
const ID_MENU_EXIT: i32 = 2003;
const ID_MENU_STREAM_INFO: i32 = 2004;
const ID_MENU_ABOUT: i32 = 2101;
const ID_MENU_README: i32 = 2102;

const README_URL: &str = "https://github.com/ironcross32/pubsplash#readme";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    Idle,
    Starting,
    Live { stream_id: String },
    Stopping,
}

/// One received chat message plus when we got it (for relative timestamps).
pub struct ChatEntry {
    pub user: String,
    pub content: String,
    pub received: Instant,
}

/// Stream metadata sent to the server when a stream is created. Deliberately
/// not persisted: it resets to these defaults every launch.
#[derive(Clone)]
pub struct StreamInfo {
    pub title: String,
    pub description: String,
    pub archive: bool,
}

impl Default for StreamInfo {
    fn default() -> Self {
        Self {
            title: "Stream".to_string(),
            description: "This is just a stream".to_string(),
            archive: false,
        }
    }
}

/// Mutable runtime state (not persisted).
pub struct Runtime {
    pub stream: StreamState,
    pub stream_started: Option<Instant>,
    pub connected_site: Option<String>,
    pub connecting: bool,
    pub listeners: u32,
    pub listener_peak: u32,
    pub chat: Vec<ChatEntry>,
    pub stream_info: StreamInfo,
    /// Whether the user has confirmed the stream info dialog this session.
    pub stream_info_set: bool,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            stream: StreamState::Idle,
            stream_started: None,
            connected_site: None,
            connecting: false,
            listeners: 0,
            listener_peak: 0,
            chat: Vec::new(),
            stream_info: StreamInfo::default(),
            stream_info_set: false,
        }
    }
}

/// An MSAA accessible object that only supplies a name, leaving all other
/// behavior to the control's default accessibility.
struct NameOnlyAccessible(String);

impl wxdragon::accessible::AccessibleImpl for NameOnlyAccessible {
    fn get_name(&self, child_id: i32) -> (wxdragon::accessible::AccStatus, Option<String>) {
        // Child id 0 is the control itself (MSAA CHILDID_SELF). Ids 1..n are
        // the control's children (e.g. list box items) — those must fall
        // through to the default accessible or every item announces as the
        // control's name.
        if child_id == 0 {
            (
                wxdragon::ffi::wxd_AccStatus_WXD_ACC_OK,
                Some(self.0.clone()),
            )
        } else {
            (wxdragon::ffi::wxd_AccStatus_WXD_ACC_NOT_IMPLEMENTED, None)
        }
    }
}

/// Gives a control an explicit accessible name for screen readers. Needed
/// where the visual label (or adjacent StaticText) is not announced.
pub fn set_accessible_name(widget: &dyn WxWidget, name: &str) {
    widget.set_accessible(wxdragon::accessible::Accessible::new(
        widget,
        NameOnlyAccessible(name.to_string()),
    ));
}

/// Extracts (key code, ctrl held) from a window event, if it's a key event.
pub fn key_of(event: &WindowEventData) -> Option<(i32, bool)> {
    if let WindowEventData::Keyboard(kb) = event {
        kb.get_key_code().map(|code| (code, kb.control_down()))
    } else {
        None
    }
}

/// Widgets that need updating after events. Populated during `build`.
pub struct Widgets {
    pub frame: Frame,
    pub status_bar: StatusBar,
    pub overview: TextCtrl,
    pub stream_button: Button,
    pub home_scene_list: ListBox,
    pub mixer_panel: Panel,
    /// The replaceable panel holding the current mixer strips.
    pub mixer_inner: RefCell<Option<Panel>>,
    pub home_panel: Panel,
    pub chat_list: ListBox,
    #[allow(dead_code)]
    pub chat_input: TextCtrl,
    pub scenes_list: ListBox,
    pub sources_list: ListBox,
    pub bus_list: ListBox,
    pub fx_list: ListBox,
}

/// Live handles into the Configure Audio Pub dialog while it is open, so
/// connection results arriving on the pump can report inside the dialog and
/// put keyboard focus back on the connect button.
#[derive(Clone)]
pub struct ConnectUi {
    pub dialog: Dialog,
    pub connect_button: Button,
}

/// A VST scan in flight: the worker handle plus the progress dialog the pump
/// drives. The progress dialog is created lazily on the `Started` event
/// (its maximum isn't known until enumeration finishes) and destroyed by
/// dropping it when the scan ends.
pub struct ScanUi {
    pub handle: crate::vst::scan::ScanHandle,
    pub progress: Option<ProgressDialog>,
    /// False while the worker is still enumerating candidates: the dialog is
    /// in indeterminate (pulse) mode until the `Started` event brings the
    /// total and it is recreated with a real range.
    pub determinate: bool,
    /// The Preferences dialog, parent for the progress dialog and result box.
    pub parent: Dialog,
}

pub struct App {
    pub config: RefCell<Config>,
    pub run: RefCell<Runtime>,
    pub engine: AudioEngine,
    pub net: NetHandle,
    pub speaker: crate::tts::sapi::Speaker,
    pub widgets: RefCell<Option<Widgets>>,
    pub connect_ui: RefCell<Option<ConnectUi>>,
    /// Plugins known from the last completed scan (vst_plugins.json).
    pub plugins: RefCell<crate::vst::PluginCache>,
    pub scan: RefCell<Option<ScanUi>>,
    /// Live plugin instances, mirroring `config.buses`.
    pub fx: RefCell<FxRuntime>,
    /// Named FX chains saved to fx_chains.json.
    pub chain_library: RefCell<crate::fx::FxChainLibrary>,
    /// Open native plugin editor windows.
    pub open_editors: RefCell<Vec<fx_editor::EditorWindow>>,
}

/// The live plugin instances backing the FX chains, kept in lockstep with
/// `config.buses`. A `None` slot is a chain entry whose plugin is missing on
/// this machine (it processes as a gap). See `ui::fx` for the lifecycle.
#[derive(Default)]
pub struct FxRuntime {
    /// `buses[bus][slot]`, matching `config.buses.buses[bus].chain[slot]`.
    pub buses: Vec<Vec<Option<Arc<crate::vst::host2::Vst2Plugin>>>>,
    /// Matching `config.buses.master_chain`.
    pub master: Vec<Option<Arc<crate::vst::host2::Vst2Plugin>>>,
    /// Instances removed from a chain, dropped once the engine acknowledges
    /// the swap (so the audio thread never holds the last reference).
    pub retiring: Vec<Arc<crate::vst::host2::Vst2Plugin>>,
}

/// Reads an incoming chat message through every unmuted TTS source in the
/// active scene.
fn speak_chat(app: &Rc<App>, user: &str, content: &str) {
    let config = app.config.borrow();
    let Some(scene) = config.scenes.active_scene() else {
        return;
    };
    for source in &scene.sources {
        let SourceKindConfig::Tts(tts) = &source.kind else {
            continue;
        };
        if source.muted {
            continue;
        }
        // Only SAPI speaks in this version; other engines are upcoming.
        if tts.engine == "sapi" {
            app.speaker.speak(crate::tts::sapi::SpeakRequest {
                text: format!("{user}: {content}"),
                voice: tts.voice.clone(),
                rate: tts.rate,
                volume: tts.volume,
                source_name: source.name.clone(),
                to_stream: tts.output_to_stream,
            });
        }
    }
}

impl App {
    pub fn widgets<R>(&self, f: impl FnOnce(&Widgets) -> R) -> Option<R> {
        self.widgets.borrow().as_ref().map(f)
    }

    pub fn save_config(&self) {
        crate::config::save(&self.config.borrow());
    }

    /// Sends the active scene's sources to the audio engine (mixer order).
    /// Send targets are translated from bus names to current bus indices,
    /// so call this again after any bus reorder.
    pub fn sync_engine_sources(&self) {
        let config = self.config.borrow();
        let Some(scene) = config.scenes.active_scene() else {
            return;
        };
        let bus_index = |name: &str| config.buses.buses.iter().position(|b| b.name == name);
        let specs: Vec<SourceSpec> = scene
            .sources
            .iter()
            .map(|s| SourceSpec {
                name: s.name.clone(),
                volume: s.volume,
                muted: s.muted,
                to_master: s.to_master,
                sends: s
                    .sends
                    .iter()
                    .filter_map(|send| {
                        Some(crate::audio::SendSpec {
                            bus_index: bus_index(&send.bus)?,
                            level: send.level,
                        })
                    })
                    .collect(),
                feed: match &s.kind {
                    SourceKindConfig::Microphone { device_id } => {
                        FeedKind::Capture(CaptureKind::Microphone {
                            device_id: device_id.clone(),
                        })
                    }
                    SourceKindConfig::DesktopAudio => FeedKind::Capture(CaptureKind::DesktopAudio),
                    SourceKindConfig::Application { process_name } => {
                        match crate::audio::device::find_process(process_name) {
                            Some(pid) => FeedKind::Capture(CaptureKind::Application { pid }),
                            None => {
                                log::warn!(
                                    "Process {process_name:?} not running; source will be silent"
                                );
                                FeedKind::External
                            }
                        }
                    }
                    SourceKindConfig::Tts(_) | SourceKindConfig::SoundEvents => FeedKind::External,
                },
            })
            .collect();
        self.engine.send(EngineCommand::SetSources(specs));
        self.engine.send(EngineCommand::SetMasterVolume(
            config.audio.master_volume,
        ));
        self.engine
            .send(EngineCommand::SetMasterMute(config.audio.master_muted));
    }

    pub fn is_streaming_or_starting(&self) -> bool {
        !matches!(self.run.borrow().stream, StreamState::Idle)
    }

    /// The public page of the current live stream, once it is live.
    #[allow(dead_code)]
    pub fn stream_url(&self) -> Option<String> {
        let run = self.run.borrow();
        let StreamState::Live { stream_id } = &run.stream else {
            return None;
        };
        let site = run.connected_site.as_deref()?;
        Some(format!("{}/live/{}", site.trim_end_matches('/'), stream_id))
    }

    pub fn stop_streaming(&self) {
        {
            let mut run = self.run.borrow_mut();
            if matches!(run.stream, StreamState::Idle | StreamState::Stopping) {
                return;
            }
            run.stream = StreamState::Stopping;
        }
        self.engine.send(EngineCommand::StopEncoding);
        self.net.send(NetCommand::StopStream);
        self.refresh_stream_ui();
    }

    /// Repaints everything that depends on stream state: overview box,
    /// stream button, status bar.
    pub fn refresh_stream_ui(&self) {
        let run = self.run.borrow();
        let config = self.config.borrow();

        let (status_text, button_label) = match &run.stream {
            StreamState::Idle => ("Not streaming".to_string(), "&Start streaming".to_string()),
            StreamState::Starting => ("Connecting...".to_string(), "S&top streaming".to_string()),
            StreamState::Live { .. } => {
                let duration = run
                    .stream_started
                    .map(|t| format_duration(t.elapsed()))
                    .unwrap_or_default();
                (
                    format!(
                        "Streaming - {} kbps {} - {}",
                        config.audio.bitrate_kbps,
                        config.audio.format.display_name(),
                        duration
                    ),
                    "S&top streaming".to_string(),
                )
            }
            StreamState::Stopping => ("Stopping...".to_string(), "S&top streaming".to_string()),
        };

        let streaming = matches!(run.stream, StreamState::Live { .. });
        let overview = format!(
            "Status: {}\nListeners: {}\nListener peak: {}\nDuration: {}",
            match &run.stream {
                StreamState::Idle => "Not streaming",
                StreamState::Starting => "Starting...",
                StreamState::Live { .. } => "Streaming",
                StreamState::Stopping => "Stopping...",
            },
            run.listeners,
            run.listener_peak,
            if streaming {
                run.stream_started
                    .map(|t| format_duration(t.elapsed()))
                    .unwrap_or_default()
            } else {
                "-".to_string()
            }
        );
        drop(run);
        drop(config);

        self.widgets(|w| {
            w.status_bar.set_status_text(&status_text, 0);
            if w.overview.get_value() != overview {
                w.overview.set_value(&overview);
            }
            w.stream_button.set_label(&button_label);
        });
    }
}

/// Kicks off the stream (engine encoding + network side). If the user never
/// confirmed the stream info this session, the Set stream info dialog opens
/// first; cancelling it cancels the start.
pub fn start_streaming(app: &Rc<App>) {
    {
        let run = app.run.borrow();
        if run.stream != StreamState::Idle {
            return;
        }
        if run.connected_site.is_none() {
            drop(run);
            app.widgets(|w| {
                show_error(
                    &w.frame,
                    "Not connected",
                    "Connect to an Audio Pub site first (File > Configure Audio Pub).",
                )
            });
            return;
        }
    }
    if !app.run.borrow().stream_info_set {
        let frame = app.widgets(|w| w.frame.clone());
        let Some(frame) = frame else { return };
        if !stream_info_dialog::show(app, &frame) {
            return;
        }
    }
    let info = app.run.borrow().stream_info.clone();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let bitrate = app.config.borrow().audio.bitrate_kbps;
    app.engine.send(EngineCommand::StartEncoding {
        bitrate_kbps: bitrate,
        out: tx,
    });
    app.net.send(NetCommand::StartStream {
        title: info.title,
        description: info.description,
        archive: info.archive,
        content_type: "audio/mpeg".into(),
        audio: rx,
    });
    {
        let mut run = app.run.borrow_mut();
        run.stream = StreamState::Starting;
        run.chat.clear();
    }
    app.refresh_stream_ui();
    chat::refresh_chat_list(app);
}

/// Expands `{title}` and `{url}` placeholders in a template, for sharing the
/// stream elsewhere (e.g. social media announcements).
#[allow(dead_code)]
pub fn expand_stream_tokens(template: &str, title: &str, url: &str) -> String {
    template.replace("{title}", title).replace("{url}", url)
}

pub fn format_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

pub fn show_error(parent: &dyn WxWidget, caption: &str, message: &str) {
    let dialog = MessageDialog::builder(parent, message, caption)
        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
        .build();
    dialog.show_modal();
}

pub fn show_info(parent: &dyn WxWidget, caption: &str, message: &str) {
    let dialog = MessageDialog::builder(parent, message, caption)
        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
        .build();
    dialog.show_modal();
}

/// Builds the whole UI. Called from inside `wxdragon::main`.
pub fn build(app: Rc<App>) {
    let frame = Frame::builder()
        .with_title("Pubsplash")
        .with_size(Size::new(900, 700))
        .build();

    let notebook = Notebook::builder(&frame).build();
    let home_panel = Panel::builder(&notebook).build();
    let chat_panel = Panel::builder(&notebook).build();
    let scenes_panel = Panel::builder(&notebook).build();
    let buses_panel = Panel::builder(&notebook).build();
    notebook.add_page(&home_panel, "Home", true, None);
    notebook.add_page(&chat_panel, "Chat", false, None);
    notebook.add_page(&scenes_panel, "Scenes and Sources", false, None);
    notebook.add_page(&buses_panel, "Buses", false, None);

    let frame_sizer = BoxSizer::builder(Orientation::Vertical).build();
    frame_sizer.add(&notebook, 1, SizerFlag::Expand | SizerFlag::All, 0);
    frame.set_sizer(frame_sizer, true);

    let status_bar = StatusBar::builder(&frame)
        .with_fields_count(1)
        .add_initial_text(0, "Not streaming")
        .build();
    frame.set_existing_status_bar(Some(&status_bar));

    build_menu(&app, &frame);

    // Tabs fill in the Widgets struct.
    let (overview, stream_button, home_scene_list, mixer_panel) = home::build(&app, &home_panel);
    let (chat_list, chat_input) = chat::build(&app, &chat_panel);
    let (scenes_list, sources_list) = scenes::build(&app, &scenes_panel);
    let (bus_list, fx_list) = buses::build(&app, &buses_panel);

    *app.widgets.borrow_mut() = Some(Widgets {
        frame: frame.clone(),
        status_bar,
        overview,
        stream_button,
        home_scene_list,
        mixer_panel,
        mixer_inner: RefCell::new(None),
        home_panel: home_panel.clone(),
        chat_list,
        chat_input,
        scenes_list,
        sources_list,
        bus_list,
        fx_list,
    });

    // Instantiate the configured FX chains before syncing the engine; collect
    // any plugins missing on this machine for a single summary.
    let missing = fx::instantiate_all(&app);

    // Populate dynamic content now that widgets exist.
    home::refresh_scene_list(&app);
    home::rebuild_mixer(&app);
    scenes::refresh_scenes_list(&app);
    scenes::refresh_sources_list(&app);
    buses::refresh_bus_list(&app);
    buses::refresh_fx_list(&app);
    app.refresh_stream_ui();
    // Buses before sources: sources reference buses by index.
    fx::sync_engine_buses(&app);
    app.sync_engine_sources();

    if !missing.is_empty() {
        let mut message = String::from(
            "Some plugins used by your buses are not installed on this machine and will be skipped until you install them and rescan:\n",
        );
        for plugin in &missing {
            message.push_str(&format!("\n- {}", plugin.display()));
        }
        show_info(&frame, "Missing plugins", &message);
    }

    // Exit confirmation while streaming (menu Exit and ALT+F4 both arrive here).
    {
        let app = app.clone();
        let frame_for_close = frame.clone();
        frame.on_close(move |event| {
            if app.is_streaming_or_starting() {
                let dialog = MessageDialog::builder(
                    &frame_for_close,
                    "You are currently streaming. Stop the stream and exit?",
                    "Exit Pubsplash",
                )
                .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
                .build();
                if dialog.show_modal() != ID_YES {
                    if let WindowEventData::General(e) = &event {
                        e.veto();
                    }
                    return;
                }
                // Cleanly terminate the stream before shutdown.
                app.stop_streaming();
            }
            // Close plugin editors (and remove the keyboard hook) on exit.
            fx_editor::close_all(&app);
            app.save_config();
            event.skip(true);
        });
    }

    // The pump: carries events from the engine/net threads onto the UI
    // thread and refreshes time-based displays.
    {
        let app = app.clone();
        let timer = Timer::new(&frame);
        let mut ticks: u32 = 0;
        timer.on_tick(move |_| {
            pump_events(&app);
            ticks = ticks.wrapping_add(1);
            if ticks % 10 == 0 {
                // Once a second: durations and relative chat times.
                if app.is_streaming_or_starting() {
                    app.refresh_stream_ui();
                }
                chat::refresh_chat_times(&app);
            }
        });
        timer.start(100, false);
        // The timer must outlive this scope.
        std::mem::forget(timer);
    }

    // Auto-connect to the last used site.
    {
        let config = app.config.borrow();
        if let Some(site_url) = config.connection.last_used_site.clone() {
            if let Some(site) = config.connection.site(&site_url) {
                if !site.email.is_empty() && !site.password.is_empty() {
                    app.run.borrow_mut().connecting = true;
                    app.net.send(NetCommand::Connect {
                        site_url: site.url.clone(),
                        email: site.email.clone(),
                        password: site.password.clone(),
                    });
                }
            }
        }
    }

    frame.show(true);
    frame.centre();
}

fn build_menu(app: &Rc<App>, frame: &Frame) {
    let file_menu = Menu::builder()
        .append_item(
            ID_MENU_CONFIGURE,
            "&Configure Audio Pub...",
            "Manage Audio Pub sites and credentials",
        )
        .append_item(
            ID_MENU_STREAM_INFO,
            "&Set stream info...",
            "Title, description, and archiving for the stream",
        )
        .append_item(
            ID_MENU_PREFERENCES,
            "&Preferences...\tCtrl+,",
            "Application preferences",
        )
        .append_separator()
        .append_item(ID_MENU_EXIT, "E&xit\tAlt+F4", "Exit Pubsplash")
        .build();
    let help_menu = Menu::builder()
        .append_item(ID_MENU_ABOUT, "&About Pubsplash", "Version information")
        .append_item(
            ID_MENU_README,
            "Open &Readme",
            "Open the documentation in your browser",
        )
        .build();
    let menu_bar = MenuBar::builder()
        .append(file_menu, "&File")
        .append(help_menu, "&Help")
        .build();
    frame.set_menu_bar(menu_bar);

    let app = app.clone();
    let frame = frame.clone();
    frame.clone().on_menu_selected(move |event| {
        match event.get_id() {
            ID_MENU_CONFIGURE => connect_dialog::show(&app, &frame),
            ID_MENU_STREAM_INFO => {
                stream_info_dialog::show(&app, &frame);
            }
            ID_MENU_PREFERENCES => preferences::show(&app, &frame),
            ID_MENU_EXIT => {
                frame.close(false);
            }
            ID_MENU_ABOUT => {
                show_info(
                    &frame,
                    "About Pubsplash",
                    &format!(
                        "Pubsplash {}\n\nAn accessibility-first streaming app for Audio Pub.",
                        env!("CARGO_PKG_VERSION")
                    ),
                );
            }
            ID_MENU_README => {
                if let Err(e) = open_in_browser(README_URL) {
                    show_error(&frame, "Open Readme", &format!("Could not open browser: {e}"));
                }
            }
            _ => {}
        }
    });
}

fn open_in_browser(url: &str) -> std::io::Result<()> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map(|_| ())
}

/// Drains engine and network events into UI state.
fn pump_events(app: &Rc<App>) {
    let mut stream_ui_dirty = false;
    let mut chat_dirty = false;

    while let Ok(event) = app.net.events.try_recv() {
        match event {
            NetEvent::Connected { site_url } => {
                {
                    let mut run = app.run.borrow_mut();
                    run.connecting = false;
                    run.connected_site = Some(site_url.clone());
                }
                {
                    let mut config = app.config.borrow_mut();
                    config.connection.last_used_site = Some(site_url.clone());
                }
                app.save_config();
                stream_ui_dirty = true;
                if let Some(ui) = app.connect_ui.borrow().clone() {
                    ui.connect_button.set_label("Dis&connect");
                    show_info(
                        &ui.dialog,
                        "Connected",
                        &format!("Connected to {site_url} and logged in successfully."),
                    );
                    ui.connect_button.set_focus();
                }
            }
            NetEvent::ConnectFailed { message } => {
                app.run.borrow_mut().connecting = false;
                let connect_ui = app.connect_ui.borrow().clone();
                let text = format!("Could not connect: {message}");
                match connect_ui {
                    Some(ui) => {
                        show_error(&ui.dialog, "Connection failed", &text);
                        ui.connect_button.set_focus();
                    }
                    None => {
                        app.widgets(|w| show_error(&w.frame, "Connection failed", &text));
                    }
                }
            }
            NetEvent::Disconnected => {
                let mut run = app.run.borrow_mut();
                run.connected_site = None;
                run.connecting = false;
                drop(run);
                stream_ui_dirty = true;
                if let Some(ui) = app.connect_ui.borrow().clone() {
                    ui.connect_button.set_label("&Connect");
                    ui.connect_button.set_focus();
                }
            }
            NetEvent::StreamStarted { stream_id } => {
                let mut run = app.run.borrow_mut();
                run.stream = StreamState::Live { stream_id };
                run.stream_started = Some(Instant::now());
                run.listeners = 0;
                run.listener_peak = 0;
                drop(run);
                stream_ui_dirty = true;
            }
            NetEvent::StreamEnded => {
                app.engine.send(EngineCommand::StopEncoding);
                let mut run = app.run.borrow_mut();
                run.stream = StreamState::Idle;
                run.stream_started = None;
                drop(run);
                stream_ui_dirty = true;
            }
            NetEvent::StreamError { message } => {
                app.engine.send(EngineCommand::StopEncoding);
                let mut run = app.run.borrow_mut();
                run.stream = StreamState::Idle;
                run.stream_started = None;
                drop(run);
                stream_ui_dirty = true;
                app.widgets(|w| show_error(&w.frame, "Streaming problem", &message));
            }
            NetEvent::Chat(message) => {
                let user = message.user.display().to_string();
                speak_chat(app, &user, &message.content);
                app.run.borrow_mut().chat.push(ChatEntry {
                    user,
                    content: message.content,
                    received: Instant::now(),
                });
                chat_dirty = true;
            }
            NetEvent::Listeners { active, peak } => {
                let mut run = app.run.borrow_mut();
                run.listeners = active;
                run.listener_peak = peak.max(run.listener_peak);
                drop(run);
                stream_ui_dirty = true;
            }
            NetEvent::ChatSendFailed { message } => {
                app.widgets(|w| {
                    show_error(&w.frame, "Chat", &format!("Message not sent: {message}"))
                });
            }
        }
    }

    while let Ok(event) = app.engine.events.try_recv() {
        match event {
            crate::audio::EngineEvent::SourceError { name, message } => {
                log::error!("Source {name:?} failed: {message}");
            }
            crate::audio::EngineEvent::EncodingStopped => {}
            // The audio thread has swapped to the new bus set and no longer
            // references any plugin instances the UI retired; dropping them
            // here (the UI thread, per the hosting contract) is now safe.
            crate::audio::EngineEvent::BusesApplied => {
                app.fx.borrow_mut().retiring.clear();
            }
        }
    }

    pump_scan_events(app);
    fx_editor::pump(app);

    if stream_ui_dirty {
        app.refresh_stream_ui();
    }
    if chat_dirty {
        chat::refresh_chat_list(app);
    }
}

/// Drives a running VST scan: relays progress into the progress dialog,
/// forwards its Cancel button to the worker, and finishes or abandons the
/// scan. Events are collected under a short borrow first — handlers below
/// open modal dialogs, which must not happen while `app.scan` is borrowed.
fn pump_scan_events(app: &Rc<App>) {
    use crate::vst::scan::ScanEvent;
    use std::sync::atomic::Ordering;

    let events: Vec<ScanEvent> = match &*app.scan.borrow() {
        Some(ui) => ui.handle.events.try_iter().collect(),
        None => return,
    };

    for event in events {
        match event {
            ScanEvent::Started { total } => {
                let mut scan = app.scan.borrow_mut();
                if let Some(ui) = scan.as_mut() {
                    // Replace the indeterminate "looking for plugins" dialog
                    // with one whose range is the real total.
                    ui.progress = None;
                    ui.determinate = true;
                    if total > 0 {
                        ui.progress = Some(
                            ProgressDialog::builder(
                                &ui.parent,
                                "Scanning VST plugins",
                                &format!("Scanning {total} plugins..."),
                                total as i32,
                            )
                            .can_abort()
                            .can_skip()
                            .smooth()
                            .build(),
                        );
                    }
                }
            }
            ScanEvent::Progress {
                done,
                total,
                current,
            } => {
                let scan = app.scan.borrow();
                if let Some(ui) = scan.as_ref() {
                    if let Some(progress) = &ui.progress {
                        let (keep_going, skipped) = progress.update_with_skip(
                            done as i32,
                            Some(&format!("Scanned {done} of {total}: {current}")),
                        );
                        if !keep_going {
                            ui.handle.cancel.store(true, Ordering::Relaxed);
                        }
                        if skipped {
                            ui.handle.skip.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }
            ScanEvent::Finished {
                cache,
                found,
                rejected,
                skipped_other_arch,
                skipped_by_user,
            } => {
                let Some(ui) = app.scan.borrow_mut().take() else {
                    continue;
                };
                drop(ui.progress);
                crate::vst::save_cache(&cache);
                let total_known = cache.plugins.len();
                *app.plugins.borrow_mut() = cache;
                let mut message = format!(
                    "Scan complete. {found} new plugins found ({total_known} known in total)."
                );
                if rejected > 0 {
                    message.push_str(&format!("\n{rejected} files could not be used as plugins."));
                }
                if skipped_other_arch > 0 {
                    message.push_str(&format!(
                        "\n{skipped_other_arch} plugins were skipped because they are built for a different processor architecture (for example 32-bit)."
                    ));
                }
                if skipped_by_user > 0 {
                    message.push_str(&format!(
                        "\n{skipped_by_user} plugins were skipped at your request. Use \"Rescan all plugins\" to try them again."
                    ));
                }
                show_info(&ui.parent, "Scan complete", &message);
            }
            ScanEvent::Cancelled => {
                let Some(ui) = app.scan.borrow_mut().take() else {
                    continue;
                };
                drop(ui.progress);
                show_info(
                    &ui.parent,
                    "Scan cancelled",
                    "The scan was cancelled. Nothing was saved.",
                );
            }
        }
    }

    let scan = app.scan.borrow();
    if let Some(ui) = scan.as_ref() {
        if let Some(progress) = &ui.progress {
            // While enumerating, animate the indeterminate bar.
            if !ui.determinate && !progress.pulse(None) {
                ui.handle.cancel.store(true, Ordering::Relaxed);
            }
            // The Cancel button doesn't always surface through update()'s
            // return value between events; poll it so cancellation is prompt.
            if progress.was_cancelled() {
                ui.handle.cancel.store(true, Ordering::Relaxed);
            }
            // Skip must also work while the scanner is stuck inside one
            // plugin and no Progress events are flowing. was_skipped() stays
            // latched until an update consumes it; re-updating at the current
            // value resets the flag and re-enables the Skip button.
            if ui.determinate && progress.was_skipped() {
                ui.handle.skip.store(true, Ordering::Relaxed);
                let _ = progress.update_with_skip(progress.get_value(), None);
            }
        }
    }
}

#[cfg(test)]
mod token_tests {
    #[test]
    fn expands_title_and_url() {
        assert_eq!(
            super::expand_stream_tokens(
                "Now live: {title} - listen at {url}",
                "Tuesday hangout",
                "https://audiopub.site/live/abc123"
            ),
            "Now live: Tuesday hangout - listen at https://audiopub.site/live/abc123"
        );
        assert_eq!(super::expand_stream_tokens("no tokens", "t", "u"), "no tokens");
    }
}
