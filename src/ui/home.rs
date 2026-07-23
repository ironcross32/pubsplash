//! Home tab: stream overview, mixer, scene switching, start/stop button.

use super::{App, WXK_END, WXK_HOME};
use crate::audio::EngineCommand;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Builds the tab; returns (overview box, stream button, scene list,
/// mixer panel placeholder).
pub fn build(app: &Rc<App>, panel: &Panel) -> (TextCtrl, Button, ListBox, Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // Overview.
    let overview_label = StaticText::builder(panel)
        .with_label("Stream overview")
        .build();
    let overview = TextCtrl::builder(panel)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .build();
    super::set_accessible_name(&overview, "Stream overview");
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
    let switch_button = Button::builder(panel)
        .with_label("S&witch to scene")
        .build();
    sizer.add(&scene_label, 0, SizerFlag::All, 4);
    sizer.add(&scene_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&switch_button, 0, SizerFlag::All, 4);

    // Stream toggle.
    let stream_button = Button::builder(panel)
        .with_label("&Start streaming")
        .build();
    sizer.add(&stream_button, 0, SizerFlag::All, 8);

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

    (overview, stream_button, scene_list, mixer_panel)
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

    if let Some(old) = old_inner {
        old.destroy();
    }
    let inner = Panel::builder(&mixer_panel).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let (master_volume, master_muted, sources, buses) = {
        let config = app.config.borrow();
        (
            config.audio.master_volume,
            config.audio.master_muted,
            config
                .scenes
                .active_scene()
                .map(|s| {
                    s.sources
                        .iter()
                        .map(|src| (src.name.clone(), src.volume, src.muted))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            config
                .buses
                .buses
                .iter()
                .map(|b| (b.name.clone(), b.volume, b.muted))
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
        StripTarget::Master,
    );

    for (index, (name, volume, muted)) in sources.iter().enumerate() {
        add_strip(
            app,
            &inner,
            &sizer,
            name,
            *volume,
            *muted,
            StripTarget::Source(index),
        );
    }
    for (index, (name, volume, muted)) in buses.iter().enumerate() {
        add_strip(
            app,
            &inner,
            &sizer,
            &format!("{name} bus"),
            *volume,
            *muted,
            StripTarget::Bus(index),
        );
    }

    inner.set_sizer(sizer, true);
    let outer = BoxSizer::builder(Orientation::Vertical).build();
    outer.add(&inner, 1, SizerFlag::Expand, 0);
    mixer_panel.set_sizer(outer, true);
    app.widgets(|w| *w.mixer_inner.borrow_mut() = Some(inner));
    home_panel.layout();
}

/// One mixer strip.
fn add_strip(
    app: &Rc<App>,
    parent: &Panel,
    sizer: &BoxSizer,
    name: &str,
    volume: u32,
    muted: bool,
    target: StripTarget,
) {
    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let label = StaticText::builder(parent)
        .with_label(&format!("{name} volume"))
        .build();
    let slider = Slider::builder(parent)
        .with_value(volume as i32)
        .with_min_value(0)
        .with_max_value(100)
        .build();
    super::set_accessible_name(&slider, &format!("{name} volume"));
    let mute_button = Button::builder(parent)
        .with_label(if muted { "Unmute" } else { "Mute" })
        .build();
    super::set_accessible_name(
        &mute_button,
        &format!("{} {name}", if muted { "Unmute" } else { "Mute" }),
    );

    row.add(&label, 0, SizerFlag::AlignCenterVertical | SizerFlag::All, 4);
    row.add(&slider, 1, SizerFlag::Expand | SizerFlag::All, 4);
    row.add(&mute_button, 0, SizerFlag::All, 4);
    sizer.add_sizer(&row, 0, SizerFlag::Expand, 0);

    // Volume changes.
    {
        let app = app.clone();
        let slider_for_handler = slider.clone();
        slider.on_slider(move |_| {
            let value = slider_for_handler.value().max(0).min(100) as u32;
            apply_volume(&app, target, value);
        });
    }

    // Home = maximum, End = minimum (the reverse of the native slider keys).
    {
        let app = app.clone();
        let slider_for_keys = slider.clone();
        slider.on_key_down(move |event| {
            match super::key_of(&event).map(|(code, _)| code) {
                Some(WXK_HOME) => {
                    slider_for_keys.set_value(100);
                    apply_volume(&app, target, 100);
                }
                Some(WXK_END) => {
                    slider_for_keys.set_value(0);
                    apply_volume(&app, target, 0);
                }
                _ => {
                    event.skip(true);
                }
            }
        });
    }

    // Mute toggling; unmute restores the last volume (kept in config).
    {
        let app = app.clone();
        let mute_button = mute_button.clone();
        let name = name.to_string();
        mute_button.clone().on_click(move |_| {
            let now_muted = {
                let mut config = app.config.borrow_mut();
                match target {
                    StripTarget::Master => {
                        config.audio.master_muted = !config.audio.master_muted;
                        config.audio.master_muted
                    }
                    StripTarget::Source(i) => {
                        let Some(source) = config
                            .scenes
                            .active_scene_mut()
                            .and_then(|s| s.sources.get_mut(i))
                        else {
                            return;
                        };
                        source.muted = !source.muted;
                        source.muted
                    }
                    StripTarget::Bus(i) => {
                        let Some(bus) = config.buses.buses.get_mut(i) else {
                            return;
                        };
                        bus.muted = !bus.muted;
                        bus.muted
                    }
                }
            };
            match target {
                StripTarget::Master => app.engine.send(EngineCommand::SetMasterMute(now_muted)),
                StripTarget::Source(i) => {
                    app.engine.send(EngineCommand::SetSourceMute(i, now_muted))
                }
                StripTarget::Bus(i) => app.engine.send(EngineCommand::SetBusMute(i, now_muted)),
            }
            let action = if now_muted { "Unmute" } else { "Mute" };
            mute_button.set_label(action);
            super::set_accessible_name(&mute_button, &format!("{action} {name}"));
            app.save_config();
        });
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
    app.sync_engine_sources();
    rebuild_mixer(app);
    refresh_scene_list(app);
}
