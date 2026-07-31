//! The wxDragon UI: main frame, tabs, menu bar, and the pump
//! timer that carries events from the audio/network threads onto the UI
//! thread.

mod api;
mod app_picker;
mod buses;
mod chat;
mod connect_dialog;
mod fx;
mod fx_editor;
mod fx_params;
mod help;
mod home;
mod keybinds;
mod keybinds_ui;
mod list;
mod native_acc;
mod panes;
mod preferences;
mod scenes;
mod sends;
mod slider_uia;
mod stream_info_dialog;

use crate::audio::{
    AudioEngine, EngineCommand, FeedKind, RoutingUpdate, SourceSpec, capture::CaptureKind,
};
use crate::config::{Config, SiteConfig, SourceConfig, SourceKindConfig, StreamingServiceType};
use crate::net::{NetCommand, NetEvent, NetHandle, ServiceProfile};
use crate::source_name::NameContext;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};
use wxdragon::prelude::*;

// wxWidgets key codes (not exported by wxdragon).
pub const WXK_TAB: i32 = 9;
pub const WXK_ESCAPE: i32 = 27;
pub const WXK_DELETE: i32 = 127;
pub const WXK_PAGEUP: i32 = 366;
pub const WXK_PAGEDOWN: i32 = 367;
pub const WXK_END: i32 = 312;
pub const WXK_HOME: i32 = 313;
pub const WXK_LEFT: i32 = 314;
pub const WXK_UP: i32 = 315;
pub const WXK_RIGHT: i32 = 316;
pub const WXK_DOWN: i32 = 317;

const ID_MENU_CONFIGURE: i32 = 2001;
const ID_MENU_PREFERENCES: i32 = 2002;
const ID_MENU_EXIT: i32 = 2003;
const ID_MENU_STREAM_INFO: i32 = 2004;
const ID_MENU_SOUND_PACK_MANAGER: i32 = 2005;
const ID_MENU_ABOUT: i32 = 2101;
const ID_MENU_README: i32 = 2102;
const ID_MENU_CHANGELOG: i32 = 2103;
/// Command id of the "Enable volume boost" item in a mixer slider's context
/// menu. One id serves every strip: the menu is popped up on the slider, so
/// the command comes back to that slider's own handler.
pub const ID_MIXER_BOOST: i32 = 2201;
/// Command id of the "Monitor this strip" item in the same menu.
pub const ID_MIXER_MONITOR: i32 = 2202;
/// Id worn by every dialog's confirm button, so that ENTER can reach it. See
/// `ok_button`, which is the only thing that should ever use it — and which
/// documents why it is this private id rather than `ID_OK`.
const ID_CONFIRM: i32 = 2301;

/// Where the Help menu goes when the copy installed with this build cannot be
/// found or will not open.
const README_URL: &str = "https://github.com/ironcross32/pubsplash#readme";
const CHANGELOG_URL: &str = "https://github.com/ironcross32/pubsplash/blob/master/changelog.md";

/// How long the exit will wait for the shutdown cue before giving up on it.
const SHUTDOWN_CUE_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// `"user: content"` with newlines flattened — the part of the list label
    /// that never changes. Built once, because the relative times are
    /// re-rendered once a second across the whole history.
    pub prefix: String,
    /// The relative time currently shown in this entry's list item, so that
    /// refresh can skip both the formatting and the FFI round-trip for the
    /// (overwhelmingly common) entries whose displayed age has not changed.
    pub shown_age: String,
}

impl ChatEntry {
    pub fn new(user: String, content: String) -> Self {
        let prefix = format!("{}: {}", user, content.replace(['\r', '\n'], " "));
        Self {
            user,
            content,
            received: Instant::now(),
            prefix,
            shown_age: String::new(),
        }
    }
}

/// Stream metadata sent to the server when a stream is created. Deliberately
/// not persisted: it resets to these defaults every launch.
#[derive(Clone)]
pub struct StreamInfo {
    pub title: String,
    pub description: String,
    pub archive: bool,
    pub record: bool,
}

impl Default for StreamInfo {
    fn default() -> Self {
        Self {
            title: "Stream".to_string(),
            description: "This is just a stream".to_string(),
            archive: false,
            record: false,
        }
    }
}

/// Mutable runtime state (not persisted).
pub struct Runtime {
    pub stream: StreamState,
    pub stream_started: Option<Instant>,
    pub connected_service: Option<String>,
    pub connecting: bool,
    pub listeners: u32,
    pub listener_peak: u32,
    pub listener_baseline: bool,
    pub chat: Vec<ChatEntry>,
    pub stream_info: StreamInfo,
    /// Whether the user has confirmed the stream info dialog this session.
    pub stream_info_set: bool,
    /// Whether a standalone (non-streaming) local recording is in progress.
    /// Drives the record button and its mutual exclusion with streaming; a
    /// recording running alongside a stream does *not* set it.
    pub recording: bool,
    /// When the current recording started, whichever way it was started —
    /// standalone or alongside a stream. The one signal for "a recording is
    /// underway", and the clock the overview list shows when not streaming.
    pub recording_started: Option<Instant>,
    /// Running processes matched to the active scene's Application sources,
    /// keyed by the configured process name (lowercased). Refreshed by the
    /// pump; drives both the labels and the pid an Application source captures.
    pub apps: HashMap<String, crate::audio::device::AppProcess>,
    /// Identity names (`SourceConfig.name`) of sources whose capture thread is
    /// currently failing and retrying. Drives the "(reconnecting)" labels.
    pub failing: HashSet<String>,
    /// The `tts::catalog::generation()` the source labels were last built from.
    /// A background voice refresh landing is the one thing that can change a
    /// label with no config edit and no process coming or going behind it, so
    /// the pump watches this to know when to re-derive them.
    pub tts_catalog_generation: u64,
    /// The `tts::usage::generation()` the API tab was last built from. Speech
    /// happens on worker threads, so the pump watches this rather than being
    /// told, exactly as it does for the catalog above.
    pub usage_generation: u64,
    /// Which mixer strips are being monitored through the local playback
    /// device. Deliberately not persisted — see [`Monitors`].
    pub monitors: Monitors,
    /// What `refresh_stream_ui` last wrote to the stream/record controls.
    pub shown: ShownStreamUi,
}

/// The last values `App::refresh_stream_ui` wrote to each control.
///
/// It runs once a second for the whole stream, and setting a label or an
/// enable state fires an MSAA change event whether or not anything changed —
/// so a screen-reader user parked on the stream button (the likeliest place to
/// be while streaming) heard it re-announced every second. Nothing is written
/// now unless it actually differs.
///
/// The comparison is against this cache and not a `get_label()` round-trip on
/// purpose: wx normalises the mnemonic ampersands, so a round-trip would never
/// compare equal and the guard would silently never fire.
#[derive(Default)]
pub struct ShownStreamUi {
    stream_label: String,
    stream_enabled: Option<bool>,
    record_label: String,
    record_enabled: Option<bool>,
    /// The rows currently in the overview list, with the text each one actually
    /// holds. Cached rather than read back through `get_string` because
    /// `home::refresh_overview` deliberately skips writing the Duration row
    /// while it is selected, and the cache is what makes that row still count
    /// as out of date on the next tick.
    pub overview: Vec<(home::OverviewRow, String)>,
    /// The rows currently in the API tab's usage list, for the same reason and
    /// with the same deliberate staleness as `overview` above — see
    /// `api::refresh`.
    pub api: Vec<(api::ApiRow, String)>,
    /// The stream phase and standalone-recording state last *spoken*. Separate
    /// from the label caches above because those change for reasons that are not
    /// a transition (the button enable rules), and because `None` marks the very
    /// first refresh, which seeds these silently rather than announcing at
    /// startup that nothing is happening.
    announced_stream: Option<StreamPhase>,
    announced_recording: Option<bool>,
}

/// The four states worth announcing, since `StreamState` also carries a stream
/// id that has nothing to do with what the user needs to hear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamPhase {
    Idle,
    Starting,
    Live,
    Stopping,
}

impl StreamPhase {
    fn of(state: &StreamState) -> Self {
        match state {
            StreamState::Idle => StreamPhase::Idle,
            StreamState::Starting => StreamPhase::Starting,
            StreamState::Live { .. } => StreamPhase::Live,
            StreamState::Stopping => StreamPhase::Stopping,
        }
    }

    /// What to say on arriving here from `previous`.
    fn announcement(&self, previous: StreamPhase) -> Option<&'static str> {
        match self {
            StreamPhase::Starting => Some("Connecting to the stream"),
            StreamPhase::Live => Some("Streaming started"),
            StreamPhase::Stopping => Some("Stopping the stream"),
            // Reaching idle from starting means the attempt failed, and
            // `NetEvent::StreamError` has already put a message box up saying
            // why — "Streaming stopped" on top of that would be noise.
            StreamPhase::Idle => match previous {
                StreamPhase::Live | StreamPhase::Stopping => Some("Streaming stopped"),
                _ => None,
            },
        }
    }
}

/// Which mixer strips are monitored, by the same indices the mixer and engine
/// use. Session-only on purpose: Pubsplash must never open the speakers (or
/// start feeding a microphone back at the room) at launch because of a choice
/// made days ago.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Monitors {
    pub master: bool,
    /// Indexed by position in the active scene's sources.
    pub sources: Vec<bool>,
    /// Indexed by position in `config.buses.buses`.
    pub buses: Vec<bool>,
}

impl Monitors {
    pub fn source(&self, index: usize) -> bool {
        self.sources.get(index).copied().unwrap_or(false)
    }

    pub fn bus(&self, index: usize) -> bool {
        self.buses.get(index).copied().unwrap_or(false)
    }

