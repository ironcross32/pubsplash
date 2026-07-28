//! Home tab: stream overview, mixer, scene switching, start/stop button.

use super::slider_uia;
use super::{
    App, ID_MIXER_BOOST, ID_MIXER_MONITOR, Monitors, StreamState, WXK_DOWN, WXK_END, WXK_HOME,
    WXK_LEFT, WXK_PAGEDOWN, WXK_PAGEUP, WXK_RIGHT, WXK_UP,
};
use crate::audio::EngineCommand;
use crate::config::max_volume;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wxdragon::event::{EventType, WxEvtHandler};
use wxdragon::prelude::*;

/// Shown when there are no scenes. See [`super::list`].
const NO_SCENES: &str = "No scenes";

/// Placeholder for the overview list, which never actually empties: the Status
/// row is always there. Present only to satisfy [`super::list`].
const NO_OVERVIEW: &str = "No stream information";

/// Builds the tab; returns (overview list, stream button, record button, scene
/// list, mixer panel placeholder).
pub fn build(app: &Rc<App>, panel: &Panel) -> (ListBox, Button, Button, ListBox, Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // Overview. A list rather than a read-only text box: the Duration row is
    // rewritten every second while a clock runs, and rewriting a `TextCtrl`
    // resets its caret, so the arrow keys could never reach the end of it.
    let overview_label = StaticText::builder(panel)
        .with_label("Stream overview")
        .build();
    let overview = ListBox::builder(panel).build();
    // `refresh_overview` fills this properly during `build`, but never leave a
    // list at zero items — NVDA reads those as "Unknown".
    super::list::fill(&overview, &[], NO_OVERVIEW);
    super::native_acc::install(&overview, "Stream overview");
    super::help::tag(&overview, "tab.home.overview", "Stream overview list");
    sizer.add(&overview_label, 0, SizerFlag::All, 4);
    sizer.add(&overview, 1, SizerFlag::Expand | SizerFlag::All, 4);

    // Mixer lives in its own panel so it can be rebuilt when sources change.
    let mixer_label = StaticText::builder(panel).with_label("Mixer").build();
    let mixer_panel = Panel::builder(panel).build();
    sizer.add(&mixer_label, 0, SizerFlag::All, 4);
    sizer.add(&mixer_panel, 0, SizerFlag::Expand | SizerFlag::All, 4);

    // Scenes.
    let scene_label = StaticText::builder(panel).with_label("Scenes").build();
    let scene_list = ListBox::builder(panel).build();
    super::native_acc::install(&scene_list, "Scenes");
    super::help::tag(&scene_list, "tab.home.sceneList", "Scenes list");
    let switch_button = Button::builder(panel)
        .with_label("S&witch to scene")
        .build();
    super::help::tag(
        &switch_button,
        "tab.home.switchScene",
        "Switch to scene button",
    );
    sizer.add(&scene_label, 0, SizerFlag::All, 4);
    sizer.add(&scene_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&switch_button, 0, SizerFlag::All, 4);

    // Stream toggle, then the standalone record toggle (Tab order: stream
    // first, record second).
    let stream_button = Button::builder(panel)
        .with_label("&Start streaming")
        .build();
    super::help::tag(
        &stream_button,
        "tab.home.streamButton",
        "Start/stop streaming button",
    );
    sizer.add(&stream_button, 0, SizerFlag::All, 8);
    let record_button = Button::builder(panel)
        .with_label("Start &recording")
        .build();
    super::help::tag(
        &record_button,
        "tab.home.recordButton",
        "Start/stop recording button",
    );
    sizer.add(&record_button, 0, SizerFlag::All, 8);

    panel.set_sizer(sizer, true);

    // The overview's clock rows are never rewritten while selected (see
    // `refresh`), so they are brought up to date at the two moments the user is
    // about to hear one instead.
    //
    // Arrowing within the list: the row just vacated is no longer selected, so
    // it updates at once and stepping off the Duration row and back reads the
    // current time rather than the value frozen when you arrived. The row being
    // moved *onto* is still skipped, so this cannot talk over it.
    {
        let app = app.clone();
        overview
            .clone()
            .on_selection_changed(move |_| refresh_overview(&app));
    }
    // Focus arriving on the list: whatever row it lands on is about to be read
    // out, so this is the one refresh allowed to write the selected row.
    {
        let app = app.clone();
        overview.clone().on_set_focus(move |event| {
            refresh(&app, Selected::Write);
            event.skip(true);
        });
    }

    // Scene switching: button, or Enter/double-click on the list.
    {
        let app = app.clone();
        let scene_list = scene_list.clone();
        switch_button.on_click(move |_| switch_to_selected_scene(&app, &scene_list));
    }
    {
        let app = app.clone();
        let scene_list_for_handler = scene_list.clone();
        scene_list.on_item_double_clicked(move |_| {
            switch_to_selected_scene(&app, &scene_list_for_handler);
        });
    }

    {
        let app = app.clone();
        stream_button.on_click(move |_| {
            if app.is_streaming_or_starting() {
                app.stop_streaming();
            } else {
                super::start_streaming(&app);
            }
        });
    }

    {
        let app = app.clone();
        record_button.on_click(move |_| {
            if app.run.borrow().recording {
                app.stop_recording();
            } else {
                app.start_recording();
            }
        });
    }

    (
        overview,
        stream_button,
        record_button,
        scene_list,
        mixer_panel,
    )
}

