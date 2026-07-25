//! Home tab: stream overview, mixer, scene switching, start/stop button.

use super::slider_uia;
use super::{
    App, ID_MIXER_BOOST, WXK_DOWN, WXK_END, WXK_HOME, WXK_LEFT, WXK_PAGEDOWN, WXK_PAGEUP,
    WXK_RIGHT, WXK_UP,
};
use crate::audio::EngineCommand;
use crate::config::max_volume;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wxdragon::event::{EventType, WxEvtHandler};
use wxdragon::prelude::*;

/// Builds the tab; returns (overview box, stream button, record button, scene
/// list, mixer panel placeholder).
pub fn build(app: &Rc<App>, panel: &Panel) -> (TextCtrl, Button, Button, ListBox, Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // Overview.
    let overview_label = StaticText::builder(panel)
        .with_label("Stream overview")
        .build();
    let overview = TextCtrl::builder(panel)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .build();
    super::set_accessible_name(&overview, "Stream overview");
    super::help::tag(&overview, "tab.home.overview", "Stream overview");
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
    super::set_accessible_name(&scene_list, "Scenes");
    super::help::tag(&scene_list, "tab.home.sceneList", "Scenes list");
    let switch_button = Button::builder(panel)
        .with_label("S&witch to scene")
        .build();
    super::help::tag(&switch_button, "tab.home.switchScene", "Switch to scene button");
    sizer.add(&scene_label, 0, SizerFlag::All, 4);
    sizer.add(&scene_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&switch_button, 0, SizerFlag::All, 4);

    // Stream toggle, then the standalone record toggle (Tab order: stream
    // first, record second).
    let stream_button = Button::builder(panel)
        .with_label("&Start streaming")
        .build();
    super::help::tag(&stream_button, "tab.home.streamButton", "Start/stop streaming button");
    sizer.add(&stream_button, 0, SizerFlag::All, 8);
    let record_button = Button::builder(panel)
        .with_label("Start &recording")
        .build();
    super::help::tag(&record_button, "tab.home.recordButton", "Start/stop recording button");
    sizer.add(&record_button, 0, SizerFlag::All, 8);

    panel.set_sizer(sizer, true);

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

    (overview, stream_button, record_button, scene_list, mixer_panel)
}

fn switch_to_selected_scene(app: &Rc<App>, list: &ListBox) {
    let Some(index) = list.get_selection() else {
        return;
    };
    let name = {
        let config = app.config.borrow();
        let Some(scene) = config.scenes.scenes.get(index as usize) else {
            return;
        };
        scene.name.clone()
    };
    let changed = app.config.borrow_mut().scenes.switch_to(&name);
    // Switching to the already-active scene must do nothing.
    if changed == crate::state::ListEdit::Changed {
        app.save_config();
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
        w.home_scene_list.clear();
        for scene in &config.scenes.scenes {
            let label = if scene.name == config.scenes.active_scene {
                format!("{} (active)", scene.name)
            } else {
                scene.name.clone()
            };
            w.home_scene_list.append(&label);
        }
        if let Some(index) = selected {
            if index < w.home_scene_list.get_count() {
                w.home_scene_list.set_selection(index, true);
            }
        }
    });
}

/// Volume change per arrow key press, in percentage points.
const STEP: i32 = 1;
/// Volume change per Page Up / Page Down press, in percentage points. Kept the
/// same whether or not the strip is boosted, so a page always means "10%".
const PAGE_STEP: i32 = 10;

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
    // ever the kind's display name — see `crate::source_name`.
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
/// — see [`relabel_source_strips`].
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
}

/// Removes the UIA providers installed on the current mixer sliders. Must run
/// while those sliders' windows still exist — i.e. before `rebuild_mixer`
/// destroys the inner panel, and before the frame is torn down.
pub fn drop_mixer_strips(app: &Rc<App>) {
    app.widgets(|w| {
        for strip in w.mixer_strips.borrow_mut().drain(..) {
            strip.announcer.uninstall();
        }
    });
}

/// Re-derives the source strips' names and applies any that changed, without
/// creating or destroying a widget — so keyboard focus survives. This runs off
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
            apply_strip_name(strip);
        }
    });
}

/// The slider's announced name. Boost is part of the name so a screen reader
/// says which strips are amplified when tabbing through the mixer.
fn spoken_name(name: &str, boost: bool) -> String {
    if boost {
        format!("{name} volume, boost")
    } else {
        format!("{name} volume")
    }
}

/// Pushes a strip's current name out to every place it is read from: the
/// visible label, the slider's accessible name, the UIA announcement, and the
/// mute checkbox.
fn apply_strip_name(strip: &MixerStrip) {
    let name = strip.name.borrow();
    let spoken = spoken_name(&name, strip.boost.get());
    strip.label.set_label(&format!("{name} volume"));
    super::set_accessible_name(&strip.slider, &spoken);
    strip
        .announcer
        .update(&spoken, &format!("{}%", strip.slider.value()));
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
    super::help::tag(&slider, "tab.home.mixer.strip.volume", "Mixer strip volume slider");
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
    };
    apply_strip_name(&strip);
    let name = strip.name.clone();
    let boost_now = strip.boost.clone();
    let strip_for_boost = strip.clone();
    app.widgets(|w| w.mixer_strips.borrow_mut().push(strip));
    super::help::tag(&mute_check, "tab.home.mixer.strip.mute", "Mixer strip mute checkbox");

    row.add(&label, 0, SizerFlag::AlignCenterVertical | SizerFlag::All, 4);
    row.add(&slider, 1, SizerFlag::Expand | SizerFlag::All, 4);
    row.add(&mute_check, 0, SizerFlag::AlignCenterVertical | SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand, 0);

    // Volume changes.
    {
        let app = app.clone();
        let slider_for_handler = slider.clone();
        let announcer = announcer.clone();
        let max = max.clone();
        let name = name.clone();
        let boost_now = boost_now.clone();
        slider.on_slider(move |_| {
            let value = slider_for_handler.value().clamp(0, max.get()) as u32;
            apply_volume(&app, target, value);
            announcer.update(
                &spoken_name(&name.borrow(), boost_now.get()),
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
        slider.on_key_down(move |event| {
            let Some((code, _)) = super::key_of(&event) else {
                event.skip(true);
                return;
            };
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
                &spoken_name(&name.borrow(), boost_now.get()),
                &format!("{value}%"),
            );
        });
    }

    // Context menu (right-click, SHIFT+F10, or the applications key — Windows
    // raises WM_CONTEXTMENU for all three, which wx turns into this event).
    // `Slider` doesn't implement wxdragon's `MenuEvents` trait and the orphan
    // rule blocks adding it, so bind the raw event types instead.
    {
        let app = app.clone();
        let slider_for_menu = slider.clone();
        slider.bind_internal(EventType::CONTEXT_MENU, move |_| {
            let boost = boost_of(&app, target);
            let mut menu = Menu::builder()
                .append_check_item(
                    ID_MIXER_BOOST,
                    "Enable volume boost",
                    "Allow this strip's volume to go above 100%, up to 500%",
                )
                .build();
            menu.check_item(ID_MIXER_BOOST, boost);
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
            apply_strip_name(&strip_for_boost);
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
    app.sync_engine_sources();
    rebuild_mixer(app);
    refresh_scene_list(app);
}