    fn set(flags: &mut Vec<bool>, index: usize, on: bool) {
        if flags.len() <= index {
            flags.resize(index + 1, false);
        }
        flags[index] = on;
    }

    pub fn remap_sources(&mut self, previous: &[SourceConfig], current: &[SourceConfig]) {
        self.sources = remapped_source_monitors(&self.sources, previous, current);
    }
}

fn remapped_source_monitors(
    flags: &[bool],
    previous: &[SourceConfig],
    current: &[SourceConfig],
) -> Vec<bool> {
    let mut previous_by_name = HashMap::new();
    for (index, source) in previous.iter().enumerate() {
        let name = source.name.as_str();
        if name.trim().is_empty() || previous_by_name.insert(name, index).is_some() {
            return Vec::new();
        }
    }

    let mut current_names = HashSet::new();
    let mut remapped = Vec::with_capacity(current.len());
    for source in current {
        let name = source.name.as_str();
        if name.trim().is_empty() || !current_names.insert(name) {
            return Vec::new();
        }
        remapped.push(
            previous_by_name
                .get(name)
                .and_then(|index| flags.get(*index))
                .copied()
                .unwrap_or(false),
        );
    }

    while remapped.last().copied() == Some(false) {
        remapped.pop();
    }
    remapped
}

#[cfg(test)]
mod monitor_tests {
    use super::Monitors;
    use crate::config::SourceConfig;

    fn source(name: &str) -> SourceConfig {
        SourceConfig {
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn source_remap_preserves_existing_sources_when_one_is_added() {
        let previous = vec![source("Microphone"), source("Text-to-Speech")];
        let current = vec![
            source("Microphone"),
            source("Text-to-Speech"),
            source("Application"),
        ];
        let mut monitors = Monitors {
            sources: vec![false, true],
            ..Default::default()
        };

        monitors.remap_sources(&previous, &current);

        assert!(!monitors.source(0));
        assert!(monitors.source(1));
        assert!(!monitors.source(2));
    }

    #[test]
    fn source_remap_follows_reordered_sources() {
        let previous = vec![
            source("Microphone"),
            source("Text-to-Speech"),
            source("Application"),
        ];
        let current = vec![
            source("Application"),
            source("Microphone"),
            source("Text-to-Speech"),
        ];
        let mut monitors = Monitors {
            sources: vec![true, false, true],
            ..Default::default()
        };

        monitors.remap_sources(&previous, &current);

        assert!(monitors.source(0));
        assert!(monitors.source(1));
        assert!(!monitors.source(2));
    }

    #[test]
    fn source_remap_drops_removed_sources() {
        let previous = vec![
            source("Microphone"),
            source("Text-to-Speech"),
            source("Application"),
        ];
        let current = vec![source("Microphone"), source("Application")];
        let mut monitors = Monitors {
            sources: vec![true, true, true],
            ..Default::default()
        };

        monitors.remap_sources(&previous, &current);

        assert!(monitors.source(0));
        assert!(monitors.source(1));
        assert!(!monitors.source(2));
    }

    #[test]
    fn source_remap_clears_ambiguous_source_names() {
        let mut monitors = Monitors {
            sources: vec![true],
            ..Default::default()
        };
        monitors.remap_sources(
            &[source("Microphone"), source("Microphone")],
            &[source("Microphone")],
        );
        assert!(monitors.sources.is_empty());

        let mut monitors = Monitors {
            sources: vec![true],
            ..Default::default()
        };
        monitors.remap_sources(&[source("Microphone")], &[source("")]);
        assert!(monitors.sources.is_empty());
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            stream: StreamState::Idle,
            stream_started: None,
            connected_service: None,
            connecting: false,
            listeners: 0,
            listener_peak: 0,
            listener_baseline: false,
            chat: Vec::new(),
            stream_info: StreamInfo::default(),
            stream_info_set: false,
            recording: false,
            recording_started: None,
            apps: HashMap::new(),
            failing: HashSet::new(),
            // Seeded from the catalog prewarmed before the UI was built, so the
            // labels start out agreeing with it and the first pump tick has no
            // phantom refresh to do.
            tts_catalog_generation: crate::tts::catalog::generation(),
            // Nothing has spoken yet, so this is 0 and the first pump tick has
            // nothing to redraw.
            usage_generation: crate::tts::usage::generation(),
            monitors: Monitors::default(),
            shown: ShownStreamUi::default(),
        }
    }
}

/// An MSAA accessible object that only supplies a name, leaving all other
/// behavior to the control's default accessibility.
struct NameOnlyAccessible(String);

impl wxdragon::accessible::AccessibleImpl for NameOnlyAccessible {
    /// Delegates the child count, which is what "name only" is supposed to mean
    /// for every method but this one.
    ///
    /// `AccessibleImpl::get_child_count` is the single trait method whose
    /// default body answers `WXD_ACC_OK` with 0 rather than
    /// `WXD_ACC_NOT_IMPLEMENTED`, and `Accessible::new` registers all eighteen
    /// callbacks unconditionally, so the "no callback, fall through" path in the
    /// C++ shim never runs. wxWidgets' `wxIAccessible::get_accChildCount` hands
    /// off to the control's standard `IAccessible` *only* on
    /// `wxACC_NOT_IMPLEMENTED` and takes any other status at face value, so
    /// without this the answer would be a flat 0 for every control.
    ///
    /// It happens to make no audible difference today, because everything still
    /// using this has no MSAA children and 0 was already the true answer:
    /// controls that do have children are list boxes, and those go through
    /// [`native_acc`] instead. Keep it anyway — the next control with children
    /// would otherwise inherit a silent lie.
    fn get_child_count(&self) -> (wxdragon::accessible::AccStatus, i32) {
        (wxdragon::ffi::wxd_AccStatus_WXD_ACC_NOT_IMPLEMENTED, 0)
    }

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
///
/// Not for list boxes: replacing wx's accessible leaves the control's MSAA
/// object split across two unrelated COM object graphs, which made every list
/// announce its selected row twice on focus. Lists use [`native_acc::install`],
/// which takes wx out of the loop entirely.
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
    /// The tab bar. Kept so [`panes`] can reach the current page and focus the
    /// tabs themselves.
    pub notebook: Notebook,
    pub overview: ListBox,
    pub stream_button: Button,
    pub record_button: Button,
    pub home_scene_list: ListBox,
    pub mixer_panel: Panel,
    /// The replaceable panel holding the current mixer strips.
    pub mixer_inner: RefCell<Option<Panel>>,
    /// The current mixer strips, in creation (and so Tab) order. Holding the
    /// widgets lets a strip be re-labelled in place — a full rebuild would move
    /// focus, and the app-detection tick re-labels exactly when the user is
    /// most likely reaching for that slider. Each strip's UIA provider must be
    /// uninstalled while its window still exists, so `rebuild_mixer` drains
    /// this before destroying `mixer_inner` (and so does the close handler).
    pub mixer_strips: RefCell<Vec<home::MixerStrip>>,
    pub home_panel: Panel,
    pub chat_list: ListBox,
    #[allow(dead_code)]
    pub chat_input: TextCtrl,
    pub scenes_list: ListBox,
    pub sources_list: ListBox,
    pub bus_list: ListBox,
    pub fx_list: ListBox,
    /// Reflects (and toggles) the selected plugin's bypass state.
    pub fx_bypass: CheckBox,
    /// Per-engine speech usage — see [`api`].
    pub usage_list: ListBox,
    #[allow(dead_code)]
    pub usage_refresh: Button,
}

/// Live handles into the Setup streaming services dialog while it is open, so
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
    pub speaker: crate::tts::speaker::Speaker,
    pub widgets: RefCell<Option<Widgets>>,
    pub connect_ui: RefCell<Option<ConnectUi>>,
    /// Plugins known from the last completed scan (vst_plugins.json).
    pub plugins: RefCell<crate::vst::PluginCache>,
    pub scan: RefCell<Option<ScanUi>>,
    /// Live plugin instances, mirroring `config.buses`.
    pub fx: RefCell<FxRuntime>,
    /// Plugin instances taken out of [`App::fx`] and not yet released. See
    /// [`fx::orphan`] — dropping one runs third-party teardown code that can
    /// re-enter the UI, so it must not happen under a `RefCell` borrow.
    pub orphaned_plugins: RefCell<Vec<Arc<crate::vst::PluginInstance>>>,
    /// Named FX chains saved to fx_chains.json.
    pub chain_library: RefCell<crate::fx::FxChainLibrary>,
    /// Open native plugin editor windows.
    pub open_editors: RefCell<Vec<fx_editor::EditorWindow>>,
    /// Set once the frame is closing. The pump timer keeps firing during the
    /// deferred frame teardown; without this guard its callback would run
    /// `pump_events` against already-destroyed widgets and crash (0xc0000005).
    pub shutting_down: std::cell::Cell<bool>,
    /// Set by [`App::save_config`], cleared by [`App::flush_config`] once the
    /// file has been written.
    pub config_dirty: std::cell::Cell<bool>,
    /// Set while `pump_events` is running, so a modal dialog it opens cannot
    /// re-enter it from the nested event loop.
    pub pumping: std::cell::Cell<bool>,
    /// Process snapshots from the worker started by
    /// [`App::request_app_processes`], and whether one is outstanding.
    pub apps_tx: crossbeam_channel::Sender<HashMap<String, crate::audio::device::AppProcess>>,
    pub apps_rx: crossbeam_channel::Receiver<HashMap<String, crate::audio::device::AppProcess>>,
    pub apps_pending: std::cell::Cell<bool>,
    /// Balance lookups that *failed*, from the worker
    /// `tts::usage::start_balance_refresh` spawns. Successes go straight into
    /// the usage store and are noticed through its generation counter; only the
    /// failures need carrying back, so the user who pressed the button learns
    /// why nothing changed.
    pub usage_tx: crossbeam_channel::Sender<crate::tts::usage::BalanceResult>,
    pub usage_rx: crossbeam_channel::Receiver<crate::tts::usage::BalanceResult>,
    /// The 100 ms pump timer. Owned here (not leaked) so `on_close` can stop it
    /// before the frame is destroyed: a running timer whose owner frame has been
    /// torn down keeps firing `WM_TIMER` into the freed frame handler and
    /// crashes inside wx's event dispatch (0xc0000005).
    pub pump_timer: RefCell<Option<Timer<Frame>>>,
    /// The 100 ms timer that services open plugin editors and a running plugin
    /// scan. Exists only while one of those does — see [`sync_fast_timer`].
    pub fast_timer: RefCell<Option<Timer<Frame>>>,
    /// Set while the shutdown cue is playing. `on_close` hides the frame and
    /// starts the cue on its own thread; the pump polls this and finishes the
    /// teardown once the cue is done, so the sound is never cut off by the
    /// process exiting and the UI never freezes waiting for it.
    pub shutdown_cue: RefCell<Option<ShutdownCue>>,
}