fn switch_to_selected_scene(app: &Rc<App>, list: &ListBox) {
    let count = app.config.borrow().scenes.scenes.len();
    let Some(index) = super::list::selection(list, count) else {
        return;
    };
    let name = {
        let config = app.config.borrow();
        let Some(scene) = config.scenes.scenes.get(index) else {
            return;
        };
        scene.name.clone()
    };
    let changed = app.config.borrow_mut().scenes.switch_to(&name);
    // Switching to the already-active scene must do nothing.
    if changed == crate::state::ListEdit::Changed {
        app.save_config();
        // The new scene's sources are different strips at the same positions.
        app.clear_source_monitors();
        app.sync_engine_sources();
        rebuild_mixer(app);
        refresh_scene_list(app);
    }
}

/// Fills the home scene list, marking the active scene.
pub fn refresh_scene_list(app: &Rc<App>) {
    app.widgets(|w| {
        let config = app.config.borrow();
        let selected = w.home_scene_list.get_selection();
        let labels: Vec<String> = config
            .scenes
            .scenes
            .iter()
            .map(|scene| {
                if scene.name == config.scenes.active_scene {
                    format!("{} (active)", scene.name)
                } else {
                    scene.name.clone()
                }
            })
            .collect();
        if super::list::sync(&w.home_scene_list, &labels, NO_SCENES) == super::list::Synced::Kept {
            return;
        }
        if let Some(index) = selected {
            if index < w.home_scene_list.get_count() {
                w.home_scene_list.set_selection(index, true);
            }
        }
    });
}

/// Which fact a row of the overview list carries.
///
/// Rows come and go with the stream state, so the selection is restored by kind
/// and not by index — see [`refresh_overview`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OverviewRow {
    Status,
    Listeners,
    ListenerPeak,
    Duration,
}

/// Everything the overview list shows, snapshotted out of `Runtime` so the rows
/// can be built (and tested) without touching widgets or the clock.
pub struct OverviewState {
    pub stream: StreamState,
    /// Whether a recording is running — either standalone or alongside the
    /// stream. See `Runtime::recording_started`.
    pub recording: bool,
    pub listeners: u32,
    pub listener_peak: u32,
    /// Elapsed time on whichever clock is running: the stream's if streaming,
    /// otherwise the recording's. `None` when neither is.
    pub elapsed: Option<std::time::Duration>,
}

/// The rows to show, in order. Only the ones that apply exist: idle and not
/// recording is a one-row list.
fn overview_rows(state: &OverviewState) -> Vec<(OverviewRow, String)> {
    let streaming = !matches!(state.stream, StreamState::Idle);
    let status = match (&state.stream, state.recording) {
        // "Not streaming and recording" would be nonsense, so the idle case
        // says what is actually happening instead.
        (StreamState::Idle, true) => "Recording".to_string(),
        (StreamState::Idle, false) => "Not streaming".to_string(),
        (state, recording) => {
            let base = match state {
                StreamState::Starting => "Starting",
                StreamState::Live { .. } => "Streaming",
                _ => "Stopping",
            };
            if recording {
                format!("{base} and recording")
            } else {
                base.to_string()
            }
        }
    };

    let mut rows = vec![(OverviewRow::Status, format!("Status: {status}"))];
    if streaming {
        rows.push((
            OverviewRow::Listeners,
            format!("Listeners: {}", state.listeners),
        ));
        rows.push((
            OverviewRow::ListenerPeak,
            format!("Listener peak: {}", state.listener_peak),
        ));
    }
    if let Some(elapsed) = state.elapsed {
        rows.push((
            OverviewRow::Duration,
            format!("Duration: {}", super::format_duration(elapsed)),
        ));
    }
    rows
}

/// Whether a row the user has selected may be rewritten.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Selected {
    /// Leave it alone: this refresh was not prompted by the user.
    Skip,
    /// Write it: the row is about to be read out anyway.
    Write,
}

/// Brings the overview list up to date.
///
/// Deliberately not [`super::list::sync`]: that rewrites every row whose text
/// differs, and the Duration row differs every second. On MSW `set_string`
/// deletes and reinserts the item and restores the selection, and a screen
/// reader announces that — so **writing the selected row is audible, whether or
/// not the list has focus**, while writing any other row is silent. (Both
/// halves of that were learned the hard way: testing focus instead of selection
/// gave a readout every second for as long as the user stood on some other
/// control.)
///
/// So the once-a-second refresh never writes the selected row, whichever row it
/// is, and nothing in this list can speak on its own. Currency instead comes
/// from the two events that precede a read: moving within the list (the row
/// just vacated updates at once, so arrowing off Duration and back reads the
/// time as it is now) and focus arriving on the list, which is the one moment
/// [`Selected::Write`] is passed.
pub fn refresh_overview(app: &App) {
    refresh(app, Selected::Skip);
}

fn refresh(app: &App, selected_rows: Selected) {
    let state = {
        let run = app.run.borrow();
        let streaming = matches!(run.stream, StreamState::Live { .. });
        OverviewState {
            stream: run.stream.clone(),
            recording: run.recording_started.is_some(),
            listeners: run.listeners,
            listener_peak: run.listener_peak,
            elapsed: if streaming {
                run.stream_started.map(|t| t.elapsed())
            } else {
                run.recording_started.map(|t| t.elapsed())
            },
        }
    };
    let mut rows = overview_rows(&state);

    let Some(list) = app.widgets(|w| w.overview) else {
        return;
    };
    // Nothing below touches a widget while `run` is borrowed: the list's own
    // selection handler calls back into here, and a borrow held across a
    // `set_string` or `set_selection` would be a panic waiting on whichever wx
    // build decides to raise an event from one.
    let mut shown = app.run.borrow().shown.overview.clone();

    let selected = list.get_selection();

    // The common case, every tick: the same rows, one of them re-timed.
    if shown.len() == rows.len() && shown.iter().zip(&rows).all(|((a, _), (b, _))| a == b) {
        for (index, (_, label)) in rows.into_iter().enumerate() {
            if shown[index].1 == label {
                continue;
            }
            if selected_rows == Selected::Skip && selected == Some(index as u32) {
                // Leave the cache stale too, so this is retried the moment the
                // row stops being the selected one.
                continue;
            }
            list.set_string(index as u32, &label);
            shown[index].1 = label;
        }
    } else {
        // The row set itself changed, which only happens on a start or stop the
        // user just performed. Refilling drops the selection, so put it back on
        // the same *kind* of row rather than the same index — a user parked on
        // Duration would otherwise land on Listeners.
        let was_on = selected
            .and_then(|index| shown.get(index as usize))
            .map(|(kind, _)| *kind);
        let labels: Vec<String> = rows.iter().map(|(_, label)| label.clone()).collect();
        super::list::fill(&list, &labels, NO_OVERVIEW);
        std::mem::swap(&mut shown, &mut rows);
        if let Some(index) = was_on.and_then(|kind| shown.iter().position(|(k, _)| *k == kind)) {
            list.set_selection(index as u32, true);
        }
    }

    app.run.borrow_mut().shown.overview = shown;
}

/// Volume change per arrow key press, in percentage points.
const STEP: i32 = 1;
/// Volume change per Page Up / Page Down press, in percentage points. Kept the
/// same whether or not the strip is boosted, so a page always means "10%".
const PAGE_STEP: i32 = 10;
/// CTRL+M toggles monitoring. wx reports letter keys as uppercase ASCII.
const KEY_M: i32 = b'M' as i32;

/// What a mixer strip controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripTarget {
    Master,
    Source(usize),
    Bus(usize),
}