/// Handle on the in-flight shutdown cue thread.
pub struct ShutdownCue {
    done: Arc<AtomicBool>,
    /// Cap on the wait, so a wedged render device cannot hang exit.
    deadline: Instant,
}

/// The live plugin instances backing the FX chains, kept in lockstep with
/// `config.buses`. A `None` slot is a chain entry whose plugin is missing on
/// this machine (it processes as a gap). See `ui::fx` for the lifecycle.
#[derive(Default)]
pub struct FxRuntime {
    /// `buses[bus][slot]`, matching `config.buses.buses[bus].chain[slot]`.
    pub buses: Vec<Vec<Option<Arc<crate::vst::PluginInstance>>>>,
    /// Matching `config.buses.master_chain`.
    pub master: Vec<Option<Arc<crate::vst::PluginInstance>>>,
}

/// Plays an event through each enabled Sound Events source in the active scene.
///
/// Every source uses the one pack chosen on the Preferences "Sound packs" tab;
/// per-source pack selection is not exposed. The broadcaster always hears the cue locally
/// through `audio::cue`; `output_to_stream` only decides whether the same
/// samples are *also* fed to the mixer through `ExternalFeeds` so listeners hear
/// them. The local path bypasses the strip, so it honours the source's mute
/// (filtered below) but not its volume.
///
/// TTS reaches the same end by the opposite means: its samples always go to the
/// mixer, and the strip itself is played locally (`SourceSpec::local`), so the
/// fader applies to what the broadcaster hears. Cues are one-shot and short
/// enough that routing them through a strip to be heard would not be worth the
/// latency; a spoken chat message is neither.
fn play_sound_event(app: &Rc<App>, event: crate::soundpack::StreamEvent) {
    let targets: Vec<(String, bool)> = {
        let config = app.config.borrow();
        let Some(scene) = config.scenes.active_scene() else {
            return;
        };
        scene
            .sources
            .iter()
            .filter_map(|source| {
                let SourceKindConfig::SoundEvents(settings) = &source.kind else {
                    return None;
                };
                if source.muted || !sound_event_enabled(settings, event) {
                    return None;
                }
                Some((source.name.clone(), settings.output_to_stream))
            })
            .collect()
    };
    if targets.is_empty() {
        return;
    }
    let sound = crate::soundpack::SoundKind::from_stream_event(event);
    let Some(pack) = crate::soundpack::active() else {
        return;
    };
    for (source_name, to_stream) in targets {
        // Decoded on the pack's own cache, so a burst of chat messages costs
        // one WAV parse per variant rather than one per message. Picking the
        // variant here rather than inside each player means the copy the
        // broadcaster hears is the same one the listeners get.
        let Some(samples) = pack.random_decoded(sound) else {
            continue;
        };
        crate::audio::cue::play_samples_async(samples.clone());
        if to_stream {
            let feeds = app.engine.external_feeds.clone();
            std::thread::spawn(move || {
                feeds.feed_all(&source_name, &samples, "Sound events");
            });
        }
    }
}

fn sound_event_enabled(
    settings: &crate::config::SoundEventsSourceConfig,
    event: crate::soundpack::StreamEvent,
) -> bool {
    match event {
        crate::soundpack::StreamEvent::ListenerIncrease => settings.listener_increase,
        crate::soundpack::StreamEvent::ListenerDecrease => settings.listener_decrease,
        crate::soundpack::StreamEvent::ListenerPeakIncrease => settings.listener_peak_increase,
        crate::soundpack::StreamEvent::IncomingChat => settings.incoming_chat,
        crate::soundpack::StreamEvent::OutgoingChat => settings.outgoing_chat,
    }
}
/// Surfaces speech failures the workers reported, into the chat log.
///
/// Not a dialog: these arrive while chat is flowing, and a modal per failed
/// message would make a wrong API key unusable rather than merely annoying.
/// The chat list is where the user is already reading, it persists, and a
/// screen reader reaches it — and the worker has already rate-limited repeats
/// to one a minute, so this cannot flood.
/// Returns how many entries were added, so the caller refreshes the list.
fn report_speech_problems(app: &Rc<App>) -> usize {
    let problems = app.speaker.take_problems();
    if problems.is_empty() {
        return 0;
    }
    let mut run = app.run.borrow_mut();
    let added = problems.len();
    for problem in problems {
        run.chat.push(ChatEntry::new(
            "Speech".into(),
            format!(
                "{} could not speak: {}",
                crate::tts::engines::display_name(&problem.engine),
                problem.message
            ),
        ));
    }
    added
}

/// Whether a source's audio should reach the listeners.
///
/// Only TTS sources can answer no, and it is exactly what "Send speech to the
/// stream" means: the strip is dropped from master *and* its sends are dropped,
/// since a bus mixes into master whatever the source wanted. It has nothing to
/// say about whether the broadcaster hears the speech — they always do, through
/// `SourceSpec::local` — which is the whole difference from what this used to
/// mean.
fn tts_reaches_the_stream(source: &crate::config::SourceConfig) -> bool {
    match &source.kind {
        SourceKindConfig::Tts(tts) => tts.output_to_stream,
        _ => true,
    }
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
        app.speaker.speak(crate::tts::speaker::SpeakRequest {
            engine: tts.engine.clone(),
            synth: crate::tts::engine::SynthRequest {
                text: format!("{user}: {content}"),
                voice: tts.voice.clone(),
                rate: tts.rate,
                volume: tts.volume,
                pitch: tts.pitch,
                provider_settings: tts.provider_settings.clone(),
            },
            source_name: source.name.clone(),
            speech: config.speech.clone(),
        });
    }
}

impl App {
    pub fn widgets<R>(&self, f: impl FnOnce(&Widgets) -> R) -> Option<R> {
        self.widgets.borrow().as_ref().map(f)
    }

    /// Marks the config as needing to be written. The write itself happens on
    /// the next one-second pump tick (or at exit) — see [`App::flush_config`].
    ///
    /// Saving here directly used to mean serializing the whole config, every
    /// plugin's base64 state included, once per slider event: a mouse drag
    /// emits 50-200 of those and a held arrow key about 30 a second, so a
    /// couple of chunked plugins turned a fader move into megabytes a second of
    /// synchronous disk I/O on the UI thread. Nothing about it needed to be
    /// immediate: the engine is told about volume changes by its own command,
    /// separately and without waiting for this.
    pub fn save_config(&self) {
        self.config_dirty.set(true);
    }

    /// Writes the config if [`App::save_config`] asked for it.
    pub fn flush_config(&self) {
        if self.config_dirty.replace(false) {
            crate::config::save(&self.config.borrow());
        }
    }

    /// The state the derived labels for `sources` are built from. Takes the
    /// sources explicitly because the Sources list shows whichever scene is
    /// selected, which is not always the active one.
    pub fn name_context(&self, sources: &[crate::config::SourceConfig]) -> NameContext {
        let run = self.run.borrow();
        NameContext::build(sources, run.apps.clone(), run.failing.clone())
    }

    /// Re-enumerates the processes behind every scene's Application sources —
    /// every scene, because the Sources list shows whichever scene is selected,
    /// not the active one.
    ///
    /// Returns `(labels_changed, capture_changed)`: the first when any
    /// displayed application name would differ, the second when a source in the
    /// *active* scene would now capture a different process (including starting
    /// or stopping capture entirely), which is what makes a re-sync worth its
    /// cost — `SetSources` respawns every capture thread.
    /// Starts the enumeration off the UI thread. Returns `Some` only when it
    /// could answer without enumerating at all; the result of a real
    /// enumeration arrives via [`App::apply_app_processes`].
    ///
    /// Enumerating is a whole-system process snapshot, and it used to run
    /// synchronously here. `device.rs`'s own budget for it is 50 ms — squarely
    /// in the range a screen-reader user feels, since NVDA's speech pump goes
    /// through the foreground window's message queue. Every two seconds, that
    /// is a periodic hitch while arrowing the mixer.
    pub fn request_app_processes(&self) -> Option<(bool, bool)> {
        let all_names = self.application_source_names(false);
        if all_names.is_empty() {
            let mut run = self.run.borrow_mut();
            let had_any = !run.apps.is_empty();
            run.apps.clear();
            return Some((had_any, had_any));
        }
        // One at a time: overlapping snapshots would contend for the shared
        // `System`, and the later answer could be the older one.
        if self.apps_pending.replace(true) {
            return None;
        }
        let sender = self.apps_tx.clone();
        std::thread::spawn(move || {
            let _ = sender.send(crate::audio::device::resolve_apps(&all_names));
        });
        None
    }

    /// Picks up a finished enumeration, if one has arrived.
    ///
    /// Driven by arrival rather than by the request on purpose: acting at
    /// request time would compare against a `run.apps` the snapshot never saw.
    pub fn apply_app_processes(&self) -> (bool, bool) {
        let Ok(apps) = self.apps_rx.try_recv() else {
            return (false, false);
        };
        self.apps_pending.set(false);
        self.absorb_apps(apps)
    }

    /// Enumerates synchronously.
    ///
    /// For one-off, user-initiated edits, whose very next line reads
    /// `run.apps` — adding a source has to know the pid before it can sync the
    /// engine, and paying 50 ms once on an explicit action is not the problem.
    /// The *periodic* poll goes through [`App::request_app_processes`].
    pub fn refresh_app_processes(&self) -> (bool, bool) {
        let all_names = self.application_source_names(false);
        if all_names.is_empty() {
            let mut run = self.run.borrow_mut();
            let had_any = !run.apps.is_empty();
            run.apps.clear();
            return (had_any, had_any);
        }
        self.absorb_apps(crate::audio::device::resolve_apps(&all_names))
    }

    /// Stores a fresh snapshot, reporting what it changed.
    fn absorb_apps(&self, apps: HashMap<String, crate::audio::device::AppProcess>) -> (bool, bool) {
        // Read the active scene now, not when the snapshot was requested: the
        // user may have switched scenes in between.
        let active_names = self.application_source_names(true);
        let mut run = self.run.borrow_mut();
        if apps == run.apps {
            return (false, false);
        }
        let capture_changed = active_names.iter().any(|name| {
            let key = name.trim().to_ascii_lowercase();
            apps.get(&key).map(|a| a.pid) != run.apps.get(&key).map(|a| a.pid)
        });
        run.apps = apps;
        (true, capture_changed)
    }

    /// The configured process names of Application sources, in the active
    /// scene only or across every scene.
    fn application_source_names(&self, active_only: bool) -> Vec<String> {
        let names = |sources: &[crate::config::SourceConfig]| -> Vec<String> {
            sources
                .iter()
                .filter_map(|s| match &s.kind {
                    SourceKindConfig::Application { process_name } => Some(process_name.clone()),
                    _ => None,
                })
                .collect()
        };
        let config = self.config.borrow();
        if active_only {
            config
                .scenes
                .active_scene()
                .map(|scene| names(&scene.sources))
                .unwrap_or_default()
        } else {
            config
                .scenes
                .scenes
                .iter()
                .flat_map(|scene| names(&scene.sources))
                .collect()
        }
    }

    /// Forgets which sources were being monitored. Use this when the active
    /// scene changes, because the new scene's strips only share positions by
    /// coincidence.
    pub fn clear_source_monitors(&self) {
        self.run.borrow_mut().monitors.sources.clear();
    }

    /// Carries source monitoring from one active-scene source list to another.
    ///
    /// Source monitoring is session-only and indexed in the engine, but
    /// `SourceConfig.name` is the stable identity within a scene. Remapping here
    /// keeps edits and reorders attached to the intended strip while dropping
    /// deleted or ambiguous sources.
    pub fn remap_source_monitors(&self, previous_sources: &[SourceConfig]) {
        let current_sources = {
            let config = self.config.borrow();
            config.scenes.active_scene().map(|s| s.sources.clone())
        };
        let Some(current_sources) = current_sources else {
            self.clear_source_monitors();
            return;
        };
        self.run
            .borrow_mut()
            .monitors
            .remap_sources(previous_sources, &current_sources);
    }

    /// The bus equivalent of [App::clear_source_monitors].
    pub fn clear_bus_monitors(&self) {
        self.run.borrow_mut().monitors.buses.clear();
    }

    /// Sends the active scene's sources to the audio engine (mixer order).
    /// Send targets are translated from bus names to current bus indices,
    /// so call this again after any bus reorder — or better, use
    /// [`App::sync_engine_routing`], which carries both in one command.
    pub fn sync_engine_sources(&self) {
        let Some(specs) = self.source_specs() else {
            return;
        };
        self.engine
            .send(EngineCommand::SetRouting(Box::new(RoutingUpdate {
                sources: Some(specs),
                ..Default::default()
            })));
        self.send_master_levels();
    }

    /// Sends sources, buses and the master chain together. A bus reorder shifts
    /// the indices the sources' sends are addressed by, so the two halves have
    /// to reach the mixer in the same command or a block can be routed through
    /// the wrong bus.
    pub fn sync_engine_routing(self: &Rc<Self>) {
        let (buses, master_chain) = fx::routing_specs(self);
        self.engine
            .send(EngineCommand::SetRouting(Box::new(RoutingUpdate {
                sources: self.source_specs(),
                buses: Some(buses),
                master_chain: Some(master_chain),
            })));
        self.send_master_levels();
    }

    fn send_master_levels(&self) {
        let config = self.config.borrow();
        self.engine
            .send(EngineCommand::SetMasterVolume(config.audio.master_volume));
        self.engine
            .send(EngineCommand::SetMasterMute(config.audio.master_muted));
    }