/// Recreates the mixer controls: master volume + mute first, then one strip
/// per source in the active scene, then one per bus, in mixer order.
/// Tab order follows creation order, satisfying the reorder requirement.
pub fn rebuild_mixer(app: &Rc<App>) {
    let Some((mixer_panel, home_panel, old_inner)) = app.widgets(|w| {
        (
            w.mixer_panel.clone(),
            w.home_panel.clone(),
            w.mixer_inner.borrow_mut().take(),
        )
    }) else {
        return;
    };

    // The old sliders' UIA providers must go before their windows do.
    drop_mixer_strips(app);
    if let Some(old) = old_inner {
        old.destroy();
    }
    let inner = Panel::builder(&mixer_panel).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // Strip names are derived from what each source points at (device, running
    // application, voice) rather than from `SourceConfig.name`, which is only
    // ever the kind's display name â€” see `crate::source_name`.
    let ((master_volume, master_muted, master_boost), sources, buses) = {
        let config = app.config.borrow();
        let ctx = app.name_context(
            config
                .scenes
                .active_scene()
                .map(|s| s.sources.as_slice())
                .unwrap_or_default(),
        );
        (
            (
                config.audio.master_volume,
                config.audio.master_muted,
                config.audio.master_boost,
            ),
            config
                .scenes
                .active_scene()
                .map(|s| {
                    crate::source_name::strip_labels(&s.sources, &ctx)
                        .into_iter()
                        .zip(&s.sources)
                        .map(|(name, src)| (name, src.volume, src.muted, src.boost))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            config
                .buses
                .buses
                .iter()
                .map(|b| (b.name.clone(), b.volume, b.muted, b.boost))
                .collect::<Vec<_>>(),
        )
    };

    // Master strip.
    add_strip(
        app,
        &inner,
        &sizer,
        "Master",
        master_volume,
        master_muted,
        master_boost,
        StripTarget::Master,
        None,
    );

    for (index, (name, volume, muted, boost)) in sources.iter().enumerate() {
        add_strip(
            app,
            &inner,
            &sizer,
            name,
            *volume,
            *muted,
            *boost,
            StripTarget::Source(index),
            Some(index),
        );
    }
    for (index, (name, volume, muted, boost)) in buses.iter().enumerate() {
        add_strip(
            app,
            &inner,
            &sizer,
            &format!("{name} bus"),
            *volume,
            *muted,
            *boost,
            StripTarget::Bus(index),
            None,
        );
    }

    inner.set_sizer(sizer, true);
    let outer = BoxSizer::builder(Orientation::Vertical).build();
    outer.add(&inner, 1, SizerFlag::Expand, 0);
    mixer_panel.set_sizer(outer, true);
    app.widgets(|w| *w.mixer_inner.borrow_mut() = Some(inner));
    home_panel.layout();
}

/// One live mixer strip. Kept so a strip can be re-named without recreating it
/// â€” see [`relabel_source_strips`].
#[derive(Clone)]
pub struct MixerStrip {
    /// Index into the active scene's sources, or `None` for master and buses
    /// (whose names come from config and never change behind the user's back).
    pub source_index: Option<usize>,
    label: StaticText,
    slider: Slider,
    mute: CheckBox,
    announcer: Rc<slider_uia::SliderAnnouncer>,
    /// The strip's current base name, shared with the strip's event handlers.
    name: Rc<RefCell<String>>,
    /// Shared with the boost handler, which is the other thing that can change
    /// what the slider announces.
    boost: Rc<Cell<bool>>,
    /// Shared with the monitoring handlers (context menu and CTRL+M).
    monitor: Rc<Cell<bool>>,
}

/// Removes the UIA providers installed on the current mixer sliders. Must run
/// while those sliders' windows still exist â€” i.e. before `rebuild_mixer`
/// destroys the inner panel, and before the frame is torn down.
pub fn drop_mixer_strips(app: &Rc<App>) {
    app.widgets(|w| {
        for strip in w.mixer_strips.borrow_mut().drain(..) {
            strip.announcer.uninstall();
        }
    });
}

/// Re-derives the source strips' names and applies any that changed, without
/// creating or destroying a widget â€” so keyboard focus survives. This runs off
/// the pump when an application starts or exits, which is exactly when the user
/// is likely to be on that strip.
pub fn relabel_source_strips(app: &Rc<App>) {
    let names = {
        let config = app.config.borrow();
        let Some(scene) = config.scenes.active_scene() else {
            return;
        };
        let ctx = app.name_context(&scene.sources);
        crate::source_name::strip_labels(&scene.sources, &ctx)
    };
    app.widgets(|w| {
        for strip in w.mixer_strips.borrow().iter() {
            let Some(name) = strip.source_index.and_then(|i| names.get(i)) else {
                continue;
            };
            if *strip.name.borrow() == *name {
                continue;
            }
            *strip.name.borrow_mut() = name.clone();
            apply_strip_name(strip, true);
        }
    });
}

/// The slider's announced name. Boost and monitoring are part of the name so a
/// screen reader says which strips are amplified, and which are being listened
/// to, when tabbing through the mixer.
fn spoken_name(name: &str, boost: bool, monitor: bool) -> String {
    let mut spoken = format!("{name} volume");
    if boost {
        spoken.push_str(", boost");
    }
    if monitor {
        spoken.push_str(", monitoring");
    }
    spoken
}

/// Pushes a strip's current name out to every place it is read from: the
/// visible label, the slider's accessible name, the UIA announcement, and the
/// mute checkbox.
///
/// `speak_value` re-announces the slider's value through the UIA provider. Pass
/// false when something else is already speaking the change, so it isn't
/// chased by a redundant "100%".
fn apply_strip_name(strip: &MixerStrip, speak_value: bool) {
    let name = strip.name.borrow();
    let monitor = strip.monitor.get();
    let spoken = spoken_name(&name, strip.boost.get(), monitor);
    // The visible label carries the monitoring state too, so it is there for
    // sighted users and for readers that fall back to the label.
    strip.label.set_label(&if monitor {
        format!("{name} volume (monitoring)")
    } else {
        format!("{name} volume")
    });
    super::set_accessible_name(&strip.slider, &spoken);
    let value = format!("{}%", strip.slider.value());
    if speak_value {
        strip.announcer.update(&spoken, &value);
    } else {
        strip.announcer.set_text(&spoken, &value);
    }
    super::set_accessible_name(&strip.mute, &format!("Mute {name}"));
}

/// One mixer strip.
#[allow(clippy::too_many_arguments)]
fn add_strip(
    app: &Rc<App>,
    parent: &Panel,
    sizer: &BoxSizer,
    name: &str,
    volume: u32,
    muted: bool,
    boost: bool,
    target: StripTarget,
    source_index: Option<usize>,
) {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let label = StaticText::builder(parent).build();
    // The strip's current ceiling, shared with the handlers so the boost toggle
    // can widen or narrow the range without rebuilding (and re-focusing) it.
    let max = Rc::new(Cell::new(max_volume(boost) as i32));
    let slider = Slider::builder(parent)
        .with_value(volume as i32)
        .with_min_value(0)
        .with_max_value(max.get())
        .build();
    super::help::tag(
        &slider,
        "tab.home.mixer.strip.volume",
        "Mixer strip volume slider",
    );
    // A native trackbar announces its position as a percentage of its *range*,
    // which is wrong once the range is 0-500 (100 would be read as "20%"). Our
    // own UIA provider speaks the literal value instead.
    let announcer = Rc::new(slider_uia::install(&slider));
    let mute_check = CheckBox::builder(parent).with_label("Mute").build();
    mute_check.set_value(muted);

    let strip = MixerStrip {
        source_index,
        label: label.clone(),
        slider: slider.clone(),
        mute: mute_check.clone(),
        announcer: announcer.clone(),
        name: Rc::new(RefCell::new(name.to_string())),
        boost: Rc::new(Cell::new(boost)),
        // Monitoring is session state; a rebuilt mixer picks up whatever is
        // playing right now rather than resetting it.
        monitor: Rc::new(Cell::new(monitor_of(app, target))),
    };
    apply_strip_name(&strip, true);
    let name = strip.name.clone();
    let boost_now = strip.boost.clone();
    let monitor_now = strip.monitor.clone();
    let strip_for_boost = strip.clone();
    let strip_for_menu_monitor = strip.clone();
    let strip_for_key_monitor = strip.clone();
    app.widgets(|w| w.mixer_strips.borrow_mut().push(strip));
    super::help::tag(
        &mute_check,
        "tab.home.mixer.strip.mute",
        "Mixer strip mute checkbox",
    );

    row.add(
        &label,
        0,
        SizerFlag::AlignCenterVertical | SizerFlag::All,
        4,
    );
    row.add(&slider, 1, SizerFlag::Expand | SizerFlag::All, 4);
    row.add(
        &mute_check,
        0,
        SizerFlag::AlignCenterVertical | SizerFlag::All,
        4,
    );
    sizer.add_sizer(&row, 0, SizerFlag::Expand, 0);

    // Volume changes.
    {
        let app = app.clone();
        let slider_for_handler = slider.clone();
        let announcer = announcer.clone();
        let max = max.clone();
        let name = name.clone();
        let boost_now = boost_now.clone();
        let monitor_now = monitor_now.clone();
        slider.on_slider(move |_| {
            let value = slider_for_handler.value().clamp(0, max.get()) as u32;
            apply_volume(&app, target, value);
            announcer.update(
                &spoken_name(&name.borrow(), boost_now.get(), monitor_now.get()),
                &format!("{value}%"),
            );
        });
    }

    // Every movement key is handled here rather than by the native trackbar,
    // whose directions are the opposite of what people expect: natively Up and
    // Page Up move *down*, Down and Page Down move *up*, Home jumps to the
    // minimum and End to the maximum. Owning the whole mapping keeps it
    // consistent across strips and at any range (0-100 or 0-500).
    //
    // wxDragon re-arms `Skip(true)` before calling every closure, so a handled
    // key must be consumed with an explicit `skip(false)`. Without it the
    // trackbar's default proc also gets the key, applies its opposite mapping,
    // and the resulting `wxEVT_SLIDER` overwrites what we just saved.
    {
        let app = app.clone();
        let slider_for_keys = slider.clone();
        let announcer = announcer.clone();
        let max = max.clone();
        let name = name.clone();
        let boost_now = boost_now.clone();
        let monitor_now = monitor_now.clone();
        slider.on_key_down(move |event| {
            let Some((code, ctrl)) = super::key_of(&event) else {
                event.skip(true);
                return;
            };
            // CTRL+M toggles monitoring for the focused strip. wx reports
            // letter keys as their uppercase ASCII code.
            if ctrl && code == KEY_M {
                event.skip(false);
                toggle_monitor(&app, target, &strip_for_key_monitor);
                return;
            }
            let ceiling = max.get();
            let current = slider_for_keys.value();
            let value = match code {
                WXK_UP | WXK_RIGHT => current + STEP,
                WXK_DOWN | WXK_LEFT => current - STEP,
                WXK_PAGEUP => current + PAGE_STEP,
                WXK_PAGEDOWN => current - PAGE_STEP,
                WXK_HOME => ceiling,
                WXK_END => 0,
                _ => {
                    event.skip(true);
                    return;
                }
            }
            .clamp(0, ceiling);
            event.skip(false);
            if value != current {
                slider_for_keys.set_value(value);
                apply_volume(&app, target, value as u32);
            }
            // Announced even when already at the end of the range, so the key
            // always produces spoken feedback.
            announcer.update(
                &spoken_name(&name.borrow(), boost_now.get(), monitor_now.get()),
                &format!("{value}%"),
            );
        });
    }

    // Context menu (right-click, SHIFT+F10, or the applications key â€” Windows
    // raises WM_CONTEXTMENU for all three, which wx turns into this event).
    // `Slider` doesn't implement wxdragon's `MenuEvents` trait and the orphan
    // rule blocks adding it, so bind the raw event types instead.
    {
        let app = app.clone();
        let slider_for_menu = slider.clone();
        slider.bind_internal(EventType::CONTEXT_MENU, move |_| {
            let boost = boost_of(&app, target);
            let monitor = monitor_of(&app, target);
            let mut menu = Menu::builder()
                .append_check_item(
                    ID_MIXER_BOOST,
                    "Enable volume boost",
                    "Allow this strip's volume to go above 100%, up to 500%",
                )
                .append_check_item(
                    ID_MIXER_MONITOR,
                    "&Monitor this strip",
                    "Play this strip through your speakers or headphones",
                )
                .build();
            menu.check_item(ID_MIXER_BOOST, boost);
            menu.check_item(ID_MIXER_MONITOR, monitor);
            // Anchor to the slider rather than the mouse so the keyboard paths
            // put the menu next to the control they came from.
            let at = slider_for_menu.client_to_screen(Point { x: 8, y: 8 });
            slider_for_menu.popup_menu(&mut menu, Some(at));
        });
    }

    // The popup is shown on the slider, so its command comes back here.
    {
        let app = app.clone();
        let slider_for_boost = slider.clone();
        let max = max.clone();
        let boost_now = boost_now.clone();
        slider.bind_with_id_internal(EventType::MENU, ID_MIXER_BOOST, move |_| {
            let boost = !boost_of(&app, target);
            set_boost(&app, target, boost);
            let ceiling = max_volume(boost) as i32;
            max.set(ceiling);
            boost_now.set(boost);
            // Dropping boost snaps an amplified strip back to 100%.
            let value = slider_for_boost.value().clamp(0, ceiling);
            slider_for_boost.set_range(0, ceiling);
            slider_for_boost.set_value(value);
            apply_volume(&app, target, value as u32);
            // Boost is spoken as part of the name, so re-announce the strip.
            apply_strip_name(&strip_for_boost, true);
        });
    }

    // Monitoring, from the same popup.
    {
        let app = app.clone();
        slider.bind_with_id_internal(EventType::MENU, ID_MIXER_MONITOR, move |_| {
            toggle_monitor(&app, target, &strip_for_menu_monitor);
        });
    }

    // Mute toggling; unmute restores the last volume (kept in config). The
    // checkbox's checked state is the mute state.
    {
        let app = app.clone();
        let mute_check = mute_check.clone();
        mute_check.clone().on_toggled(move |_| {
            let now_muted = mute_check.get_value();
            {
                let mut config = app.config.borrow_mut();
                match target {
                    StripTarget::Master => config.audio.master_muted = now_muted,
                    StripTarget::Source(i) => {
                        let Some(source) = config
                            .scenes
                            .active_scene_mut()
                            .and_then(|s| s.sources.get_mut(i))
                        else {
                            return;
                        };
                        source.muted = now_muted;
                    }
                    StripTarget::Bus(i) => {
                        let Some(bus) = config.buses.buses.get_mut(i) else {
                            return;
                        };
                        bus.muted = now_muted;
                    }
                }
            }
            match target {
                StripTarget::Master => app.engine.send(EngineCommand::SetMasterMute(now_muted)),
                StripTarget::Source(i) => {
                    app.engine.send(EngineCommand::SetSourceMute(i, now_muted))
                }
                StripTarget::Bus(i) => app.engine.send(EngineCommand::SetBusMute(i, now_muted)),
            }
            app.save_config();
        });
    }
}

/// Whether `target` is currently being monitored. Monitoring is session state,
/// so unlike boost this reads `Runtime`, not `Config`.
fn monitor_of(app: &Rc<App>, target: StripTarget) -> bool {
    let monitors = &app.run.borrow().monitors;
    match target {
        StripTarget::Master => monitors.master,
        StripTarget::Source(i) => monitors.source(i),
        StripTarget::Bus(i) => monitors.bus(i),
    }
}

/// Flips `target`'s monitoring, tells the engine, re-labels the strip, and
/// announces the change.
///
/// The announcement is a UIA notification rather than a value change: the
/// slider's value hasn't moved, and a notification interrupts, so the answer
/// arrives the moment the key is pressed. The strip's name (and its visible
/// label) then carry "monitoring" for the next time it takes focus.
fn toggle_monitor(app: &Rc<App>, target: StripTarget, strip: &MixerStrip) {
    let on = !monitor_of(app, target);
    {
        let monitors = &mut app.run.borrow_mut().monitors;
        match target {
            StripTarget::Master => monitors.master = on,
            StripTarget::Source(i) => Monitors::set(&mut monitors.sources, i, on),
            StripTarget::Bus(i) => Monitors::set(&mut monitors.buses, i, on),
        }
    }
    app.engine.send(match target {
        StripTarget::Master => EngineCommand::SetMasterMonitor(on),
        StripTarget::Source(i) => EngineCommand::SetSourceMonitor(i, on),
        StripTarget::Bus(i) => EngineCommand::SetBusMonitor(i, on),
    });
    strip.monitor.set(on);
    apply_strip_name(strip, false);
    let name = strip.name.borrow().clone();
    super::help::announce(&format!(
        "{name} monitoring {}",
        if on { "on" } else { "off" }
    ));
}

/// Whether `target`'s volume boost is currently on.
fn boost_of(app: &Rc<App>, target: StripTarget) -> bool {
    let config = app.config.borrow();
    match target {
        StripTarget::Master => config.audio.master_boost,
        StripTarget::Source(i) => config
            .scenes
            .active_scene()
            .and_then(|s| s.sources.get(i))
            .is_some_and(|s| s.boost),
        StripTarget::Bus(i) => config.buses.buses.get(i).is_some_and(|b| b.boost),
    }
}

/// Sets `target`'s volume boost. The caller saves (via `apply_volume`).
fn set_boost(app: &Rc<App>, target: StripTarget, boost: bool) {
    let mut config = app.config.borrow_mut();
    match target {
        StripTarget::Master => config.audio.master_boost = boost,
        StripTarget::Source(i) => {
            if let Some(source) = config
                .scenes
                .active_scene_mut()
                .and_then(|s| s.sources.get_mut(i))
            {
                source.boost = boost;
            }
        }
        StripTarget::Bus(i) => {
            if let Some(bus) = config.buses.buses.get_mut(i) {
                bus.boost = boost;
            }
        }
    }
}

fn apply_volume(app: &Rc<App>, target: StripTarget, value: u32) {
    {
        let mut config = app.config.borrow_mut();
        match target {
            StripTarget::Master => config.audio.master_volume = value,
            StripTarget::Source(i) => {
                if let Some(source) = config
                    .scenes
                    .active_scene_mut()
                    .and_then(|s| s.sources.get_mut(i))
                {
                    source.volume = value;
                }
            }
            StripTarget::Bus(i) => {
                if let Some(bus) = config.buses.buses.get_mut(i) {
                    bus.volume = value;
                }
            }
        }
    }
    match target {
        StripTarget::Master => app.engine.send(EngineCommand::SetMasterVolume(value)),
        StripTarget::Source(i) => app.engine.send(EngineCommand::SetSourceVolume(i, value)),
        StripTarget::Bus(i) => app.engine.send(EngineCommand::SetBusVolume(i, value)),
    }
    app.save_config();
}

/// Used by the scenes tab after edits that affect the mixer.
pub fn on_sources_changed(app: &Rc<App>) {
    // Resolve applications first: both the engine's pid and the strip names
    // come out of that cache.
    app.refresh_app_processes();
    // Sources may have been added, removed, or reordered; monitoring is held
    // by position, so keeping the flags would monitor the wrong strip.
    app.clear_source_monitors();
    app.sync_engine_sources();
    rebuild_mixer(app);
    refresh_scene_list(app);
}

#[cfg(test)]
mod tests {
    use super::{OverviewRow, OverviewState, overview_rows};
    use crate::ui::StreamState;
    use std::time::Duration;

    fn state(stream: StreamState, recording: bool, elapsed: Option<u64>) -> OverviewState {
        OverviewState {
            stream,
            recording,
            listeners: 4,
            listener_peak: 7,
            elapsed: elapsed.map(Duration::from_secs),
        }
    }

    #[test]
    fn idle_is_one_row() {
        let rows = overview_rows(&state(StreamState::Idle, false, None));
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0],
            (OverviewRow::Status, "Status: Not streaming".into())
        );
    }

    #[test]
    fn standalone_recording_reports_itself_and_its_clock() {
        let rows = overview_rows(&state(StreamState::Idle, true, Some(41)));
        let kinds: Vec<_> = rows.iter().map(|(kind, _)| *kind).collect();
        // No listener counts while not streaming.
        assert_eq!(kinds, [OverviewRow::Status, OverviewRow::Duration]);
        assert_eq!(rows[0].1, "Status: Recording");
        assert_eq!(rows[1].1, "Duration: 00:00:41");
    }

    #[test]
    fn streaming_and_recording_shows_everything() {
        let rows = overview_rows(&state(
            StreamState::Live {
                stream_id: "1".into(),
            },
            true,
            Some(3723),
        ));
        let kinds: Vec<_> = rows.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds,
            [
                OverviewRow::Status,
                OverviewRow::Listeners,
                OverviewRow::ListenerPeak,
                OverviewRow::Duration,
            ]
        );
        assert_eq!(rows[0].1, "Status: Streaming and recording");
        assert_eq!(rows[1].1, "Listeners: 4");
        assert_eq!(rows[2].1, "Listener peak: 7");
        assert_eq!(rows[3].1, "Duration: 01:02:03");
    }

    #[test]
    fn starting_has_no_duration_row_until_a_clock_runs() {
        let rows = overview_rows(&state(StreamState::Starting, false, None));
        let kinds: Vec<_> = rows.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds,
            [
                OverviewRow::Status,
                OverviewRow::Listeners,
                OverviewRow::ListenerPeak,
            ]
        );
        assert_eq!(rows[0].1, "Status: Starting");
    }

    #[test]
    fn stopping_while_recording_still_says_so() {
        let rows = overview_rows(&state(StreamState::Stopping, true, Some(0)));
        assert_eq!(rows[0].1, "Status: Stopping and recording");
    }
}