    /// The active scene's sources as engine specs, or `None` when there is no
    /// active scene (in which case the engine keeps what it has).
    fn source_specs(&self) -> Option<Vec<SourceSpec>> {
        let (apps, monitors) = {
            let run = self.run.borrow();
            (run.apps.clone(), run.monitors.clone())
        };
        let config = self.config.borrow();
        let scene = config.scenes.active_scene()?;
        let bus_index = |name: &str| config.buses.buses.iter().position(|b| b.name == name);
        let specs: Vec<SourceSpec> = scene
            .sources
            .iter()
            .enumerate()
            .map(|(index, s)| SourceSpec {
                name: s.name.clone(),
                volume: s.volume,
                muted: s.muted,
                monitor: monitors.source(index),
                // Speech is for the broadcaster first, so a TTS strip is always
                // played out of the local device — see `SourceSpec::local`.
                local: matches!(&s.kind, SourceKindConfig::Tts(_)),
                to_master: s.to_master && tts_reaches_the_stream(s),
                // Sends go too when speech is off the stream. A bus mixes into
                // master unconditionally, so leaving them would have put the
                // speech back on the stream by another route.
                sends: if tts_reaches_the_stream(s) {
                    s.sends
                        .iter()
                        .filter_map(|send| {
                            Some(crate::audio::SendSpec {
                                bus_index: bus_index(&send.bus)?,
                                level: send.level,
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                },
                feed: match &s.kind {
                    SourceKindConfig::Microphone { device_id } => {
                        FeedKind::Capture(CaptureKind::Microphone {
                            device_id: device_id.clone(),
                        })
                    }
                    SourceKindConfig::DesktopAudio => FeedKind::Capture(CaptureKind::DesktopAudio),
                    SourceKindConfig::Application { process_name } => {
                        match apps.get(&process_name.trim().to_ascii_lowercase()) {
                            Some(app) => {
                                FeedKind::Capture(CaptureKind::Application { pid: app.pid })
                            }
                            None => {
                                log::warn!(
                                    "Process {process_name:?} not running; source will be silent"
                                );
                                FeedKind::External
                            }
                        }
                    }
                    SourceKindConfig::Tts(_) | SourceKindConfig::SoundEvents(_) => {
                        FeedKind::External
                    }
                },
            })
            .collect();
        Some(specs)
    }

    pub fn is_streaming_or_starting(&self) -> bool {
        !matches!(self.run.borrow().stream, StreamState::Idle)
    }

    /// Whether the overview list has a clock that needs re-rendering every
    /// second. A standalone recording has one even though nothing is streaming.
    pub fn overview_ticking(&self) -> bool {
        let run = self.run.borrow();
        !matches!(run.stream, StreamState::Idle) || run.recording_started.is_some()
    }

    /// The public page of the current live stream, once it is live.
    #[allow(dead_code)]
    pub fn stream_url(&self) -> Option<String> {
        let run = self.run.borrow();
        let StreamState::Live { stream_id } = &run.stream else {
            return None;
        };
        let site = run.connected_service.as_deref()?;
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
        self.engine.send(EngineCommand::StopRecording);
        self.run.borrow_mut().recording_started = None;
        self.net.send(NetCommand::StopStream);
        self.refresh_stream_ui();
    }

    /// Starts a standalone local recording (no streaming). The file name is
    /// timestamped (see `recording_filename`); does not prompt.
    pub fn start_recording(&self) {
        {
            let run = self.run.borrow();
            if run.recording || !matches!(run.stream, StreamState::Idle) {
                return;
            }
        }
        let (bitrate, path) = {
            let config = self.config.borrow();
            let desired = config.archiving.recording_dir().join(recording_filename());
            (
                config.audio.bitrate_kbps,
                crate::audio::recorder::unique_path(&desired),
            )
        };
        self.engine.send(EngineCommand::StartRecording {
            bitrate_kbps: bitrate,
            path,
        });
        {
            let mut run = self.run.borrow_mut();
            run.recording = true;
            run.recording_started = Some(Instant::now());
        }
        self.refresh_stream_ui();
    }

    pub fn stop_recording(&self) {
        {
            let mut run = self.run.borrow_mut();
            if !run.recording {
                return;
            }
            run.recording = false;
            run.recording_started = None;
        }
        self.engine.send(EngineCommand::StopRecording);
        self.refresh_stream_ui();
    }

    /// Repaints everything that depends on stream state: overview list and the
    /// stream/record buttons.
    pub fn refresh_stream_ui(&self) {
        let run = self.run.borrow();

        let button_label = match &run.stream {
            StreamState::Idle => "&Start streaming",
            _ => "S&top streaming",
        };

        let streaming_or_starting = !matches!(run.stream, StreamState::Idle);
        let phase = StreamPhase::of(&run.stream);
        let recording = run.recording;
        let record_label = if recording {
            "Stop re&cording"
        } else {
            "Start &recording"
        };
        drop(run);

        self.widgets(|w| {
            // Every write below fires an accessibility change event, so each
            // one is guarded — see [`ShownStreamUi`].
            let mut run = self.run.borrow_mut();
            let shown = &mut run.shown;
            if shown.stream_label != button_label {
                w.stream_button.set_label(button_label);
                shown.stream_label = button_label.to_string();
            }
            // Streaming and standalone recording are mutually exclusive.
            if shown.stream_enabled != Some(!recording) {
                w.stream_button.enable(!recording);
                shown.stream_enabled = Some(!recording);
            }
            if shown.record_label != record_label {
                w.record_button.set_label(record_label);
                shown.record_label = record_label.to_string();
            }
            if shown.record_enabled != Some(!streaming_or_starting) {
                w.record_button.enable(!streaming_or_starting);
                shown.record_enabled = Some(!streaming_or_starting);
            }
        });
        // Outside the closure above: it holds a borrow of `run`, and this takes
        // its own.
        home::refresh_overview(self);

        // Speak the transitions. This is the one funnel every one of them passes
        // through — the start/stop methods call it directly and every `NetEvent`
        // arm reaches it via `stream_ui_dirty` — so the guard below is what keeps
        // the once-a-second refresh from repeating itself. It matters most when
        // the change came from a keybinding pressed on another tab, or from the
        // server ending the stream, with nothing on screen to notice.
        let lines = {
            let mut run = self.run.borrow_mut();
            let shown = &mut run.shown;
            let stream_line = match shown.announced_stream.replace(phase) {
                Some(previous) if previous != phase => phase.announcement(previous),
                // First refresh: seed silently rather than narrate the startup
                // state nobody asked about.
                _ => None,
            };
            let record_line = match shown.announced_recording.replace(recording) {
                Some(previous) if previous != recording => Some(if recording {
                    "Recording started"
                } else {
                    "Recording stopped"
                }),
                _ => None,
            };
            [stream_line, record_line]
        };
        for line in lines.into_iter().flatten() {
            help::announce(line);
        }
    }
}

/// Builds a recording file name from the current local date/time:
/// `recording_<yyyy-mm-dd>_<HH-MM-SS>.mp3`. The prefix is always the literal
/// word "recording" so files sort together regardless of the stream title.
fn recording_filename() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let t = unsafe { GetLocalTime() };
    format!(
        "recording_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.mp3",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

pub fn service_profile_from_site(site: &SiteConfig) -> Result<ServiceProfile, String> {
    let nickname = site.display_name();
    match site.service_type {
        StreamingServiceType::Audiopub => {
            let site_url = site.url.trim();
            if site_url.is_empty() || !site_url.starts_with("http") {
                return Err("Enter a full Audiopub URL starting with http(s)://".to_string());
            }
            if site.email.trim().is_empty() || site.password.is_empty() {
                return Err("Enter your email and password first.".to_string());
            }
            Ok(ServiceProfile::Audiopub {
                id: site.id.clone(),
                nickname,
                site_url: site_url.to_string(),
                email: site.email.trim().to_string(),
                password: site.password.clone(),
            })
        }
        StreamingServiceType::Icecast => {
            if site.icecast_server.trim().is_empty() {
                return Err("Enter the Icecast server.".to_string());
            }
            if site.icecast_port == 0 {
                return Err("Enter a valid Icecast port.".to_string());
            }
            if site.icecast_mount.trim().trim_start_matches('/').is_empty() {
                return Err("Enter the Icecast mount point.".to_string());
            }
            if site.icecast_password.is_empty() {
                return Err("Enter the Icecast password.".to_string());
            }
            Ok(ServiceProfile::Icecast {
                id: site.id.clone(),
                nickname,
                server: site.icecast_server.trim().to_string(),
                port: site.icecast_port,
                mount: site.icecast_mount.trim().to_string(),
                username: site.icecast_username(),
                password: site.icecast_password.clone(),
            })
        }
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
        if run.connected_service.is_none() {
            drop(run);
            app.widgets(|w| {
                show_error(
                    &w.frame,
                    "Not connected",
                    "Connect to a streaming service first (File > Setup streaming services).",
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
    // Bounded, at roughly two seconds of encoded audio. Unbounded, a stalled
    // TCP send window meant the queue grew at the encoded bitrate for as long
    // as the stall lasted, silently — and for a live stream, minutes of
    // buffered audio is worse than a gap.
    let (tx, rx) = tokio::sync::mpsc::channel(200);
    let bitrate = app.config.borrow().audio.bitrate_kbps;
    app.engine.send(EngineCommand::StartEncoding {
        bitrate_kbps: bitrate,
        out: tx,
    });
    if info.record {
        let desired = app
            .config
            .borrow()
            .archiving
            .recording_dir()
            .join(recording_filename());
        // Guard against clobbering a prior recording that resolved to the same
        // name (a stop/start within the same one-second timestamp).
        let path = crate::audio::recorder::unique_path(&desired);
        app.engine.send(EngineCommand::StartRecording {
            bitrate_kbps: bitrate,
            path,
        });
        // Not `run.recording`: that flag means a *standalone* recording and
        // disables the stream button. This is the clock and the overview's
        // "a recording is underway" signal.
        app.run.borrow_mut().recording_started = Some(Instant::now());
    }
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

/// Builds a dialog's confirm button as the **default item** — the one ENTER fires.
///
/// The real wx id is load-bearing, and the two keys get there by different routes.
/// Escape maps to the cancel id on its own, but Enter reaches a button only through
/// the dialog's *default item*, and that is a native mechanism (`DM_SETDEFID`) which
/// needs a real control id: builders leave the id at `ID_ANY`, so wx hands out a
/// negative auto-id and `set_default()` alone silently does nothing — verified, Enter
/// produced no event at all. `with_id(ID_OK)` plus `set_default()` is what makes it
/// work, and every hand-built dialog in the app goes through here (or through
/// `dismiss_button`) so that neither half can be forgotten.
///
/// The button is returned so the caller still binds its own `on_click` — validation
/// that refuses to close lives there, and Enter routes through it like a click does.
///
/// The id is deliberately **`ID_CONFIRM`, not `ID_OK`**, and that is load-bearing for
/// the validating dialogs. Any real id satisfies `DM_SETDEFID`, but `ID_OK` is one wx
/// answers itself: `wxDialogBase`'s table binds `EVT_BUTTON(wxID_OK)` to a handler that
/// calls `EndModal(wxID_OK)`. A click is a command event and propagates from the button
/// up to the dialog unless a handler consumes it, and wxdragon's trampoline
/// (`wxdragon-sys/cpp/src/event.cpp`) calls `event.Skip(true)` *before* each closure, so
/// a closure that does not call `skip(false)` leaves it propagating. Under `ID_OK` the
/// dialog would then close itself behind handlers that deliberately `return` without
/// closing — the shortcut-conflict and empty-nickname paths in `keybinds_ui` and
/// `connect_dialog` — and the refusal would flash an error and confirm anyway. With an
/// id wx has no handler for, the event propagates to the dialog, finds nothing, and
/// stops; no call site has to remember to consume it.
pub fn ok_button(parent: &dyn WxWidget, label: &str) -> Button {
    let button = Button::builder(parent)
        .with_id(ID_CONFIRM)
        .with_label(label)
        .build();
    button.set_default();
    button
}

/// The same, for a dismiss-only dialog whose single button is both OK and Cancel.
///
/// It keeps `ID_CANCEL` — that is the id wx maps Escape to, and it is what the
/// `end_modal` on these dialogs reports — and `set_default()` brings Enter to the
/// same button, so both keys close the dialog. Unlike `ok_button` the real id is
/// wanted here: none of these dialogs refuses to close, so `wxDialogBase`'s own
/// `wxID_CANCEL` handler doing the same thing a second time changes nothing.
pub fn dismiss_button(parent: &dyn WxWidget, label: &str) -> Button {
    let button = Button::builder(parent)
        .with_id(ID_CANCEL)
        .with_label(label)
        .build();
    button.set_default();
    button
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
    let api_panel = Panel::builder(&notebook).build();
    notebook.add_page(&home_panel, "Home", true, None);
    notebook.add_page(&chat_panel, "Chat", false, None);
    notebook.add_page(&scenes_panel, "Scenes and Sources", false, None);
    notebook.add_page(&buses_panel, "Buses", false, None);
    // Last in the strip: read-only, and nothing here is needed to get a stream
    // on the air.
    notebook.add_page(&api_panel, "API", false, None);
    help::tag(&notebook, "window.tabBar", "Main tab bar");

    let frame_sizer = BoxSizer::builder(Orientation::Vertical).build();
    frame_sizer.add(&notebook, 1, SizerFlag::Expand | SizerFlag::All, 0);
    frame.set_sizer(frame_sizer, true);

    build_menu(&app, &frame);

    // Tabs fill in the Widgets struct.
    let (overview, stream_button, record_button, home_scene_list, mixer_panel) =
        home::build(&app, &home_panel);
    let (chat_list, chat_input) = chat::build(&app, &chat_panel);
    let (scenes_list, sources_list) = scenes::build(&app, &scenes_panel);
    let (bus_list, fx_list, fx_bypass) = buses::build(&app, &buses_panel);
    let (usage_list, usage_refresh) = api::build(&app, &api_panel);

    *app.widgets.borrow_mut() = Some(Widgets {
        frame: frame.clone(),
        notebook: notebook.clone(),
        overview,
        stream_button,
        record_button,
        home_scene_list,
        mixer_panel,
        mixer_inner: RefCell::new(None),
        mixer_strips: RefCell::new(Vec::new()),
        home_panel: home_panel.clone(),
        chat_list,
        chat_input,
        scenes_list,
        sources_list,
        bus_list,
        fx_list,
        fx_bypass,
        usage_list,
        usage_refresh,
    });

    // Instantiate the configured FX chains before syncing the engine; collect
    // any slot that could not be filled for a single summary.
    let failures = fx::instantiate_all(&app);

    // Populate dynamic content now that widgets exist. Application sources are
    // resolved first so the very first labels name the running apps.
    app.refresh_app_processes();
    home::refresh_scene_list(&app);
    home::rebuild_mixer(&app);
    scenes::refresh_scenes_list(&app);
    scenes::refresh_sources_list(&app);
    buses::refresh_bus_list(&app);
    buses::refresh_fx_list(&app);
    api::refresh_usage(&app);
    app.refresh_stream_ui();
    // Buses before sources: sources reference buses by index.
    fx::sync_engine_buses(&app);
    app.sync_engine_sources();

    if !failures.is_empty() {
        // Two different problems with two different answers: install the
        // plugin, or find out why the one you have would not start.
        let (uninstalled, failed): (Vec<_>, Vec<_>) = failures
            .iter()
            .partition(|f| f.error == fx::SlotError::NotInstalled);
        let mut message = String::new();
        if !uninstalled.is_empty() {
            message.push_str(
                "Some plugins used by your buses are not installed on this machine and will be skipped until you install them and rescan:\n",
            );
            for failure in &uninstalled {
                message.push_str(&format!("\n- {}", failure.describe()));
            }
        }
        if !failed.is_empty() {
            if !message.is_empty() {
                message.push_str("\n\n");
            }
            message.push_str(
                "These plugins are installed but could not be loaded, so they will be skipped:\n",
            );
            for failure in &failed {
                message.push_str(&format!("\n- {}", failure.describe()));
            }
        }
        show_info(&frame, "Plugins skipped", &message);
    }

    // Exit confirmation while streaming (menu Exit and ALT+F4 both arrive here).
    {
        let app = app.clone();
        let frame_for_close = frame.clone();
        frame.on_close(move |event| {
            // Already sequencing an exit: keep the frame alive for the cue
            // rather than restarting (or short-circuiting) the teardown.
            if app.shutting_down.get() {
                if let WindowEventData::General(e) = &event {
                    e.veto();
                }
                return;
            }
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
            // Flush any recording. Standalone recording is not covered by the
            // streaming check above, and without this the encoder's flush and
            // the writer's tail never run, truncating the file the user was
            // making. (Falling back to `AudioEngine::drop` would not do it:
            // `App` is an `Rc` held by every event closure and may never drop.)
            app.stop_recording();
            // Mark the exit as under way. The pump uses this to stop touching
            // widgets, and from here on it drives the rest of the teardown.
            app.shutting_down.set(true);
            // Close plugin editors (and remove the keyboard hook) before the
            // main frame goes away.
            fx_editor::close_all(&app);
            // Vanish immediately: the user asked to exit, so the app should
            // look gone while the cue finishes in the background.
            frame_for_close.show(false);
            app.save_config();
            app.flush_config();
            start_shutdown_cue(&app);
            // The frame has to outlive the cue, so this close does not proceed.
            // `finish_close` destroys the frame once the cue is done.
            if let WindowEventData::General(e) = &event {
                e.veto();
            }
        });
    }

    // Context-sensitive help: capture the frame's UIA provider for spoken
    // announcements and install the app-wide F1 hook.
    help::install_announcer(&frame);
    help::install_hook();
    // The same hook dispatches user keybindings; give it the table to match on.
    keybinds::reload(&app.config.borrow());

    // The pump has two halves.
    //
    // Events from the engine and network threads arrive on **idle**: each
    // sender rings `wake_up_idle` after posting, so a chat message reaches
    // `speak_chat` as soon as it lands instead of waiting out an average half
    // a timer period. For an app whose whole point is that a blind broadcaster
    // hears their chat, that queueing delay was a real cost.
    //
    // The timer keeps only what genuinely needs a clock — elapsed durations,
    // relative timestamps, the deferred config write, and asking after running
    // applications — and so runs once a second rather than ten times.
    {
        let app = app.clone();
        frame.on_idle(move |_| {
            if app.shutting_down.get() {
                // The frame and its widgets are being torn down; touching them
                // would be a use-after-free. The only job left is finishing the
                // exit once the shutdown cue has played out.
                if shutdown_cue_finished(&app) {
                    finish_close(&app);
                }
                return;
            }
            pump_events(&app);
        });
    }
    {
        let app_for_timer = app.clone();
        let app = app.clone();
        let timer = Timer::new(&frame);
        let mut ticks: u32 = 0;
        timer.on_tick(move |_| {
            if app.shutting_down.get() {
                if shutdown_cue_finished(&app) {
                    finish_close(&app);
                }
                return;
            }
            // Durations, relative chat times, and the config write that slider
            // and text edits deferred to here.
            if app.overview_ticking() {
                app.refresh_stream_ui();
            }
            chat::refresh_chat_times(&app);
            app.flush_config();

            ticks = ticks.wrapping_add(1);
            if ticks % 2 == 0 {
                // Every two seconds: ask for a fresh look at which applications
                // are running, so their strips say which app they are and (once
                // running) actually capture it. The enumeration happens on a
                // worker; only the "nothing to enumerate" case answers here.
                if let Some(changes) = app.request_app_processes() {
                    apply_app_changes(&app, changes);
                }
            }
            // A snapshot may have finished since the last tick.
            let changes = app.apply_app_processes();
            apply_app_changes(&app, changes);
            // A backstop for the fast timer, in case an idle never followed the
            // transition that should have started or stopped it.
            sync_fast_timer(&app);
        });
        timer.start(1000, false);
        // Own the timer via App so `on_close` can stop it before teardown.
        // (Leaking it here would keep it firing into the destroyed frame.)
        *app_for_timer.pump_timer.borrow_mut() = Some(timer);
    }

    // Auto-connect to the last used service.
    {
        let config = app.config.borrow();
        if let Some(service_id) = config.connection.last_used_site.clone() {
            if let Some(site) = config.connection.site(&service_id) {
                match service_profile_from_site(site) {
                    Ok(profile) => {
                        app.run.borrow_mut().connecting = true;
                        app.net.send(NetCommand::Connect { profile });
                    }
                    Err(message) => log::warn!("Skipping auto-connect: {message}"),
                }
            }
        }
    }

    frame.show(true);
    frame.centre();
}

/// Plays the shutdown cue on its own thread and records a handle on it, so the
/// pump can tell when the sound has actually finished. With the cue turned off
/// in Preferences nothing is recorded, and the exit is not delayed at all.
fn start_shutdown_cue(app: &Rc<App>) {
    if !app.config.borrow().sounds.play_shutdown {
        return;
    }
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let spawned = std::thread::Builder::new()
        .name("shutdown-sound-cue".into())
        .spawn(move || {
            if let Err(e) =
                crate::audio::cue::play_sound_kind_blocking(crate::soundpack::SoundKind::Shutdown)
            {
                log::warn!("Could not play shutdown sound cue: {e}");
            }
            flag.store(true, Ordering::SeqCst);
            // Ring the doorbell: the frame is already hidden, so no input will
            // arrive to wake the idle handler that finishes the exit.
            wxdragon::wake_up_idle();
        })
        .is_ok();
    if spawned {
        *app.shutdown_cue.borrow_mut() = Some(ShutdownCue {
            done,
            deadline: Instant::now() + SHUTDOWN_CUE_TIMEOUT,
        });
    }
}

/// True once the shutdown cue is done, its deadline has passed, or there is no
/// cue at all (a failed spawn must not strand the app in a hidden window).
fn shutdown_cue_finished(app: &Rc<App>) -> bool {
    match app.shutdown_cue.borrow().as_ref() {
        Some(cue) => cue.done.load(Ordering::SeqCst) || Instant::now() >= cue.deadline,
        None => true,
    }
}

/// The second half of the exit, run from the pump once the cue has finished.
fn finish_close(app: &Rc<App>) {
    // Stop the pump before the frame is torn down: a running timer whose owner
    // frame has been destroyed keeps firing WM_TIMER into the freed handler.
    // (Stopped, not dropped: this runs from inside the timer's own callback.)
    if let Some(timer) = app.pump_timer.borrow().as_ref() {
        timer.stop();
    }
    if let Some(timer) = app.fast_timer.borrow().as_ref() {
        timer.stop();
    }
    app.shutdown_cue.borrow_mut().take();
    // Remove the F1 hook and drop the help announcer provider.
    help::uninstall_hook();
    help::uninstall_announcer();
    // Same for the mixer sliders' providers, while their windows live.
    home::drop_mixer_strips(app);
    let frame = app.widgets.borrow().as_ref().map(|w| w.frame.clone());
    if let Some(frame) = frame {
        // Destroy explicitly (deferred, wx-managed) rather than skipping to
        // the platform default. On the native ALT+F4 path, skipping hands the
        // WM_CLOSE to DefWindowProc, which destroys the window *synchronously*
        // and joins the engine/net threads from inside the window procedure -
        // a shutdown access violation. This routes ALT+F4 through the same
        // deferred teardown that File > Exit already uses.
        frame.destroy();
    }
}

fn build_menu(app: &Rc<App>, frame: &Frame) {
    let file_menu = Menu::builder()
        .append_item(
            ID_MENU_CONFIGURE,
            "Setup streaming &services...",
            "Manage Audiopub and Icecast streaming services",
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
    let tools_menu = Menu::builder()
        .append_item(
            ID_MENU_SOUND_PACK_MANAGER,
            "Sound Pack &Manager...",
            "Create and compile Pubsplash sound packs",
        )
        .build();
    let help_menu = Menu::builder()
        .append_item(ID_MENU_ABOUT, "&About Pubsplash", "Version information")
        .append_item(
            ID_MENU_README,
            "Open &Readme",
            "Open the documentation in your browser",
        )
        .append_item(
            ID_MENU_CHANGELOG,
            "View &Changelog",
            "Open the list of changes in your browser",
        )
        .build();
    let menu_bar = MenuBar::builder()
        .append(file_menu, "&File")
        .append(tools_menu, "&Tools")
        .append(help_menu, "&Help")
        .build();
    frame.set_menu_bar(menu_bar);

    let app = app.clone();
    let frame = frame.clone();
    frame
        .clone()
        .on_menu_selected(move |event| match event.get_id() {
            ID_MENU_CONFIGURE => connect_dialog::show(&app, &frame),
            ID_MENU_STREAM_INFO => {
                stream_info_dialog::show(&app, &frame);
            }
            ID_MENU_PREFERENCES => preferences::show(&app, &frame),
            ID_MENU_EXIT => {
                frame.close(false);
            }
            ID_MENU_SOUND_PACK_MANAGER => {
                if let Err(message) = launch_sound_pack_manager() {
                    show_error(&frame, "Sound Pack Manager", &message);
                }
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
                if let Err(message) = open_doc("readme.html", README_URL) {
                    show_error(&frame, "Open Readme", &message);
                }
            }
            ID_MENU_CHANGELOG => {
                if let Err(message) = open_doc("changelog.html", CHANGELOG_URL) {
                    show_error(&frame, "View Changelog", &message);
                }
            }
            _ => {}
        });
}

/// Starts the standalone Sound Pack Manager, which ships next to pubsplash.exe.
///
/// Sibling-of-`current_exe` only, exactly like `vst::scan::helper_path`. Falling
/// back to the bare name would let Windows resolve it against the working
/// directory and PATH — which either fails with a pathless "os error 2" or, on
/// an unlucky machine, runs something else entirely.
fn launch_sound_pack_manager() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe failed: {e}"))?;
    let manager = exe.with_file_name("pubsplash-soundpack.exe");
    if !manager.is_file() {
        return Err(format!(
            "The Sound Pack Manager ({}) is missing. Reinstall Pubsplash to restore it.",
            manager.display()
        ));
    }
    std::process::Command::new(&manager)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start {}: {e}", manager.display()))
}

/// Opens a documentation file that ships with Pubsplash, falling back to the
/// copy on GitHub.
///
/// The local file is preferred because it matches the build the user is
/// actually running and needs no network. `fallback_url` covers a source
/// checkout that never generated the HTML, plus the case where the file is
/// there but has no handler — hence the fall-through on a failed open, not
/// just on a missing file.
fn open_doc(name: &str, fallback_url: &str) -> Result<(), String> {
    if let Some(path) = find_doc(name) {
        let target = path.to_string_lossy().into_owned();
        if shell_open(&target).is_ok() {
            return Ok(());
        }
    }
    shell_open(fallback_url).map_err(|e| format!("Could not open {fallback_url}: {e}"))
}

/// Finds a documentation file that ships with Pubsplash.
///
/// Both the installer (everything lands in `$INSTDIR`) and the portable ZIP put
/// the docs directly beside `pubsplash.exe`, so the sibling check covers every
/// shipped layout. Walking on up the exe's ancestors additionally picks up a
/// source checkout, where the generated HTML sits at the repository root and the
/// exe is down in `target/<profile>`. Resolved from the exe, never the working
/// directory, which a shortcut's "Start in" can point anywhere.
fn find_doc(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    doc_in(exe.parent()?.ancestors().take(4), name)
}

/// The pure half of [`find_doc`], split out so it can be tested.
fn doc_in<'a>(dirs: impl Iterator<Item = &'a Path>, name: &str) -> Option<PathBuf> {
    dirs.map(|dir| dir.join(name)).find(|path| path.is_file())
}

/// Opens a file path or URL with whatever the user has it associated with.
///
/// `ShellExecuteW` rather than `cmd /C start`: the latter flashes a console
/// window and treats `&` in a path as a command separator.
fn shell_open(target: &str) -> Result<(), String> {
    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Success is any value above 32; at or below that it is an error code.
    let code = result.0 as usize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecute failed with code {code}"))
    }
}

/// Diagnostic only.
///
/// `App` is an `Rc` and every wx event closure holds a clone, while
/// `App.widgets` holds the `Frame` those closures are bound to — a cycle, so
/// whether this ever runs depends on whether wxdragon frees closure boxes when
/// the frame is destroyed. If it does not, plugin DLLs are never `FreeLibrary`d
/// and the TTS and scan threads are never joined at exit. Nothing depends on
/// that today (`on_close` flushes the config, the recording and the stream
/// explicitly, precisely because this cannot be relied on), but the log line
/// says which world we are in.
impl Drop for App {
    fn drop(&mut self) {
        log::debug!("App dropped");
    }
}

/// Starts or stops the 100 ms timer that services open plugin editors and a
/// running plugin scan, to match whether either exists.
///
/// These two are the only things left that need a steady cadence no event can
/// supply: a plugin editor expects `effEditIdle` regularly whether or not the
/// user is doing anything, and a progress dialog has to animate and have its
/// Cancel button polled. Neither is usually happening, so rather than keep a
/// 10 Hz timer running for the whole session, the timer exists only while one
/// of them does.
///
/// Deriving the answer from current state, rather than starting and stopping in
/// pairs, is what keeps this from drifting out of step: call it after anything
/// that could have changed either, and it settles on the right answer.
pub fn sync_fast_timer(app: &Rc<App>) {
    let needed = !app.open_editors.borrow().is_empty() || app.scan.borrow().is_some();
    let mut slot = app.fast_timer.borrow_mut();
    if needed == slot.is_some() {
        return;
    }
    if !needed {
        // Dropping the timer destroys it, which stops it.
        *slot = None;
        return;
    }
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let timer = Timer::new(&frame);
    let app_for_tick = app.clone();
    timer.on_tick(move |_| {
        if app_for_tick.shutting_down.get() {
            return;
        }
        pump_scan_events(&app_for_tick);
        fx_editor::pump(&app_for_tick);
    });
    timer.start(100, false);
    *slot = Some(timer);
}

/// Acts on the result of an application-process refresh.
fn apply_app_changes(app: &Rc<App>, (labels_changed, capture_changed): (bool, bool)) {
    if capture_changed {
        app.sync_engine_sources();
    }
    if labels_changed {
        home::relabel_source_strips(app);
        scenes::refresh_sources_list(app);
    }
}

/// Clears [`App::pumping`] however `pump_events` returns — including if a
/// handler panics, which wxdragon catches and discards, and which would
/// otherwise wedge the pump for the rest of the session.
struct PumpGuard(Rc<App>);

impl Drop for PumpGuard {
    fn drop(&mut self) {
        self.0.pumping.set(false);
    }
}

/// Drains engine and network events into UI state.
///
/// Not re-entrant. Several of the handlers below open modal dialogs, and a
/// modal runs a nested event loop — which can fire the pump timer again, on top
/// of a call that is part-way through and may be holding an `App` borrow. The
/// guard makes the nested call a no-op; the events are still there for the
/// outer call (or the next tick) to drain.
/// Callbacks waiting on a background thread, run from the pump.
///
/// wxdragon has no post-to-UI-thread primitive, so work that must touch
/// widgets after a worker finishes parks a polling closure here instead. Each
/// returns `true` when it is done and should be dropped. The UI thread owns
/// this outright — hence `thread_local` rather than a lock.
type PendingCallback = Box<dyn FnMut() -> bool>;

thread_local! {
    static PENDING: std::cell::RefCell<Vec<PendingCallback>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Runs `callback` on each pump tick until it returns `true`.
///
/// Must be called from the UI thread; the callback runs there too, so it may
/// touch widgets freely.
pub fn run_when_ready(callback: impl FnMut() -> bool + 'static) {
    PENDING.with(|pending| pending.borrow_mut().push(Box::new(callback)));
}

/// Polls every parked callback, dropping the ones that report completion.
fn run_pending() {
    // Taken before running: a callback may park another (a fetch that leads to
    // a preview), and appending to a borrowed vector would panic.
    let mut callbacks = PENDING.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
    callbacks.retain_mut(|callback| !callback());
    PENDING.with(|pending| pending.borrow_mut().extend(callbacks));
}

fn pump_events(app: &Rc<App>) {
    if app.pumping.replace(true) {
        return;
    }
    let _guard = PumpGuard(app.clone());

    run_pending();

    let mut stream_ui_dirty = false;
    let mut chat_arrived = report_speech_problems(app);
    let mut sound_events = Vec::new();

    while let Ok(event) = app.net.events.try_recv() {
        match event {
            NetEvent::Connected {
                service_id,
                display_name,
            } => {
                {
                    let mut run = app.run.borrow_mut();
                    run.connecting = false;
                    run.connected_service = Some(service_id.clone());
                }
                {
                    let mut config = app.config.borrow_mut();
                    config.connection.last_used_site = Some(service_id.clone());
                }
                app.save_config();
                stream_ui_dirty = true;
                // Bound to a local first: an `if let` scrutinee temporary
                // lives for the whole then-branch, which here opens a modal.
                let connect_ui = app.connect_ui.borrow().clone();
                if let Some(ui) = connect_ui {
                    ui.connect_button.set_label("Dis&connect");
                    show_info(
                        &ui.dialog,
                        "Connected",
                        &format!("Connected to {display_name}."),
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
                        // The frame is cloned out before the modal opens:
                        // `widgets` stays borrowed for the whole closure, and
                        // a modal runs a nested event loop underneath it.
                        if let Some(frame) = app.widgets(|w| w.frame.clone()) {
                            show_error(&frame, "Connection failed", &text);
                        }
                    }
                }
            }
            NetEvent::Disconnected => {
                let mut run = app.run.borrow_mut();
                run.connected_service = None;
                run.connecting = false;
                drop(run);
                stream_ui_dirty = true;
                let connect_ui = app.connect_ui.borrow().clone();
                if let Some(ui) = connect_ui {
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
                run.listener_baseline = false;
                drop(run);
                stream_ui_dirty = true;
            }
            NetEvent::StreamEnded => {
                app.engine.send(EngineCommand::StopEncoding);
                app.engine.send(EngineCommand::StopRecording);
                let mut run = app.run.borrow_mut();
                run.stream = StreamState::Idle;
                run.stream_started = None;
                run.recording_started = None;
                drop(run);
                stream_ui_dirty = true;
            }
            NetEvent::StreamError { message } => {
                app.engine.send(EngineCommand::StopEncoding);
                app.engine.send(EngineCommand::StopRecording);
                let mut run = app.run.borrow_mut();
                run.stream = StreamState::Idle;
                run.stream_started = None;
                run.recording_started = None;
                drop(run);
                stream_ui_dirty = true;
                if let Some(frame) = app.widgets(|w| w.frame.clone()) {
                    show_error(&frame, "Streaming problem", &message);
                }
            }
            NetEvent::Chat(message) => {
                play_sound_event(app, crate::soundpack::StreamEvent::IncomingChat);
                let user = message.user.display().to_string();
                speak_chat(app, &user, &message.content);
                app.run
                    .borrow_mut()
                    .chat
                    .push(ChatEntry::new(user, message.content));
                chat_arrived += 1;
            }
            NetEvent::Listeners { active, peak } => {
                let mut run = app.run.borrow_mut();
                if run.listener_baseline {
                    if active > run.listeners {
                        sound_events.push(crate::soundpack::StreamEvent::ListenerIncrease);
                    }
                    if active < run.listeners {
                        sound_events.push(crate::soundpack::StreamEvent::ListenerDecrease);
                    }
                    if peak > run.listener_peak {
                        sound_events.push(crate::soundpack::StreamEvent::ListenerPeakIncrease);
                    }
                }
                run.listeners = active;
                run.listener_peak = peak.max(run.listener_peak);
                run.listener_baseline = true;
                drop(run);
                stream_ui_dirty = true;
            }
            NetEvent::ChatSent => {
                play_sound_event(app, crate::soundpack::StreamEvent::OutgoingChat)
            }
            NetEvent::ChatSendFailed { message } => {
                app.widgets(|w| {
                    show_error(&w.frame, "Chat", &format!("Message not sent: {message}"))
                });
            }
        }
    }

    for event in sound_events {
        play_sound_event(app, event);
    }

    // Sources whose capture threads changed state. The capture thread has
    // already logged the detail, so nothing is logged again here; what matters
    // is telling the user, which the strip labels do.
    let mut labels_dirty = false;
    while let Ok(event) = app.engine.events.try_recv() {
        match event {
            crate::audio::EngineEvent::SourceError { name, message } => {
                log::debug!("Source {name:?} is reconnecting: {message}");
                labels_dirty |= app.run.borrow_mut().failing.insert(name);
            }
            crate::audio::EngineEvent::SourceRecovered { name } => {
                labels_dirty |= app.run.borrow_mut().failing.remove(&name);
            }
            crate::audio::EngineEvent::EncodingStopped => {}
            // The audio thread has swapped to the new FX chains and returned
            // the replaced ones. Reclaim them here so plugin teardown follows
            // the UI-thread hosting contract.
            crate::audio::EngineEvent::BusesApplied => {
                app.engine.reclaim_retired_chains();
            }
        }
    }
    // Instances the UI took out of a chain but did not release, for the same
    // reason: plugin teardown runs third-party code and belongs here, where no
    // `RefCell` is borrowed, not inside the edit that removed it. Unconditional
    // rather than folded into the arm above — a plugin whose slot never reached
    // the engine is retired without a `BusesApplied` to hang it on.
    fx::release_orphans(app);

    // A TTS voice catalog refresh committed since the last tick. ElevenLabs
    // source labels are built from it, so they are stale now. The refresh below
    // is in place, so a generation that turns out not to have changed any label
    // writes nothing and announces nothing.
    {
        let generation = crate::tts::catalog::generation();
        let mut run = app.run.borrow_mut();
        if run.tts_catalog_generation != generation {
            run.tts_catalog_generation = generation;
            labels_dirty = true;
        }
    }

    if labels_dirty {
        // In place, exactly as the application poll does it: rebuilding the
        // mixer would move focus out from under whoever is tabbing through it.
        home::relabel_source_strips(app);
        scenes::refresh_sources_list(app);
    }

    // Speech usage, the same way: the workers bump a counter rather than
    // sending an event, and this notices. `refresh_usage` never writes the
    // selected row, so a chat flood cannot talk over a user reading the tab.
    // A catalog refresh also improves these rows (it is what turns an opaque
    // ElevenLabs voice id into a name), so redraw when either has moved.
    {
        let generation = crate::tts::usage::generation();
        let moved = {
            let mut run = app.run.borrow_mut();
            let moved = run.usage_generation != generation;
            run.usage_generation = generation;
            moved
        };
        if moved || labels_dirty {
            api::refresh_usage(app);
        }
    }
    chat_arrived += api::report_balance_failures(app);

    // F1 help: one relaxed atomic unless F1 was actually pressed, and the hook
    // rings the idle doorbell when it was. F6 pane cycling arrives the same way.
    help::pump();
    panes::pump(app);
    // User keybindings ride the same hook, and their actions run here so nothing
    // touches `App` from the hook context.
    keybinds::pump(app);
    // Scans and plugin editors are serviced by the fast timer, which only runs
    // while one of them exists; settle that here so no transition can leave it
    // running with nothing to do (or stopped with something waiting).
    sync_fast_timer(app);

    if stream_ui_dirty {
        app.refresh_stream_ui();
    }
    if chat_arrived > 0 {
        chat::append_new_messages(app, chat_arrived);
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
mod accessible_tests {
    use super::NameOnlyAccessible;
    use wxdragon::accessible::AccessibleImpl;
    use wxdragon::ffi::{wxd_AccStatus_WXD_ACC_NOT_IMPLEMENTED, wxd_AccStatus_WXD_ACC_OK};

    #[test]
    fn child_count_is_delegated() {
        // Answering "0 children" is taken at face value by wxWidgets, so any
        // control that grows real MSAA children would be published as empty.
        let (status, _) = NameOnlyAccessible("Bypass selected plugin".into()).get_child_count();
        assert_eq!(status, wxd_AccStatus_WXD_ACC_NOT_IMPLEMENTED);
    }

    #[test]
    fn name_answers_only_for_self() {
        let acc = NameOnlyAccessible("Speech engine".into());
        assert_eq!(
            acc.get_name(0),
            (wxd_AccStatus_WXD_ACC_OK, Some("Speech engine".to_string()))
        );
        // Children must keep their own text; naming them would announce every
        // one of them as the control's name.
        assert_eq!(
            acc.get_name(1),
            (wxd_AccStatus_WXD_ACC_NOT_IMPLEMENTED, None)
        );
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
        assert_eq!(
            super::expand_stream_tokens("no tokens", "t", "u"),
            "no tokens"
        );
    }

    #[test]
    fn recording_filename_is_stamped() {
        let name = super::recording_filename();
        // "recording_<yyyy-mm-dd>_<HH-MM-SS>.mp3"
        assert!(name.starts_with("recording_"), "got {name}");
        assert!(name.ends_with(".mp3"));
        let stamp = name.trim_end_matches(".mp3").rsplit('_').next().unwrap();
        assert_eq!(stamp.len(), 8, "time HH-MM-SS in {name}");
    }
}

#[cfg(test)]
mod doc_tests {
    #[test]
    fn doc_in_finds_the_first_directory_holding_the_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src = root.join("src");
        // changelog.md lives at the repo root, not in src/, so the walk has to
        // get past the first candidate before it matches.
        let found = super::doc_in([src.as_path(), root].into_iter(), "changelog.md");
        assert_eq!(found, Some(root.join("changelog.md")));
    }

    #[test]
    fn doc_in_is_none_when_no_directory_has_the_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            super::doc_in([root].into_iter(), "no-such-doc.html"),
            None,
            "a doc that does not exist must not resolve"
        );
    }

    /// Ignored because it opens a browser window. Run with
    /// `cargo test shell_open_launches_the_readme -- --include-ignored` after
    /// generating readme.html (`marked README.md -o readme.html`).
    #[test]
    #[ignore]
    fn shell_open_launches_the_readme() {
        let readme = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("readme.html");
        assert!(readme.is_file(), "generate readme.html first");
        super::shell_open(&readme.to_string_lossy()).expect("ShellExecute");
    }
}
