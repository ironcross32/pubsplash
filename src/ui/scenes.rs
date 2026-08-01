//! Scenes and Sources tab: two lists with move/add/rename/delete controls
//! and per-type source edit dialogs.

use super::home::on_sources_changed;
use super::slider_uia::SliderAnnouncer;
use super::{App, WXK_DELETE, WXK_DOWN, WXK_UP, show_error};
use crate::config::{
    AzureTtsSettings, ElevenLabsTtsSettings, GoogleTtsSettings, GttsTtsSettings, OpenAiTtsSettings,
    PollyTtsSettings, SoundEventsSourceConfig, SourceConfig, SourceKindConfig, TtsEngineProfile,
    TtsEngineSettings, TtsSourceConfig,
};
use crate::soundpack::StreamEvent;
use crate::state::{ListEdit, move_down, move_up};
use std::collections::HashMap;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown when a list has nothing in it. See [`super::list`].
const NO_SCENES: &str = "No scenes";
const NO_SOURCES: &str = "No sources";

pub fn build(app: &Rc<App>, panel: &Panel) -> (ListBox, ListBox) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // --- Scenes ---
    let scenes_label = StaticText::builder(panel).with_label("Scenes").build();
    let scenes_list = ListBox::builder(panel).build();
    super::native_acc::install(&scenes_list, "Scenes");
    super::help::tag(&scenes_list, "tab.scenes.sceneList", "Scenes list");
    let scenes_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let scene_up = Button::builder(panel).with_label("Move &up").build();
    let scene_down = Button::builder(panel).with_label("Move &down").build();
    let scene_add = Button::builder(panel).with_label("Add &scene").build();
    let scene_rename = Button::builder(panel).with_label("&Rename scene").build();
    super::help::tag(&scene_up, "tab.scenes.sceneUp", "Move scene up button");
    super::help::tag(
        &scene_down,
        "tab.scenes.sceneDown",
        "Move scene down button",
    );
    super::help::tag(&scene_add, "tab.scenes.sceneAdd", "Add scene button");
    super::help::tag(
        &scene_rename,
        "tab.scenes.sceneRename",
        "Rename scene button",
    );
    for b in [&scene_up, &scene_down, &scene_add, &scene_rename] {
        scenes_buttons.add(b, 0, SizerFlag::All, 4);
    }

    sizer.add(&scenes_label, 0, SizerFlag::All, 4);
    sizer.add(&scenes_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&scenes_buttons, 0, SizerFlag::Expand, 0);

    // --- Sources ---
    let sources_label = StaticText::builder(panel)
        .with_label("Sources in selected scene")
        .build();
    let sources_list = ListBox::builder(panel).build();
    super::native_acc::install(&sources_list, "Sources in selected scene");
    super::help::tag(
        &sources_list,
        "tab.scenes.sourceList",
        "Sources list for the selected scene",
    );
    let sources_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let source_add = Button::builder(panel).with_label("&Add source").build();
    let source_edit = Button::builder(panel).with_label("&Edit").build();
    let source_sends = Button::builder(panel).with_label("Se&nds...").build();
    let source_remove = Button::builder(panel).with_label("Re&move source").build();
    let source_up = Button::builder(panel).with_label("Move u&p").build();
    let source_down = Button::builder(panel).with_label("Move do&wn").build();
    super::help::tag(&source_add, "tab.scenes.sourceAdd", "Add source button");
    super::help::tag(&source_edit, "tab.scenes.sourceEdit", "Edit source button");
    super::help::tag(
        &source_sends,
        "tab.scenes.sourceSends",
        "Source sends button",
    );
    super::help::tag(
        &source_remove,
        "tab.scenes.sourceRemove",
        "Remove source button",
    );
    super::help::tag(&source_up, "tab.scenes.sourceUp", "Move source up button");
    super::help::tag(
        &source_down,
        "tab.scenes.sourceDown",
        "Move source down button",
    );
    for b in [
        &source_add,
        &source_edit,
        &source_sends,
        &source_remove,
        &source_up,
        &source_down,
    ] {
        sources_buttons.add(b, 0, SizerFlag::All, 4);
    }

    sizer.add(&sources_label, 0, SizerFlag::All, 4);
    sizer.add(&sources_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&sources_buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);

    // Scene list selection drives the sources list.
    {
        let app = app.clone();
        scenes_list
            .clone()
            .on_selection_changed(move |_| refresh_sources_list(&app));
    }

    // Scene buttons.
    {
        let app = app.clone();
        let list = scenes_list.clone();
        scene_up.on_click(move |_| move_scene(&app, &list, true));
    }
    {
        let app = app.clone();
        let list = scenes_list.clone();
        scene_down.on_click(move |_| move_scene(&app, &list, false));
    }
    {
        let app = app.clone();
        scene_add.on_click(move |_| add_scene(&app));
    }
    {
        let app = app.clone();
        let list = scenes_list.clone();
        scene_rename.on_click(move |_| rename_scene(&app, &list));
    }

    // Scene list keys: CTRL+Up/Down reorder, Delete removes.
    {
        let app = app.clone();
        let list = scenes_list.clone();
        scenes_list
            .clone()
            .on_key_down(move |event| match super::key_of(&event) {
                Some((WXK_UP, true)) => move_scene(&app, &list, true),
                Some((WXK_DOWN, true)) => move_scene(&app, &list, false),
                Some((WXK_DELETE, _)) => delete_scene(&app, &list),
                _ => event.skip(true),
            });
    }

    // Source buttons.
    {
        let app = app.clone();
        source_add.on_click(move |_| add_source(&app));
    }
    {
        let app = app.clone();
        let list = sources_list.clone();
        source_edit.on_click(move |_| edit_source(&app, &list));
    }
    {
        let app = app.clone();
        let list = sources_list.clone();
        source_sends.on_click(move |_| {
            let scene_index = selected_scene_index(&app);
            let Some(index) = super::list::selection(&list, source_count(&app, scene_index)) else {
                return;
            };
            super::sends::edit_sends(&app, scene_index, index);
        });
    }
    {
        let app = app.clone();
        let list = sources_list.clone();
        source_remove.on_click(move |_| remove_source(&app, &list));
    }
    {
        let app = app.clone();
        let list = sources_list.clone();
        source_up.on_click(move |_| move_source(&app, &list, true));
    }
    {
        let app = app.clone();
        let list = sources_list.clone();
        source_down.on_click(move |_| move_source(&app, &list, false));
    }

    // Source list keys.
    {
        let app = app.clone();
        let list = sources_list.clone();
        sources_list
            .clone()
            .on_key_down(move |event| match super::key_of(&event) {
                Some((WXK_UP, true)) => move_source(&app, &list, true),
                Some((WXK_DOWN, true)) => move_source(&app, &list, false),
                Some((WXK_DELETE, _)) => remove_source(&app, &list),
                _ => event.skip(true),
            });
    }

    (scenes_list, sources_list)
}

/// How many sources the scene at `scene_index` has, for the bounds check in
/// [`super::list::selection`].
fn source_count(app: &Rc<App>, scene_index: usize) -> usize {
    app.config
        .borrow()
        .scenes
        .scenes
        .get(scene_index)
        .map(|scene| scene.sources.len())
        .unwrap_or(0)
}

/// The scene currently highlighted in the scenes-tab list (defaults to the
/// active scene when nothing is selected, or when the list is showing its
/// placeholder).
fn selected_scene_index(app: &Rc<App>) -> usize {
    let count = app.config.borrow().scenes.scenes.len();
    app.widgets(|w| super::list::selection(&w.scenes_list, count))
        .flatten()
        .unwrap_or_else(|| {
            let config = app.config.borrow();
            config
                .scenes
                .scenes
                .iter()
                .position(|s| s.name == config.scenes.active_scene)
                .unwrap_or(0)
        })
}

pub fn refresh_scenes_list(app: &Rc<App>) {
    app.widgets(|w| {
        let config = app.config.borrow();
        let selected = w.scenes_list.get_selection();
        let labels: Vec<String> = config
            .scenes
            .scenes
            .iter()
            .map(|scene| {
                if scene.is_default {
                    format!("{} (default)", scene.name)
                } else {
                    scene.name.clone()
                }
            })
            .collect();
        if super::list::sync(&w.scenes_list, &labels, NO_SCENES) == super::list::Synced::Kept {
            return;
        }
        // Falls back to the first scene, so the list starts out with one
        // selected rather than making the user arrow into it.
        let index = selected.unwrap_or(0);
        if index < w.scenes_list.get_count() {
            w.scenes_list.set_selection(index, true);
        }
    });
}

/// Refreshes the Sources list, in place when only the labels changed.
///
/// This is driven by the two-second application poll, so an application that
/// comes and goes from the process table used to clear and rebuild the list
/// every two seconds — restoring the selection each time, and interrupting
/// anyone arrowing through it. `list::sync` is what keeps that quiet, the same
/// in-place treatment `home::relabel_source_strips` gives the mixer strips.
pub fn refresh_sources_list(app: &Rc<App>) {
    let scene_index = selected_scene_index(app);
    let labels = {
        let config = app.config.borrow();
        match config.scenes.scenes.get(scene_index) {
            Some(scene) => {
                let ctx = app.name_context(&scene.sources);
                crate::source_name::list_labels(&scene.sources, &ctx)
            }
            None => Vec::new(),
        }
    };
    app.widgets(|w| {
        let selected = w.sources_list.get_selection();
        if super::list::sync(&w.sources_list, &labels, NO_SOURCES) == super::list::Synced::Kept {
            return;
        }
        // The list itself changed, so the focus context genuinely has too.
        if let Some(index) = selected {
            if index < w.sources_list.get_count() {
                w.sources_list.set_selection(index, true);
            }
        }
    });
}

fn active_sources(app: &Rc<App>) -> Option<Vec<SourceConfig>> {
    app.config
        .borrow()
        .scenes
        .active_scene()
        .map(|scene| scene.sources.clone())
}

fn after_scene_edit(app: &Rc<App>) {
    after_scene_edit_with_sources(app, None);
}

fn after_source_edit(app: &Rc<App>, previous_sources: Option<Vec<SourceConfig>>) {
    after_scene_edit_with_sources(app, previous_sources);
}

fn after_scene_edit_with_sources(app: &Rc<App>, previous_sources: Option<Vec<SourceConfig>>) {
    app.save_config();
    refresh_scenes_list(app);
    // The sources list is refreshed last, after `on_sources_changed` has
    // re-resolved Application sources: an edit may have named a new
    // application, and the labels read the resolved process out of that cache.
    on_sources_changed(app, previous_sources.as_deref());
    refresh_sources_list(app);
}

fn move_scene(app: &Rc<App>, list: &ListBox, up: bool) {
    let count = app.config.borrow().scenes.scenes.len();
    let Some(index) = super::list::selection(list, count) else {
        return;
    };
    let changed = {
        let mut config = app.config.borrow_mut();
        let scenes = &mut config.scenes.scenes;
        if up {
            move_up(scenes, index)
        } else {
            move_down(scenes, index)
        }
    };
    if changed == ListEdit::Changed {
        let new_index = if up { index - 1 } else { index + 1 };
        after_scene_edit(app);
        app.widgets(|w| w.scenes_list.set_selection(new_index as u32, true));
    }
}

fn delete_scene(app: &Rc<App>, list: &ListBox) {
    let count = app.config.borrow().scenes.scenes.len();
    let Some(index) = super::list::selection(list, count) else {
        return;
    };
    let changed = app.config.borrow_mut().scenes.delete_scene(index);
    if changed == ListEdit::Changed {
        after_scene_edit(app);
    }
}

fn add_scene(app: &Rc<App>) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let dialog = TextEntryDialog::builder(&frame, "Name for the new scene:", "Add scene").build();
    if dialog.show_modal() == ID_OK {
        if let Some(name) = dialog.get_value() {
            let changed = app.config.borrow_mut().scenes.add_scene(&name);
            if changed == ListEdit::Changed {
                after_scene_edit(app);
            } else if !name.trim().is_empty() {
                show_error(
                    &frame,
                    "Add scene",
                    "A scene with that name already exists.",
                );
            }
        }
    }
}

fn rename_scene(app: &Rc<App>, list: &ListBox) {
    let count = app.config.borrow().scenes.scenes.len();
    let Some(index) = super::list::selection(list, count) else {
        return;
    };
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let current = {
        let config = app.config.borrow();
        let Some(scene) = config.scenes.scenes.get(index) else {
            return;
        };
        scene.name.clone()
    };
    let dialog = TextEntryDialog::builder(&frame, "New name for the scene:", "Rename scene")
        .with_default_value(&current)
        .build();
    if dialog.show_modal() == ID_OK {
        if let Some(name) = dialog.get_value() {
            let changed = app.config.borrow_mut().scenes.rename_scene(index, &name);
            if changed == ListEdit::Changed {
                after_scene_edit(app);
            }
        }
    }
}

fn move_source(app: &Rc<App>, list: &ListBox, up: bool) {
    let scene_index = selected_scene_index(app);
    let Some(index) = super::list::selection(list, source_count(app, scene_index)) else {
        return;
    };
    let previous_sources = active_sources(app);
    let changed = {
        let mut config = app.config.borrow_mut();
        let Some(scene) = config.scenes.scenes.get_mut(scene_index) else {
            return;
        };
        if up {
            move_up(&mut scene.sources, index)
        } else {
            move_down(&mut scene.sources, index)
        }
    };
    if changed == ListEdit::Changed {
        let new_index = if up { index - 1 } else { index + 1 };
        after_source_edit(app, previous_sources);
        app.widgets(|w| w.sources_list.set_selection(new_index as u32, true));
    }
}

fn remove_source(app: &Rc<App>, list: &ListBox) {
    let scene_index = selected_scene_index(app);
    let Some(index) = super::list::selection(list, source_count(app, scene_index)) else {
        return;
    };
    let previous_sources = active_sources(app);
    {
        let mut config = app.config.borrow_mut();
        let Some(scene) = config.scenes.scenes.get_mut(scene_index) else {
            return;
        };
        if index >= scene.sources.len() {
            return;
        }
        scene.sources.remove(index);
    }
    after_source_edit(app, previous_sources);
}

/// Ensures a unique source name within a scene by appending a number.
fn unique_source_name(existing: &[SourceConfig], base: &str) -> String {
    if !existing.iter().any(|s| s.name == base) {
        return base.to_string();
    }
    for n in 2.. {
        let candidate = format!("{base} {n}");
        if !existing.iter().any(|s| s.name == candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn add_source(app: &Rc<App>) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let types = [
        "Microphone",
        "Desktop Audio",
        "Application",
        "Text-to-Speech",
        "Sound Events",
    ];
    let dialog =
        SingleChoiceDialog::builder(&frame, "What kind of source?", "Add source", &types).build();
    super::native_acc::install_in_dialog(&dialog, "What kind of source?");
    if dialog.show_modal() != ID_OK {
        return;
    }
    let kind = match dialog.get_selection() {
        0 => SourceKindConfig::Microphone { device_id: None },
        1 => SourceKindConfig::DesktopAudio,
        2 => SourceKindConfig::Application {
            process_name: String::new(),
        },
        3 => SourceKindConfig::Tts(TtsSourceConfig::default()),
        4 => SourceKindConfig::SoundEvents(SoundEventsSourceConfig::default()),
        _ => return,
    };

    let scene_index = selected_scene_index(app);
    let previous_sources = active_sources(app);
    let new_index = {
        let mut config = app.config.borrow_mut();
        let Some(scene) = config.scenes.scenes.get_mut(scene_index) else {
            return;
        };
        let name = unique_source_name(&scene.sources, kind.type_display_name());
        scene.sources.push(SourceConfig {
            name,
            kind,
            ..Default::default()
        });
        scene.sources.len() - 1
    };
    after_source_edit(app, previous_sources);
    app.widgets(|w| w.sources_list.set_selection(new_index as u32, true));
    // Open the parameter dialog right away for types that need setup.
    if let Some(w) = app.widgets(|w| w.sources_list.clone()) {
        edit_source(app, &w);
    }
}

fn edit_source(app: &Rc<App>, list: &ListBox) {
    let scene_index = selected_scene_index(app);
    let Some(index) = super::list::selection(list, source_count(app, scene_index)) else {
        return;
    };
    let kind = {
        let config = app.config.borrow();
        let Some(source) = config
            .scenes
            .scenes
            .get(scene_index)
            .and_then(|s| s.sources.get(index))
        else {
            return;
        };
        source.kind.clone()
    };

    match kind {
        SourceKindConfig::Microphone { device_id } => {
            edit_microphone(app, scene_index, index, device_id)
        }
        SourceKindConfig::Tts(tts) => edit_tts(app, scene_index, index, tts),
        SourceKindConfig::Application { process_name } => {
            edit_application(app, scene_index, index, process_name)
        }
        SourceKindConfig::DesktopAudio => {
            app.widgets(|w| {
                super::show_info(
                    &w.frame,
                    "Desktop Audio",
                    "Desktop Audio captures all system sound and has no settings.",
                )
            });
        }
        SourceKindConfig::SoundEvents(settings) => {
            edit_sound_events(app, scene_index, index, settings)
        }
    }
}

fn set_source_kind(app: &Rc<App>, scene_index: usize, source_index: usize, kind: SourceKindConfig) {
    let previous_sources = active_sources(app);
    {
        let mut config = app.config.borrow_mut();
        match config
            .scenes
            .scenes
            .get_mut(scene_index)
            .and_then(|s| s.sources.get_mut(source_index))
        {
            Some(source) => source.kind = kind,
            // The indices came from list selections taken before a dialog ran,
            // so they can go stale. Dropping the user's edit silently is the one
            // outcome that must not happen quietly.
            None => log::error!(
                "Discarding a source edit: scene {scene_index}, source {source_index} no longer exists"
            ),
        }
    }
    after_source_edit(app, previous_sources);
}
fn edit_microphone(
    app: &Rc<App>,
    scene_index: usize,
    source_index: usize,
    current: Option<String>,
) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let devices = crate::audio::device::capture_devices();
    if devices.is_empty() {
        show_error(&frame, "Microphone", "No microphones were found.");
        return;
    }
    let mut labels: Vec<String> = vec!["Default microphone".to_string()];
    labels.extend(devices.iter().map(|d| d.name.clone()));
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let dialog = SingleChoiceDialog::builder(
        &frame,
        "Which microphone should this source use?",
        "Microphone",
        &label_refs,
    )
    .build();
    super::native_acc::install_in_dialog(&dialog, "Which microphone should this source use?");
    // Preselect the current device.
    let preselect = current
        .as_deref()
        .and_then(|id| devices.iter().position(|d| d.id == id).map(|i| i + 1))
        .unwrap_or(0);
    dialog.set_selection(preselect as i32);
    if dialog.show_modal() == ID_OK {
        let selection = dialog.get_selection();
        let device_id = if selection <= 0 {
            None
        } else {
            devices.get(selection as usize - 1).map(|d| d.id.clone())
        };
        set_source_kind(
            app,
            scene_index,
            source_index,
            SourceKindConfig::Microphone { device_id },
        );
    }
}

fn edit_application(app: &Rc<App>, scene_index: usize, source_index: usize, current: String) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let process_name = match super::app_picker::pick_application(&frame, &current) {
        super::app_picker::Pick::App(name) => name,
        // Opened here, after the picker has closed, so no modal is ever nested.
        super::app_picker::Pick::TypeAName => {
            match super::app_picker::type_a_name(&frame, &current) {
                Some(name) => name,
                None => return,
            }
        }
        super::app_picker::Pick::Cancelled => return,
    };
    // Only reachable through the typed fallback now, but still worth saying: a
    // name that resolves to nothing produces a source that is silent without
    // complaining, which is the failure this dialog exists to prevent.
    if crate::audio::device::find_process(&process_name).is_none() {
        super::show_info(
            &frame,
            "Application source",
            &format!(
                "{process_name} does not appear to be running. The source will stay silent until it starts, and will be picked up automatically when it does."
            ),
        );
    }
    set_source_kind(
        app,
        scene_index,
        source_index,
        SourceKindConfig::Application { process_name },
    );
}

fn edit_tts(app: &Rc<App>, scene_index: usize, source_index: usize, current: TtsSourceConfig) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let dialog = Dialog::builder(&frame, "Text-to-Speech source")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(560, 640)
        .build();
    // Voice fetches and previews finish on the pump, which keeps running
    // inside this dialog's modal loop — and can outlive the dialog if the user
    // closes it mid-request. Cleared just before `destroy()`, so those
    // callbacks bail instead of touching freed widgets.
    let alive = Rc::new(std::cell::Cell::new(true));
    let scrolled = ScrolledWindow::builder(&dialog)
        .with_style(ScrolledWindowStyle::VScroll)
        .build();
    scrolled.set_scroll_rate(0, 12);
    let panel = Panel::builder(&scrolled).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let engine_label = StaticText::builder(&panel).with_label("Engine").build();
    let engines = crate::tts::engine_names();
    let engine_choice = Choice::builder(&panel).build();
    super::set_accessible_name(&engine_choice, "Engine");
    super::help::tag(
        &engine_choice,
        "dialog.ttsSource.engine",
        "TTS engine choice",
    );
    for (_, display) in &engines {
        engine_choice.append(display);
    }
    let selected_id = crate::tts::engines::resolve_id(&current.engine);
    let engine_index = engines
        .iter()
        .position(|(id, _)| *id == selected_id)
        .unwrap_or(0);
    engine_choice.set_selection(engine_index as u32);

    let voice_label = StaticText::builder(&panel).with_label("Voice").build();
    let voice_choice = Choice::builder(&panel).build();
    super::help::tag(&voice_choice, "dialog.ttsSource.voice", "TTS voice choice");
    // The list backing the picker; index 0 of the control is "Default voice",
    // so a selection of n maps to `voices[n - 1]`.
    let voices: Rc<std::cell::RefCell<Vec<crate::tts::engine::Voice>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));
    fill_voice_choice(&voice_choice, &voices, selected_id, "", &current.voice);
    // How many voices this engine has, as a real label rather than only an
    // accessible name — and refreshed with the engine, so it can never report
    // the previous engine's count.
    let voice_count_label = StaticText::builder(&panel).build();
    update_voice_status(&voice_count_label, &voice_choice, selected_id, "");

    let volume_label = StaticText::builder(&panel)
        .with_label("Voice volume")
        .build();
    let volume_slider = Slider::builder(&panel)
        .with_value(current.volume as i32)
        .with_min_value(0)
        .with_max_value(100)
        .build();
    super::set_accessible_name(&volume_slider, "Voice volume");
    super::help::tag(
        &volume_slider,
        "dialog.ttsSource.volume",
        "TTS voice volume slider",
    );
    let volume_announcer = wire_slider(&volume_slider, "Voice volume", "%", 0, 100, 10);

    let rate_label = StaticText::builder(&panel)
        .with_label("Speech rate (-10 to 10)")
        .build();
    let rate_slider = Slider::builder(&panel)
        .with_value(current.rate)
        .with_min_value(-10)
        .with_max_value(10)
        .build();
    super::set_accessible_name(&rate_slider, "Speech rate");
    super::help::tag(
        &rate_slider,
        "dialog.ttsSource.rate",
        "TTS speech rate slider",
    );
    // Page step of 2 rather than 10: the whole range is only 20 wide.
    let rate_announcer = wire_slider(&rate_slider, "Speech rate", "", -10, 10, 2);

    let pitch_label = StaticText::builder(&panel)
        .with_label("Voice pitch (-50 to 50)")
        .build();
    let pitch_slider = Slider::builder(&panel)
        .with_value(current.pitch)
        .with_min_value(-50)
        .with_max_value(50)
        .build();
    super::help::tag(
        &pitch_slider,
        "dialog.ttsSource.pitch",
        "TTS voice pitch slider",
    );
    let pitch_announcer = wire_slider(&pitch_slider, "Voice pitch", "", -50, 50, 10);
    // Named per engine: not every engine has a pitch control, and a slider
    // that silently does nothing is worse than one that says so.
    set_pitch_name(&pitch_slider, &pitch_announcer, selected_id);

    let provider_controls =
        TtsProviderControls::build(&panel, &current, &app.config.borrow().speech, selected_id);
    provider_controls.show(selected_id);

    let output_check = CheckBox::builder(&panel)
        .with_label("Send speech to the stream")
        .build();
    // The visual label alone is not announced by screen readers here; give
    // the control an explicit accessible name.
    super::set_accessible_name(&output_check, "Send speech to the stream");
    super::help::tag(
        &output_check,
        "dialog.ttsSource.toStream",
        "Send speech to the stream checkbox",
    );
    output_check.set_value(current.output_to_stream);

    let preview = Button::builder(&panel).with_label("&Preview voice").build();
    super::set_accessible_name(&preview, "Preview voice");
    super::help::tag(&preview, "dialog.ttsSource.preview", "Preview voice button");

    let reset = Button::builder(&panel)
        .with_label("&Reset this engine to defaults")
        .build();
    super::set_accessible_name(&reset, "Reset this engine to defaults");
    super::help::tag(
        &reset,
        "dialog.ttsSource.reset",
        "Reset this engine to defaults button",
    );

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let ok = super::ok_button(&panel, "OK");
    // `ID_CANCEL` is what wx maps Escape to; without it Escape does nothing.
    let cancel = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("Cancel")
        .build();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);

    sizer.add(&engine_label, 0, SizerFlag::All, 4);
    sizer.add(&engine_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&voice_label, 0, SizerFlag::All, 4);
    sizer.add(&voice_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&voice_count_label, 0, SizerFlag::All, 4);
    sizer.add(&volume_label, 0, SizerFlag::All, 4);
    sizer.add(&volume_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&rate_label, 0, SizerFlag::All, 4);
    sizer.add(&rate_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&pitch_label, 0, SizerFlag::All, 4);
    sizer.add(&pitch_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    for provider_panel in provider_controls.panels() {
        sizer.add(&provider_panel, 0, SizerFlag::Expand | SizerFlag::All, 4);
    }
    sizer.add(&output_check, 0, SizerFlag::All, 8);
    sizer.add(&preview, 0, SizerFlag::All, 4);
    sizer.add(&reset, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);
    panel.set_sizer(sizer, true);
    let scrolled_sizer = BoxSizer::builder(Orientation::Vertical).build();
    scrolled_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    scrolled.set_sizer(scrolled_sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&scrolled, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    let rate_supported = rate_is_supported(selected_id);
    rate_label.show(rate_supported);
    rate_slider.show(rate_supported);
    let initial_pitch_supported = pitch_is_supported(selected_id)
        && (selected_id != crate::tts::engines::AWS
            || matches!(
                provider_controls.polly_engine_value().as_str(),
                "" | "standard"
            ));
    pitch_label.show(initial_pitch_supported);
    pitch_slider.show(initial_pitch_supported);

    // Reads the engine id the picker currently shows.
    let engines_for_read = engines.clone();
    let engine_choice_for_read = engine_choice.clone();
    let selected_engine = move || -> &'static str {
        engines_for_read
            .get(
                engine_choice_for_read
                    .get_selection()
                    .map(|i| i as usize)
                    .unwrap_or(0),
            )
            .map(|(id, _)| *id)
            .unwrap_or(crate::tts::engines::SAPI)
    };

    // Switching engines invalidates the voice list: an ElevenLabs voice id
    // means nothing to Azure. Rebuilding it is not cheap, though — a few
    // hundred native combobox inserts plus a fresh MSAA object — and a wxChoice
    // fires a selection change on *every* arrow key, so doing the work inline
    // made arrowing from SAPI to Star pay for it nine times. Instead:
    //
    //   * `applied` records which engine the voice picker, count label and
    //     pitch name currently reflect, so passing back through where you
    //     started costs a pointer compare;
    //   * `settle` is a one-shot timer restarted on each keypress, so the work
    //     happens once, shortly after the user stops;
    //   * leaving the control (Tab) applies immediately, because a user who has
    //     moved on should not have to wait out a timer.
    //
    // A timer rather than a `run_when_ready` deadline: the pump is idle-driven,
    // and no idle follows the user's last arrow key, so a polled deadline would
    // never come due.
    let applied = Rc::new(std::cell::Cell::new(selected_id));
    // What each engine is set to. Seeded from the source's saved sections, so
    // an engine configured in an earlier sitting comes back as it was left, and
    // written back for every engine on OK — arrowing *through* an engine on the
    // way to another one must not throw away what is stored there either.
    let profiles: Rc<std::cell::RefCell<HashMap<&'static str, TtsEngineProfile>>> = {
        let speech = &app.config.borrow().speech;
        let mut seeded: HashMap<&'static str, TtsEngineProfile> = crate::tts::engine_names()
            .iter()
            .map(|(id, _)| {
                let saved = current.engines.get(*id).cloned();
                (
                    *id,
                    saved.unwrap_or_else(|| default_profile(id, Some(speech))),
                )
            })
            .collect();
        // The flat fields win for the selected engine: they are what the app is
        // actually speaking with, whatever the section says.
        seeded.insert(selected_id, current.active_profile());
        Rc::new(std::cell::RefCell::new(seeded))
    };
    let pitch_supported = Rc::new(std::cell::Cell::new(pitch_is_supported(selected_id)));

    // Copies the widgets into the profile of whichever engine they are showing.
    let stash: Rc<dyn Fn()> = {
        let voice_choice = voice_choice.clone();
        let voices = voices.clone();
        let volume_slider = volume_slider.clone();
        let rate_slider = rate_slider.clone();
        let pitch_slider = pitch_slider.clone();
        let provider_controls = provider_controls.clone();
        let applied = applied.clone();
        let profiles = profiles.clone();
        Rc::new(move || {
            let engine = applied.get();
            profiles.borrow_mut().insert(
                engine,
                TtsEngineProfile {
                    voice: selected_voice(&voice_choice, &voices),
                    volume: volume_slider.value().clamp(0, 100) as u32,
                    rate: rate_slider.value().clamp(-10, 10),
                    pitch: pitch_slider.value().clamp(-50, 50),
                    settings: provider_controls.settings(engine),
                },
            );
        })
    };

    let apply_engine: Rc<dyn Fn()> = {
        let voice_choice = voice_choice.clone();
        let voice_count_label = voice_count_label.clone();
        let pitch_slider = pitch_slider.clone();
        let pitch_label = pitch_label.clone();
        let rate_slider = rate_slider.clone();
        let rate_label = rate_label.clone();
        let pitch_announcer = pitch_announcer.clone();
        let voices = voices.clone();
        let selected_engine = selected_engine.clone();
        let applied = applied.clone();
        let profiles = profiles.clone();
        let pitch_supported = pitch_supported.clone();
        let provider_controls = provider_controls.clone();
        let volume_slider = volume_slider.clone();
        let volume_announcer = volume_announcer.clone();
        let rate_announcer = rate_announcer.clone();
        let panel = panel.clone();
        Rc::new(move || {
            let engine = selected_engine();
            if engine == applied.get() {
                return;
            }
            let profile = profiles.borrow().get(engine).cloned().unwrap_or_default();
            // The provider panel first: its model is what the voice list below
            // is filtered by, so a stale one would list the wrong voices.
            provider_controls.load(engine, profile.settings.as_ref());
            set_slider(
                &volume_slider,
                &volume_announcer,
                profile.volume as i32,
                "%",
            );
            set_slider(&rate_slider, &rate_announcer, profile.rate, "");
            set_slider(&pitch_slider, &pitch_announcer, profile.pitch, "");
            let wanted = profile.voice;
            voice_choice.freeze();
            fill_voice_choice(
                &voice_choice,
                &voices,
                engine,
                &provider_controls.model_value(engine),
                &wanted,
            );
            voice_choice.thaw();
            update_voice_status(
                &voice_count_label,
                &voice_choice,
                engine,
                &provider_controls.model_value(engine),
            );
            provider_controls.show(engine);
            let supported = pitch_is_supported(engine)
                && (engine != crate::tts::engines::AWS
                    || matches!(
                        provider_controls.polly_engine_value().as_str(),
                        "" | "standard"
                    ));
            if supported != pitch_supported.replace(supported) {
                set_pitch_name(&pitch_slider, &pitch_announcer, engine);
            }
            pitch_label.show(supported);
            pitch_slider.show(supported);
            let rate_supported = rate_is_supported(engine);
            rate_label.show(rate_supported);
            rate_slider.show(rate_supported);
            if engine == crate::tts::engines::AZURE {
                let selected = voice_choice
                    .get_selection()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| voices.borrow().get(index as usize).cloned());
                provider_controls.refresh_azure_voice(selected.as_ref());
            }
            panel.layout();
            applied.set(engine);
        })
    };

    {
        let controls = provider_controls.clone();
        let choice = voice_choice.clone();
        let voices_for_model = voices.clone();
        let count_label = voice_count_label.clone();
        provider_controls
            .eleven_model
            .clone()
            .on_selection_changed(move |_| {
                let engine = crate::tts::engines::ELEVENLABS;
                let wanted = selected_voice(&choice, &voices_for_model);
                let model = controls.model_value(engine);
                fill_voice_choice(&choice, &voices_for_model, engine, &model, &wanted);
                update_voice_status(&count_label, &choice, engine, &model);
                controls.refresh_compatibility();
            });
    }
    {
        let controls = provider_controls.clone();
        let choice = voice_choice.clone();
        let voices_for_model = voices.clone();
        let count_label = voice_count_label.clone();
        provider_controls
            .openai_model
            .clone()
            .on_selection_changed(move |_| {
                let engine = crate::tts::engines::OPENAI;
                let wanted = selected_voice(&choice, &voices_for_model);
                let model = controls.model_value(engine);
                fill_voice_choice(&choice, &voices_for_model, engine, &model, &wanted);
                update_voice_status(&count_label, &choice, engine, &model);
                controls.refresh_compatibility();
            });
    }
    {
        let controls = provider_controls.clone();
        provider_controls
            .azure_style
            .clone()
            .on_selection_changed(move |_| {
                controls.refresh_compatibility();
            });
    }
    {
        let controls = provider_controls.clone();
        let pitch_slider = pitch_slider.clone();
        let pitch_label = pitch_label.clone();
        let choice = voice_choice.clone();
        let voices_for_model = voices.clone();
        let count_label = voice_count_label.clone();
        provider_controls
            .polly_engine
            .clone()
            .on_selection_changed(move |_| {
                let engine = crate::tts::engines::AWS;
                let wanted = selected_voice(&choice, &voices_for_model);
                let model = controls.model_value(engine);
                fill_voice_choice(&choice, &voices_for_model, engine, &model, &wanted);
                update_voice_status(&count_label, &choice, engine, &model);
                let supported = matches!(controls.polly_engine_value().as_str(), "" | "standard");
                pitch_label.show(supported);
                pitch_slider.show(supported);
            });
    }
    {
        let controls = provider_controls.clone();
        let voice_choice = voice_choice.clone();
        let voices = voices.clone();
        voice_choice.clone().on_selection_changed(move |_| {
            let selected = voice_choice
                .get_selection()
                .and_then(|index| index.checked_sub(1))
                .and_then(|index| voices.borrow().get(index as usize).cloned());
            controls.refresh_azure_voice(selected.as_ref());
        });
    }

    // Owned here so it dies with the dialog: a timer whose owner window has
    // been destroyed keeps firing into freed memory (see the pump timer in
    // `ui/mod.rs`). Stopped explicitly before `destroy()` below.
    let settle = Rc::new(Timer::new(&dialog));
    {
        let apply_engine = apply_engine.clone();
        let alive = alive.clone();
        settle.on_tick(move |_| {
            if alive.get() {
                apply_engine();
            }
        });
    }

    {
        let stash = stash.clone();
        let settle = settle.clone();
        engine_choice.clone().on_selection_changed(move |_| {
            // Remember what the engine we are leaving was set to, then do
            // nothing else until the user settles.
            stash();
            settle.start(SETTLE_MS, true);
        });
    }

    {
        let apply_engine = apply_engine.clone();
        let alive = alive.clone();
        engine_choice.clone().on_kill_focus(move |event| {
            if alive.get() {
                apply_engine();
            }
            event.skip(true);
        });
    }

    let catalog_generation = Rc::new(std::cell::Cell::new(crate::tts::catalog::generation()));
    {
        let alive = alive.clone();
        let generation = catalog_generation.clone();
        let selected_engine = selected_engine.clone();
        let voice_choice = voice_choice.clone();
        let voices = voices.clone();
        let provider_controls = provider_controls.clone();
        let voice_count_label = voice_count_label.clone();
        super::run_when_ready(move || {
            if !alive.get() {
                return true;
            }
            let current_generation = crate::tts::catalog::generation();
            if current_generation != generation.get() {
                generation.set(current_generation);
                let engine = selected_engine();
                let wanted = selected_voice(&voice_choice, &voices);
                provider_controls.refresh_catalog();
                fill_voice_choice(
                    &voice_choice,
                    &voices,
                    engine,
                    &provider_controls.model_value(engine),
                    &wanted,
                );
                update_voice_status(
                    &voice_count_label,
                    &voice_choice,
                    engine,
                    &provider_controls.model_value(engine),
                );
            }
            false
        });
    }
    {
        let app = app.clone();
        let panel = panel.clone();
        let voice_choice = voice_choice.clone();
        let voices = voices.clone();
        let volume_slider = volume_slider.clone();
        let rate_slider = rate_slider.clone();
        let pitch_slider = pitch_slider.clone();
        let selected_engine = selected_engine.clone();
        let alive = alive.clone();
        let apply_engine = apply_engine.clone();
        let provider_controls = provider_controls.clone();
        preview.on_click(move |_| {
            apply_engine();
            let engine = selected_engine();
            let synth = crate::tts::engine::SynthRequest {
                text: "Pubsplash text to speech is working.".into(),
                voice: selected_voice(&voice_choice, &voices),
                rate: rate_slider.value().clamp(-10, 10),
                volume: volume_slider.value().clamp(0, 100) as u32,
                pitch: pitch_slider.value().clamp(-50, 50),
                provider_settings: provider_controls.settings(engine),
            };
            preview_voice(&app, &panel, engine, synth, &alive);
        });
    }

    {
        // Resets the engine on show, and only that one: the other engines' saved
        // sections are what makes switching lossless, and a reset button that
        // quietly cleared them all would undo exactly that. Nothing is written
        // until OK, so Cancel takes it back.
        let panel = panel.clone();
        let voice_choice = voice_choice.clone();
        let voice_count_label = voice_count_label.clone();
        let voices = voices.clone();
        let volume_slider = volume_slider.clone();
        let rate_slider = rate_slider.clone();
        let pitch_slider = pitch_slider.clone();
        let rate_label = rate_label.clone();
        let pitch_label = pitch_label.clone();
        let volume_announcer = volume_announcer.clone();
        let rate_announcer = rate_announcer.clone();
        let pitch_announcer = pitch_announcer.clone();
        let selected_engine = selected_engine.clone();
        let apply_engine = apply_engine.clone();
        let provider_controls = provider_controls.clone();
        let pitch_supported = pitch_supported.clone();
        let stash = stash.clone();
        reset.on_click(move |_| {
            // The picker may still be mid-settle, in which case the widgets are
            // showing the engine we are *leaving*.
            apply_engine();
            let engine = selected_engine();
            let profile = default_profile(engine, None);
            provider_controls.load(engine, profile.settings.as_ref());
            set_slider(
                &volume_slider,
                &volume_announcer,
                profile.volume as i32,
                "%",
            );
            set_slider(&rate_slider, &rate_announcer, profile.rate, "");
            set_slider(&pitch_slider, &pitch_announcer, profile.pitch, "");
            fill_voice_choice(
                &voice_choice,
                &voices,
                engine,
                &provider_controls.model_value(engine),
                &profile.voice,
            );
            update_voice_status(
                &voice_count_label,
                &voice_choice,
                engine,
                &provider_controls.model_value(engine),
            );
            // Polly's engine mode decides whether pitch applies, and the reset
            // has just put it back to the provider default.
            let supported = pitch_is_supported(engine);
            if supported != pitch_supported.replace(supported) {
                set_pitch_name(&pitch_slider, &pitch_announcer, engine);
            }
            pitch_label.show(supported);
            pitch_slider.show(supported);
            let rate_supported = rate_is_supported(engine);
            rate_label.show(rate_supported);
            rate_slider.show(rate_supported);
            if engine == crate::tts::engines::AZURE {
                provider_controls.refresh_azure_voice(None);
            }
            panel.layout();
            stash();
            super::help::announce(&format!(
                "{} settings reset to defaults.",
                crate::tts::engines::display_name(engine)
            ));
        });
    }

    {
        let dialog = dialog.clone();
        ok.on_click(move |_| dialog.end_modal(ID_OK));
    }
    {
        let dialog = dialog.clone();
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    let outcome = dialog.show_modal();
    // Nothing may have settled yet if OK was pressed straight after an arrow
    // key; `selected_voice` below would then read the old engine's list.
    apply_engine();
    if outcome == ID_OK {
        // Every engine is saved, not only the selected one, so coming back to
        // one configured earlier finds it as it was left. The selected engine's
        // section is also mirrored into the flat fields, which are what the
        // running app speaks with.
        stash();
        let engine = selected_engine();
        let profiles = profiles.borrow();
        let active = profiles.get(engine).cloned().unwrap_or_default();
        set_source_kind(
            app,
            scene_index,
            source_index,
            SourceKindConfig::Tts(TtsSourceConfig {
                engine: engine.to_string(),
                voice: active.voice,
                volume: active.volume,
                rate: active.rate,
                pitch: active.pitch,
                provider_settings: active.settings,
                engines: profiles
                    .iter()
                    .map(|(id, profile)| ((*id).to_string(), profile.clone()))
                    .collect(),
                output_to_stream: output_check.get_value(),
            }),
        );
    }
    alive.set(false);
    settle.stop();
    // The provider registry is keyed by HWND, so it must let go of these sliders
    // before the window is destroyed and the handles can be recycled.
    volume_announcer.uninstall();
    rate_announcer.uninstall();
    pitch_announcer.uninstall();
    dialog.destroy();
}

/// How long the engine picker waits after the last keypress before rebuilding
/// the voice list. Long enough to arrow through all nine engines without
/// stopping, short enough to feel immediate once you land.
const SETTLE_MS: i32 = 300;

fn rate_is_supported(engine: &str) -> bool {
    !matches!(
        engine,
        crate::tts::engines::GTTS | crate::tts::engines::STAR
    )
}

/// What an engine is set to before the user has configured it.
///
/// `speech` carries the three global fallbacks a source used before per-source
/// provider settings existed, so an engine seen for the first time starts from
/// what the user already told the app globally. Pass `None` — the reset button
/// does — for the factory values with no such carry-over.
fn default_profile(engine: &str, speech: Option<&crate::config::SpeechConfig>) -> TtsEngineProfile {
    use crate::tts::engines;
    let settings = match engine {
        engines::ELEVENLABS => Some(TtsEngineSettings::ElevenLabs(ElevenLabsTtsSettings {
            model: speech
                .map(|speech| speech.elevenlabs_model.clone())
                .unwrap_or_default(),
            ..Default::default()
        })),
        engines::OPENAI => Some(TtsEngineSettings::OpenAi(OpenAiTtsSettings::default())),
        engines::AZURE => Some(TtsEngineSettings::Azure(AzureTtsSettings::default())),
        engines::GOOGLE => Some(TtsEngineSettings::Google(GoogleTtsSettings {
            language_code: speech
                .map(|speech| speech.google_language_code.clone())
                .unwrap_or_default(),
            ..Default::default()
        })),
        engines::AWS => Some(TtsEngineSettings::Polly(PollyTtsSettings {
            engine: speech
                .map(|speech| speech.aws_engine.clone())
                .unwrap_or_default(),
            ..Default::default()
        })),
        engines::GTTS => Some(TtsEngineSettings::Gtts(GttsTtsSettings::default())),
        _ => None,
    };
    TtsEngineProfile {
        settings,
        ..Default::default()
    }
}

/// Sets a slider's value without speaking it, keeping the announced value in
/// step. The user did not press a key — the engine changed under them, or they
/// pressed Reset, which announces itself — so a value event here would talk
/// over what they are actually doing.
fn set_slider(slider: &Slider, announcer: &SliderAnnouncer, value: i32, suffix: &str) {
    slider.set_value(value);
    announcer.set_value_text(&format!("{value}{suffix}"));
}

/// Whether an engine honours the pitch slider at all.
fn pitch_is_supported(engine: &str) -> bool {
    use crate::tts::engines;
    matches!(
        engine,
        engines::EDGE | engines::AZURE | engines::GOOGLE | engines::AWS
    )
}

#[derive(Clone)]
struct TtsProviderControls {
    none_panel: Panel,
    eleven_panel: Panel,
    eleven_model: Choice,
    eleven_language: TextCtrl,
    eleven_stability_override: CheckBox,
    eleven_stability: SpinCtrlDouble,
    eleven_similarity_override: CheckBox,
    eleven_similarity: SpinCtrlDouble,
    eleven_style_override: CheckBox,
    eleven_style: SpinCtrlDouble,
    eleven_boost: Choice,
    eleven_stream: CheckBox,
    openai_panel: Panel,
    openai_model: Choice,
    openai_instructions: TextCtrl,
    azure_panel: Panel,
    azure_style: Choice,
    azure_degree: SpinCtrlDouble,
    azure_role: Choice,
    google_panel: Panel,
    google_language: TextCtrl,
    google_effect: Choice,
    polly_panel: Panel,
    polly_engine: Choice,
    polly_language: TextCtrl,
    gtts_panel: Panel,
    gtts_tld: TextCtrl,
    gtts_speed: Choice,
}

impl TtsProviderControls {
    fn build(
        parent: &Panel,
        current: &TtsSourceConfig,
        speech: &crate::config::SpeechConfig,
        selected_engine: &str,
    ) -> Self {
        let eleven = match &current.provider_settings {
            Some(TtsEngineSettings::ElevenLabs(settings)) => settings.clone(),
            None if selected_engine == crate::tts::engines::ELEVENLABS => ElevenLabsTtsSettings {
                model: speech.elevenlabs_model.clone(),
                ..Default::default()
            },
            _ => ElevenLabsTtsSettings::default(),
        };
        let openai = match &current.provider_settings {
            Some(TtsEngineSettings::OpenAi(settings)) => settings.clone(),
            None if selected_engine == crate::tts::engines::OPENAI => OpenAiTtsSettings {
                model: "tts-1".into(),
                instructions: String::new(),
            },
            _ => OpenAiTtsSettings::default(),
        };
        let azure = match &current.provider_settings {
            Some(TtsEngineSettings::Azure(settings)) => settings.clone(),
            _ => AzureTtsSettings::default(),
        };
        let google = match &current.provider_settings {
            Some(TtsEngineSettings::Google(settings)) => settings.clone(),
            None if selected_engine == crate::tts::engines::GOOGLE => GoogleTtsSettings {
                language_code: speech.google_language_code.clone(),
                ..Default::default()
            },
            _ => GoogleTtsSettings::default(),
        };
        let polly = match &current.provider_settings {
            Some(TtsEngineSettings::Polly(settings)) => settings.clone(),
            None if selected_engine == crate::tts::engines::AWS => PollyTtsSettings {
                engine: speech.aws_engine.clone(),
                ..Default::default()
            },
            _ => PollyTtsSettings::default(),
        };
        let gtts = match &current.provider_settings {
            Some(TtsEngineSettings::Gtts(settings)) => settings.clone(),
            _ => GttsTtsSettings::default(),
        };

        let none_panel = Panel::builder(parent).build();
        let none_sizer = BoxSizer::builder(Orientation::Vertical).build();
        let none_label = StaticText::builder(&none_panel)
            .with_label("This engine has no additional voice settings.")
            .build();
        none_sizer.add(&none_label, 0, SizerFlag::All, 4);
        none_panel.set_sizer(none_sizer, true);

        let eleven_panel = Panel::builder(parent).build();
        let (eleven_sizer, eleven_box) =
            super::group_box(&eleven_panel, "ElevenLabs voice settings");
        let eleven_model = Choice::builder(&eleven_box).build();
        fill_model_choice(
            &eleven_model,
            crate::tts::engines::ELEVENLABS,
            &eleven.model,
        );
        super::set_accessible_name(
            &eleven_model,
            "ElevenLabs model; blank uses provider default",
        );
        super::help::tag(
            &eleven_model,
            "dialog.ttsSource.elevenModel",
            "ElevenLabs model",
        );
        eleven_sizer.add(
            &StaticText::builder(&eleven_box)
                .with_label("Model (blank uses provider default)")
                .build(),
            0,
            SizerFlag::All,
            3,
        );
        eleven_sizer.add(&eleven_model, 0, SizerFlag::Expand | SizerFlag::All, 3);
        let eleven_language = TextCtrl::builder(&eleven_box)
            .with_value(&eleven.language_code)
            .build();
        super::set_accessible_name(
            &eleven_language,
            "ElevenLabs language code; blank uses provider default",
        );
        super::help::tag(
            &eleven_language,
            "dialog.ttsSource.elevenLanguage",
            "ElevenLabs language code",
        );
        eleven_sizer.add(
            &StaticText::builder(&eleven_box)
                .with_label("Language code (optional)")
                .build(),
            0,
            SizerFlag::All,
            3,
        );
        eleven_sizer.add(&eleven_language, 0, SizerFlag::Expand | SizerFlag::All, 3);

        let eleven_stability_override = CheckBox::builder(&eleven_box)
            .with_label("Override stability")
            .build();
        super::set_accessible_name(&eleven_stability_override, "Override stability");
        eleven_stability_override.set_value(eleven.stability.is_some());
        let eleven_stability = SpinCtrlDouble::builder(&eleven_box)
            .with_range(0.0, 1.0)
            .build();
        eleven_stability.set_value(eleven.stability.unwrap_or(0.5));
        eleven_stability.set_digits(2);
        eleven_stability.set_increment(0.05);
        eleven_stability.enable(eleven.stability.is_some());
        super::set_accessible_name(&eleven_stability, "ElevenLabs stability, zero to one");
        super::help::tag(
            &eleven_stability,
            "dialog.ttsSource.elevenStability",
            "ElevenLabs stability",
        );
        eleven_sizer.add(&eleven_stability_override, 0, SizerFlag::All, 3);
        eleven_sizer.add(&eleven_stability, 0, SizerFlag::All, 3);

        let eleven_similarity_override = CheckBox::builder(&eleven_box)
            .with_label("Override similarity boost")
            .build();
        super::set_accessible_name(&eleven_similarity_override, "Override similarity boost");
        eleven_similarity_override.set_value(eleven.similarity_boost.is_some());
        let eleven_similarity = SpinCtrlDouble::builder(&eleven_box)
            .with_range(0.0, 1.0)
            .build();
        eleven_similarity.set_value(eleven.similarity_boost.unwrap_or(0.75));
        eleven_similarity.set_digits(2);
        eleven_similarity.set_increment(0.05);
        eleven_similarity.enable(eleven.similarity_boost.is_some());
        super::set_accessible_name(
            &eleven_similarity,
            "ElevenLabs similarity boost, zero to one",
        );
        super::help::tag(
            &eleven_similarity,
            "dialog.ttsSource.elevenSimilarity",
            "ElevenLabs similarity boost",
        );
        eleven_sizer.add(&eleven_similarity_override, 0, SizerFlag::All, 3);
        eleven_sizer.add(&eleven_similarity, 0, SizerFlag::All, 3);

        let eleven_style_override = CheckBox::builder(&eleven_box)
            .with_label("Override style exaggeration")
            .build();
        super::set_accessible_name(&eleven_style_override, "Override style exaggeration");
        eleven_style_override.set_value(eleven.style.is_some());
        let eleven_style = SpinCtrlDouble::builder(&eleven_box)
            .with_range(0.0, 1.0)
            .build();
        eleven_style.set_value(eleven.style.unwrap_or(0.0));
        eleven_style.set_digits(2);
        eleven_style.set_increment(0.05);
        eleven_style.enable(eleven.style.is_some());
        super::set_accessible_name(&eleven_style, "ElevenLabs style exaggeration, zero to one");
        super::help::tag(
            &eleven_style,
            "dialog.ttsSource.elevenStyle",
            "ElevenLabs style exaggeration",
        );
        eleven_sizer.add(&eleven_style_override, 0, SizerFlag::All, 3);
        eleven_sizer.add(&eleven_style, 0, SizerFlag::All, 3);

        let eleven_boost = Choice::builder(&eleven_box).build();
        for label in [
            "Speaker boost: provider default",
            "Speaker boost: on",
            "Speaker boost: off",
        ] {
            eleven_boost.append(label);
        }
        eleven_boost.set_selection(match eleven.speaker_boost {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        });
        super::set_accessible_name(&eleven_boost, "ElevenLabs speaker boost");
        super::help::tag(
            &eleven_boost,
            "dialog.ttsSource.elevenBoost",
            "ElevenLabs speaker boost",
        );
        eleven_sizer.add(&eleven_boost, 0, SizerFlag::Expand | SizerFlag::All, 3);

        // Last in the panel, so turning it on or off never shifts the controls
        // above it in Tab order.
        let eleven_stream = CheckBox::builder(&eleven_box)
            .with_label("Stream audio as it is generated")
            .build();
        super::set_accessible_name(&eleven_stream, "Stream audio as it is generated");
        eleven_stream.set_value(eleven.stream);
        super::help::tag(
            &eleven_stream,
            "dialog.ttsSource.elevenStream",
            "Stream ElevenLabs audio",
        );
        eleven_sizer.add(&eleven_stream, 0, SizerFlag::All, 3);
        eleven_panel.set_sizer(eleven_sizer, true);

        bind_optional_double(&eleven_stability_override, &eleven_stability);
        bind_optional_double(&eleven_similarity_override, &eleven_similarity);
        bind_optional_double(&eleven_style_override, &eleven_style);

        let openai_panel = Panel::builder(parent).build();
        let (openai_sizer, openai_box) = super::group_box(&openai_panel, "OpenAI voice settings");
        let openai_model = Choice::builder(&openai_box).build();
        fill_model_choice(&openai_model, crate::tts::engines::OPENAI, &openai.model);
        super::set_accessible_name(&openai_model, "OpenAI speech model");
        super::help::tag(
            &openai_model,
            "dialog.ttsSource.openaiModel",
            "OpenAI speech model",
        );
        openai_sizer.add(
            &StaticText::builder(&openai_box).with_label("Model").build(),
            0,
            SizerFlag::All,
            3,
        );
        openai_sizer.add(&openai_model, 0, SizerFlag::Expand | SizerFlag::All, 3);
        let openai_instructions = TextCtrl::builder(&openai_box)
            .with_value(&openai.instructions)
            .build();
        super::set_accessible_name(&openai_instructions, "OpenAI voice instructions");
        super::help::tag(
            &openai_instructions,
            "dialog.ttsSource.openaiInstructions",
            "OpenAI voice instructions",
        );
        openai_sizer.add(
            &StaticText::builder(&openai_box)
                .with_label("Voice instructions (GPT-4o mini TTS)")
                .build(),
            0,
            SizerFlag::All,
            3,
        );
        openai_sizer.add(
            &openai_instructions,
            0,
            SizerFlag::Expand | SizerFlag::All,
            3,
        );
        openai_panel.set_sizer(openai_sizer, true);

        let azure_panel = Panel::builder(parent).build();
        let (azure_sizer, azure_box) = super::group_box(&azure_panel, "Azure voice settings");
        let azure_style = Choice::builder(&azure_box).build();
        super::set_accessible_name(&azure_style, "Azure speaking style");
        fill_default_choice(&azure_style, &[], &azure.style, "Default speaking style");
        super::help::tag(
            &azure_style,
            "dialog.ttsSource.azureStyle",
            "Azure speaking style",
        );
        azure_sizer.add(&azure_style, 0, SizerFlag::Expand | SizerFlag::All, 3);
        let azure_degree = SpinCtrlDouble::builder(&azure_box)
            .with_range(0.01, 2.0)
            .build();
        azure_degree.set_value(azure.style_degree.clamp(0.01, 2.0));
        azure_degree.set_digits(2);
        azure_degree.set_increment(0.05);
        super::set_accessible_name(
            &azure_degree,
            "Azure style intensity, zero point zero one to two",
        );
        super::help::tag(
            &azure_degree,
            "dialog.ttsSource.azureDegree",
            "Azure style intensity",
        );
        azure_sizer.add(
            &StaticText::builder(&azure_box)
                .with_label("Style intensity")
                .build(),
            0,
            SizerFlag::All,
            3,
        );
        azure_sizer.add(&azure_degree, 0, SizerFlag::All, 3);
        let azure_role = Choice::builder(&azure_box).build();
        super::set_accessible_name(&azure_role, "Azure speaking role");
        fill_default_choice(&azure_role, &[], &azure.role, "Default speaking role");
        super::help::tag(
            &azure_role,
            "dialog.ttsSource.azureRole",
            "Azure speaking role",
        );
        azure_sizer.add(&azure_role, 0, SizerFlag::Expand | SizerFlag::All, 3);
        azure_panel.set_sizer(azure_sizer, true);

        let google_panel = Panel::builder(parent).build();
        let (google_sizer, google_box) =
            super::group_box(&google_panel, "Google Cloud voice settings");
        let google_language = TextCtrl::builder(&google_box)
            .with_value(&google.language_code)
            .build();
        super::set_accessible_name(
            &google_language,
            "Google Cloud language code; blank infers it from the voice",
        );
        super::help::tag(
            &google_language,
            "dialog.ttsSource.googleLanguage",
            "Google Cloud language code",
        );
        google_sizer.add(
            &StaticText::builder(&google_box)
                .with_label("Language code (blank infers from voice)")
                .build(),
            0,
            SizerFlag::All,
            3,
        );
        google_sizer.add(&google_language, 0, SizerFlag::Expand | SizerFlag::All, 3);
        let google_effect = Choice::builder(&google_box).build();
        super::set_accessible_name(&google_effect, "Google Cloud audio effects profile");
        google_effect.append("No effects profile");
        let mut effect_selection = 0;
        for (index, (id, label)) in crate::tts::engines::google::EFFECTS_PROFILES
            .iter()
            .enumerate()
        {
            google_effect.append(label);
            if *id == google.effects_profile {
                effect_selection = index + 1;
            }
        }
        google_effect.set_selection(effect_selection as u32);
        super::help::tag(
            &google_effect,
            "dialog.ttsSource.googleEffect",
            "Google Cloud audio effects profile",
        );
        google_sizer.add(
            &StaticText::builder(&google_box)
                .with_label("Audio effects profile")
                .build(),
            0,
            SizerFlag::All,
            3,
        );
        google_sizer.add(&google_effect, 0, SizerFlag::Expand | SizerFlag::All, 3);
        google_panel.set_sizer(google_sizer, true);

        let polly_panel = Panel::builder(parent).build();
        let (polly_sizer, polly_box) = super::group_box(&polly_panel, "AWS Polly voice settings");
        let polly_engine = Choice::builder(&polly_box).build();
        // Filled the same way as every other model picker — including its
        // accessible name, so nothing here may pre-populate it: `clear()` is the
        // first thing `fill_model_choice` does.
        fill_model_choice(&polly_engine, crate::tts::engines::AWS, &polly.engine);
        super::help::tag(
            &polly_engine,
            "dialog.ttsSource.pollyEngine",
            "Polly engine",
        );
        polly_sizer.add(
            &StaticText::builder(&polly_box)
                .with_label("Synthesis engine")
                .build(),
            0,
            SizerFlag::All,
            3,
        );
        polly_sizer.add(&polly_engine, 0, SizerFlag::Expand | SizerFlag::All, 3);
        let polly_language = TextCtrl::builder(&polly_box)
            .with_value(&polly.language_code)
            .build();
        super::set_accessible_name(
            &polly_language,
            "Polly language code; blank uses the voice default",
        );
        super::help::tag(
            &polly_language,
            "dialog.ttsSource.pollyLanguage",
            "Polly language code",
        );
        polly_sizer.add(
            &StaticText::builder(&polly_box)
                .with_label("Language code (optional)")
                .build(),
            0,
            SizerFlag::All,
            3,
        );
        polly_sizer.add(&polly_language, 0, SizerFlag::Expand | SizerFlag::All, 3);
        polly_panel.set_sizer(polly_sizer, true);

        let gtts_panel = Panel::builder(parent).build();
        let (gtts_sizer, gtts_box) =
            super::group_box(&gtts_panel, "Google Translate voice settings");
        let gtts_tld = TextCtrl::builder(&gtts_box).with_value(&gtts.tld).build();
        super::set_accessible_name(
            &gtts_tld,
            "Google Translate accent domain suffix; blank uses com",
        );
        super::help::tag(
            &gtts_tld,
            "dialog.ttsSource.gttsTld",
            "Google Translate accent domain",
        );
        gtts_sizer.add(
            &StaticText::builder(&gtts_box)
                .with_label("Accent domain suffix, for example co.uk")
                .build(),
            0,
            SizerFlag::All,
            3,
        );
        gtts_sizer.add(&gtts_tld, 0, SizerFlag::Expand | SizerFlag::All, 3);
        let gtts_speed = Choice::builder(&gtts_box).build();
        super::set_accessible_name(&gtts_speed, "Google Translate speed mode");
        for label in ["Speed: provider default", "Speed: normal", "Speed: slow"] {
            gtts_speed.append(label);
        }
        gtts_speed.set_selection(match gtts.slow {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        });
        super::help::tag(
            &gtts_speed,
            "dialog.ttsSource.gttsSpeed",
            "Google Translate speed mode",
        );
        gtts_sizer.add(&gtts_speed, 0, SizerFlag::Expand | SizerFlag::All, 3);
        gtts_panel.set_sizer(gtts_sizer, true);

        let controls = Self {
            none_panel,
            eleven_panel,
            eleven_model,
            eleven_language,
            eleven_stability_override,
            eleven_stability,
            eleven_similarity_override,
            eleven_similarity,
            eleven_style_override,
            eleven_style,
            eleven_boost,
            eleven_stream,
            openai_panel,
            openai_model,
            openai_instructions,
            azure_panel,
            azure_style,
            azure_degree,
            azure_role,
            google_panel,
            google_language,
            google_effect,
            polly_panel,
            polly_engine,
            polly_language,
            gtts_panel,
            gtts_tld,
            gtts_speed,
        };
        controls.refresh_compatibility();
        controls
    }

    fn panels(&self) -> [Panel; 7] {
        [
            self.none_panel.clone(),
            self.eleven_panel.clone(),
            self.openai_panel.clone(),
            self.azure_panel.clone(),
            self.google_panel.clone(),
            self.polly_panel.clone(),
            self.gtts_panel.clone(),
        ]
    }

    fn refresh_catalog(&self) {
        let eleven = model_choice_value(&self.eleven_model);
        let openai = model_choice_value(&self.openai_model);
        let polly = self.polly_engine_value();
        fill_model_choice(&self.eleven_model, crate::tts::engines::ELEVENLABS, &eleven);
        fill_model_choice(&self.openai_model, crate::tts::engines::OPENAI, &openai);
        fill_model_choice(&self.polly_engine, crate::tts::engines::AWS, &polly);
        self.refresh_compatibility();
    }
    fn show(&self, engine: &str) {
        use crate::tts::engines;
        self.eleven_panel.show(engine == engines::ELEVENLABS);
        self.openai_panel.show(engine == engines::OPENAI);
        self.azure_panel.show(engine == engines::AZURE);
        self.google_panel.show(engine == engines::GOOGLE);
        self.polly_panel.show(engine == engines::AWS);
        self.gtts_panel.show(engine == engines::GTTS);
        self.none_panel.show(matches!(
            engine,
            engines::SAPI | engines::EDGE | engines::STAR
        ));
        self.refresh_compatibility();
    }

    fn refresh_compatibility(&self) {
        let eleven_v3 = model_choice_value(&self.eleven_model) == "eleven_v3";
        self.eleven_similarity_override.enable(!eleven_v3);
        self.eleven_similarity
            .enable(!eleven_v3 && self.eleven_similarity_override.get_value());
        self.eleven_boost.enable(!eleven_v3);
        self.eleven_stream.enable(!eleven_v3);
        self.openai_instructions
            .enable(model_choice_value(&self.openai_model).starts_with("gpt-4o-mini-tts"));
        self.azure_degree
            .enable(self.azure_style.get_selection().unwrap_or(0) > 0);
    }

    fn refresh_azure_voice(&self, voice: Option<&crate::tts::engine::Voice>) {
        let wanted_style = selected_choice_value(&self.azure_style);
        let wanted_role = selected_choice_value(&self.azure_role);
        let styles = voice.map(|voice| voice.styles.as_slice()).unwrap_or(&[]);
        let roles = voice.map(|voice| voice.roles.as_slice()).unwrap_or(&[]);
        fill_default_choice(
            &self.azure_style,
            styles,
            &wanted_style,
            "Default speaking style",
        );
        fill_default_choice(
            &self.azure_role,
            roles,
            &wanted_role,
            "Default speaking role",
        );
        self.refresh_compatibility();
    }

    /// Writes a saved section back into the widgets — the mirror of
    /// [`Self::settings`], and what makes an engine come back as it was left.
    /// Settings of the wrong provider (or `None`, for an engine with no panel)
    /// leave the widgets alone.
    fn load(&self, engine: &str, settings: Option<&TtsEngineSettings>) {
        use crate::tts::engines;
        match (engine, settings) {
            (engines::ELEVENLABS, Some(TtsEngineSettings::ElevenLabs(eleven))) => {
                fill_model_choice(&self.eleven_model, engines::ELEVENLABS, &eleven.model);
                self.eleven_language.set_value(&eleven.language_code);
                set_optional_double(
                    &self.eleven_stability_override,
                    &self.eleven_stability,
                    eleven.stability,
                    0.5,
                );
                set_optional_double(
                    &self.eleven_similarity_override,
                    &self.eleven_similarity,
                    eleven.similarity_boost,
                    0.75,
                );
                set_optional_double(
                    &self.eleven_style_override,
                    &self.eleven_style,
                    eleven.style,
                    0.0,
                );
                self.eleven_boost.set_selection(match eleven.speaker_boost {
                    None => 0,
                    Some(true) => 1,
                    Some(false) => 2,
                });
                self.eleven_stream.set_value(eleven.stream);
            }
            (engines::OPENAI, Some(TtsEngineSettings::OpenAi(openai))) => {
                fill_model_choice(&self.openai_model, engines::OPENAI, &openai.model);
                self.openai_instructions.set_value(&openai.instructions);
            }
            (engines::AZURE, Some(TtsEngineSettings::Azure(azure))) => {
                // The style and role lists belong to the selected voice, so keep
                // them and only move the selection; `refresh_azure_voice` rebuilds
                // them once the voice is known.
                fill_default_choice(
                    &self.azure_style,
                    &choice_values(&self.azure_style),
                    &azure.style,
                    "Default speaking style",
                );
                self.azure_degree
                    .set_value(azure.style_degree.clamp(0.01, 2.0));
                fill_default_choice(
                    &self.azure_role,
                    &choice_values(&self.azure_role),
                    &azure.role,
                    "Default speaking role",
                );
            }
            (engines::GOOGLE, Some(TtsEngineSettings::Google(google))) => {
                self.google_language.set_value(&google.language_code);
                let selection = crate::tts::engines::google::EFFECTS_PROFILES
                    .iter()
                    .position(|(id, _)| *id == google.effects_profile)
                    .map(|index| index + 1)
                    .unwrap_or(0);
                self.google_effect.set_selection(selection as u32);
            }
            (engines::AWS, Some(TtsEngineSettings::Polly(polly))) => {
                fill_model_choice(&self.polly_engine, engines::AWS, &polly.engine);
                self.polly_language.set_value(&polly.language_code);
            }
            (engines::GTTS, Some(TtsEngineSettings::Gtts(gtts))) => {
                self.gtts_tld.set_value(&gtts.tld);
                self.gtts_speed.set_selection(match gtts.slow {
                    None => 0,
                    Some(false) => 1,
                    Some(true) => 2,
                });
            }
            _ => {}
        }
        self.refresh_compatibility();
    }

    fn model_value(&self, engine: &str) -> String {
        match engine {
            crate::tts::engines::ELEVENLABS => model_choice_value(&self.eleven_model),
            crate::tts::engines::OPENAI => model_choice_value(&self.openai_model),
            crate::tts::engines::AWS => self.polly_engine_value(),
            _ => String::new(),
        }
    }
    fn polly_engine_value(&self) -> String {
        model_choice_value(&self.polly_engine)
    }

    fn settings(&self, engine: &str) -> Option<TtsEngineSettings> {
        use crate::tts::engines;
        match engine {
            engines::ELEVENLABS => Some(TtsEngineSettings::ElevenLabs(ElevenLabsTtsSettings {
                model: model_choice_value(&self.eleven_model),
                language_code: self.eleven_language.get_value().trim().to_string(),
                stability: self
                    .eleven_stability_override
                    .get_value()
                    .then(|| self.eleven_stability.get_value()),
                similarity_boost: self
                    .eleven_similarity_override
                    .get_value()
                    .then(|| self.eleven_similarity.get_value()),
                style: self
                    .eleven_style_override
                    .get_value()
                    .then(|| self.eleven_style.get_value()),
                speaker_boost: match self.eleven_boost.get_selection().unwrap_or(0) {
                    1 => Some(true),
                    2 => Some(false),
                    _ => None,
                },
                stream: self.eleven_stream.get_value(),
            })),
            engines::OPENAI => Some(TtsEngineSettings::OpenAi(OpenAiTtsSettings {
                model: model_choice_value(&self.openai_model),
                instructions: self.openai_instructions.get_value().trim().to_string(),
            })),
            engines::AZURE => Some(TtsEngineSettings::Azure(AzureTtsSettings {
                style: selected_choice_value(&self.azure_style),
                style_degree: self.azure_degree.get_value(),
                role: selected_choice_value(&self.azure_role),
            })),
            engines::GOOGLE => Some(TtsEngineSettings::Google(GoogleTtsSettings {
                language_code: self.google_language.get_value().trim().to_string(),
                effects_profile: self
                    .google_effect
                    .get_selection()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| {
                        crate::tts::engines::google::EFFECTS_PROFILES.get(index as usize)
                    })
                    .map(|(id, _)| (*id).to_string())
                    .unwrap_or_default(),
            })),
            engines::AWS => Some(TtsEngineSettings::Polly(PollyTtsSettings {
                engine: self.polly_engine_value(),
                language_code: self.polly_language.get_value().trim().to_string(),
            })),
            engines::GTTS => Some(TtsEngineSettings::Gtts(GttsTtsSettings {
                tld: self.gtts_tld.get_value().trim().to_string(),
                slow: match self.gtts_speed.get_selection().unwrap_or(0) {
                    1 => Some(false),
                    2 => Some(true),
                    _ => None,
                },
            })),
            _ => None,
        }
    }
}

fn bind_optional_double(check: &CheckBox, input: &SpinCtrlDouble) {
    let check = check.clone();
    let input = input.clone();
    check
        .clone()
        .on_toggled(move |_| input.enable(check.get_value()));
}

/// Sets an override checkbox and its input together, the way
/// [`bind_optional_double`] keeps them while the user works.
fn set_optional_double(
    check: &CheckBox,
    input: &SpinCtrlDouble,
    value: Option<f64>,
    fallback: f64,
) {
    check.set_value(value.is_some());
    input.set_value(value.unwrap_or(fallback));
    input.enable(value.is_some());
}

/// The choice's real values — everything but the index-0 "default" row that
/// [`fill_default_choice`] puts there.
fn choice_values(choice: &Choice) -> Vec<String> {
    (1..choice.get_count())
        .filter_map(|index| choice.get_string(index))
        .collect()
}

fn fill_default_choice(choice: &Choice, values: &[String], wanted: &str, default_label: &str) {
    choice.clear();
    choice.append(default_label);
    let mut selection = 0;
    for value in values {
        choice.append(value);
        if value == wanted {
            selection = choice.get_count().saturating_sub(1);
        }
    }
    if !wanted.is_empty() && !values.iter().any(|value| value == wanted) {
        choice.append(wanted);
        selection = choice.get_count().saturating_sub(1);
    }
    choice.set_selection(selection);
}

fn selected_choice_value(choice: &Choice) -> String {
    choice
        .get_selection()
        .filter(|selection| *selection > 0)
        .and_then(|selection| choice.get_string(selection))
        .unwrap_or_default()
}

/// The models to offer before — or without — a successful catalog refresh.
///
/// Polly's four synthesis engines and ElevenLabs' and OpenAI's speech models are
/// fixed, documented lists that the app already hard-codes elsewhere, so a
/// source can be configured on a machine whose catalog has never been fetched
/// (no credentials yet, discovery blocked, first run). Without this the picker
/// offers nothing but "Provider default" and the engine is unselectable.
fn builtin_models(engine: &str) -> Vec<crate::tts::catalog::CatalogModel> {
    use crate::tts::catalog::CatalogModel;
    use crate::tts::engines;
    let ids: &[&str] = match engine {
        engines::AWS => &["standard", "neural", "long-form", "generative"],
        engines::ELEVENLABS => engines::elevenlabs::MODELS,
        engines::OPENAI => &["gpt-4o-mini-tts", "tts-1", "tts-1-hd"],
        _ => &[],
    };
    ids.iter().copied().map(CatalogModel::plain).collect()
}

fn fill_model_choice(choice: &Choice, engine: &str, wanted: &str) {
    choice.clear();
    choice.append("Provider default");
    let mut models = crate::tts::catalog::models(engine);
    if models.is_empty() {
        models = builtin_models(engine);
    }
    for model in &models {
        choice.append(&model.id);
    }
    let wanted = wanted.trim();
    let selection = if wanted.is_empty() {
        0
    } else if let Some(index) = models.iter().position(|model| model.id == wanted) {
        index + 1
    } else {
        choice.append(&format!("{wanted} (unavailable)"));
        models.len() + 1
    };
    choice.set_selection(selection as u32);
    // Polly's "model" is its synthesis engine, and that is what the label above
    // the picker calls it — announcing it as a model would be a third name for
    // the same control.
    let noun = if engine == crate::tts::engines::AWS {
        "synthesis engine"
    } else {
        "model"
    };
    super::set_accessible_name(
        choice,
        &format!("{} {noun}", crate::tts::engines::display_name(engine)),
    );
}

fn model_choice_value(choice: &Choice) -> String {
    let value = choice
        .get_selection()
        .and_then(|index| choice.get_string(index))
        .unwrap_or_default();
    if value == "Provider default" {
        String::new()
    } else {
        value
            .strip_suffix(" (unavailable)")
            .unwrap_or(&value)
            .to_string()
    }
}
/// The count line for `engine`, given what is cached for it.
///
/// Split out from the widget so the three cases can be tested: "never fetched"
/// and "fetched and empty" are different answers and must not collapse.
fn voice_status_text(engine: &str, count: Option<usize>) -> String {
    let name = crate::tts::engines::display_name(engine);
    match count {
        Some(0) => format!("{name} reported no voices."),
        Some(1) => format!("1 voice available for {name}."),
        Some(n) => format!("{n} voices available for {name}."),
        None => format!("Voice catalog for {name} is refreshing automatically."),
    }
}

/// Refreshes the count label, and the picker's accessible name from the same
/// text so the spoken and the visible answer can never disagree.
///
/// Counted under `model`, not engine-wide, so the number matches what
/// [`fill_voice_choice`] just put in the picker — an engine whose API states a
/// real per-voice model constraint (Polly) would otherwise have the label
/// claiming every voice on the account beside a dropdown holding three.
fn update_voice_status(label: &StaticText, choice: &Choice, engine: &str, model: &str) {
    let text = voice_status_text(engine, crate::tts::voice_count_for_model(engine, model));
    label.set_label(&text);
    super::set_accessible_name(choice, &format!("Voice. {text}"));
}

/// Repopulates the voice picker for `engine`, selecting `wanted` if it is
/// still in the list.
///
/// Engines whose voices have not been fetched yet show only "Default voice" —
/// the fetch button fills the rest in. That keeps opening the dialog instant
/// even when the configured engine is a slow cloud service.
fn fill_voice_choice(
    choice: &Choice,
    voices: &Rc<std::cell::RefCell<Vec<crate::tts::engine::Voice>>>,
    engine: &str,
    model: &str,
    wanted: &str,
) {
    let fetched = crate::tts::cached_voices_for_model(engine, model).unwrap_or_default();
    choice.clear();
    choice.append("Default voice");
    for voice in &fetched {
        choice.append(&voice.label);
    }
    let selection = fetched
        .iter()
        .position(|v| v.id == wanted)
        .map(|i| i + 1)
        .unwrap_or(0);
    choice.set_selection(selection as u32);
    *voices.borrow_mut() = fetched;
}

/// The voice id the picker is showing; empty means the engine default.
fn selected_voice(
    choice: &Choice,
    voices: &Rc<std::cell::RefCell<Vec<crate::tts::engine::Voice>>>,
) -> String {
    let selection = choice.get_selection().map(|i| i as usize).unwrap_or(0);
    if selection == 0 {
        return String::new();
    }
    voices
        .borrow()
        .get(selection - 1)
        .map(|v| v.id.clone())
        .unwrap_or_default()
}

/// Names the pitch slider, saying so when the engine ignores it.
fn set_pitch_name(slider: &Slider, announcer: &SliderAnnouncer, engine: &str) {
    let name = if pitch_is_supported(engine) {
        "Voice pitch"
    } else {
        "Voice pitch, not supported by this engine"
    };
    super::set_accessible_name(slider, name);
    // The MSAA name above is only half of it — NVDA reads the UIA one.
    announcer.set_name(name);
}

/// Gives a dialog slider the two things that make it usable with a screen
/// reader: our UIA provider, so the announced value is the slider's own number
/// rather than the trackbar's percentage of range, and the app's movement
/// mapping instead of the trackbar's backwards one (see
/// [`super::slider_uia::key_step`]).
///
/// `suffix` is appended to the announced value. The returned announcer must be
/// kept alive for the slider's lifetime and uninstalled before the dialog is
/// destroyed.
fn wire_slider(
    slider: &Slider,
    name: &str,
    suffix: &'static str,
    min: i32,
    max: i32,
    page: i32,
) -> Rc<SliderAnnouncer> {
    let announcer = Rc::new(super::slider_uia::install(slider));
    announcer.set_text(name, &format!("{}{suffix}", slider.value()));
    let slider_for_keys = slider.clone();
    let announcer_for_keys = announcer.clone();
    slider.clone().on_key_down(move |event| {
        let Some((code, _)) = super::key_of(&event) else {
            event.skip(true);
            return;
        };
        let current = slider_for_keys.value();
        let Some(value) = super::slider_uia::key_step(code, current, min, max, page) else {
            event.skip(true);
            return;
        };
        // wxDragon re-arms `Skip(true)` before every closure, so a handled key
        // needs an explicit `skip(false)`; without it the trackbar's default proc
        // also gets the key and applies its opposite mapping.
        event.skip(false);
        slider_for_keys.set_value(value);
        // Announced even at the end of the range, so the key always speaks.
        announcer_for_keys.update_value(&format!("{value}{suffix}"));
    });
    announcer
}

/// Synthesizes a fixed phrase and reports the outcome.
///
/// This is the only way a user can tell a wrong API key from a silent source:
/// chat-driven failures are deliberately reported quietly, but a button the
/// user just pressed should answer them directly.
fn preview_voice(
    app: &Rc<App>,
    parent: &Panel,
    engine: &'static str,
    synth: crate::tts::engine::SynthRequest,
    alive: &Rc<std::cell::Cell<bool>>,
) {
    let speech = app.config.borrow().speech.clone();
    let (sender, receiver) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("tts-preview".into())
        .spawn(move || {
            // SAPI is not in `engines::build` — it is not a network engine and
            // lives on its own apartment thread — but it renders to the same
            // samples, so from here on the two are one path.
            let result = if crate::tts::engines::is_network(engine) {
                match crate::tts::engines::build(engine, &speech) {
                    Some(built) => built.synth(&synth.truncated(speech.max_chars())),
                    None => Ok(Vec::new()),
                }
            } else {
                crate::tts::sapi::synth_preview(&synth.truncated(speech.max_chars()))
            };
            let _ = sender.send(result);
        })
        .ok();

    let parent = parent.clone();
    let alive = alive.clone();
    super::run_when_ready(move || {
        if !alive.get() {
            return true;
        }
        let Ok(result) = receiver.try_recv() else {
            return false;
        };
        match result {
            Ok(samples) if samples.is_empty() => {
                super::show_error(&parent, "Preview voice", "The engine returned no audio.")
            }
            Ok(samples) => {
                // Played through the app's own cue output rather than a mixer
                // source, so a preview works before the source exists and can
                // never reach the stream.
                crate::audio::cue::play_samples_async(std::sync::Arc::new(samples));
            }
            Err(error) => super::show_error(&parent, "Preview voice", &error.to_string()),
        }
        true
    });
}
fn edit_sound_events(
    app: &Rc<App>,
    scene_index: usize,
    source_index: usize,
    current: SoundEventsSourceConfig,
) {
    let Some(frame) = app.widgets(|w| w.frame.clone()) else {
        return;
    };
    let dialog = Dialog::builder(&frame, "Sound Events source")
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(420, 320)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // There is no pack picker here: every Sound Events source plays the pack
    // baked into the executable. Choosing a custom pack will live on the
    // Preferences "Sound packs" tab, which is why `current.pack_path` is
    // carried through untouched rather than dropped.

    // The visible labels come from the sound pack's own event labels, so this
    // dialog and the Sound Pack Manager can never name an event differently.
    // Screen readers do not announce a checkbox's label here on their own.
    let event_check = |event: StreamEvent, value: bool| {
        let check = CheckBox::builder(&panel).with_label(event.label()).build();
        super::set_accessible_name(&check, event.label());
        check.set_value(value);
        check
    };
    let listener_increase = event_check(StreamEvent::ListenerIncrease, current.listener_increase);
    super::help::tag(
        &listener_increase,
        "dialog.soundEventsSource.listenerIncrease",
        "Listener count increased checkbox",
    );
    let listener_decrease = event_check(StreamEvent::ListenerDecrease, current.listener_decrease);
    super::help::tag(
        &listener_decrease,
        "dialog.soundEventsSource.listenerDecrease",
        "Listener count decreased checkbox",
    );
    let listener_peak_increase = event_check(
        StreamEvent::ListenerPeakIncrease,
        current.listener_peak_increase,
    );
    super::help::tag(
        &listener_peak_increase,
        "dialog.soundEventsSource.listenerPeakIncrease",
        "Listener peak increased checkbox",
    );
    let incoming_chat = event_check(StreamEvent::IncomingChat, current.incoming_chat);
    super::help::tag(
        &incoming_chat,
        "dialog.soundEventsSource.incomingChat",
        "Incoming chat message checkbox",
    );
    let outgoing_chat = event_check(StreamEvent::OutgoingChat, current.outgoing_chat);
    super::help::tag(
        &outgoing_chat,
        "dialog.soundEventsSource.outgoingChat",
        "Outgoing chat message checkbox",
    );

    let output_check = CheckBox::builder(&panel)
        .with_label("Send these sounds to the stream")
        .build();
    super::set_accessible_name(&output_check, "Send these sounds to the stream");
    super::help::tag(
        &output_check,
        "dialog.soundEventsSource.toStream",
        "Send these sounds to the stream checkbox",
    );
    output_check.set_value(current.output_to_stream);

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let ok = super::ok_button(&panel, "OK");
    // `ID_CANCEL` is what wx maps Escape to; without it Escape does nothing.
    let cancel = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("Cancel")
        .build();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);

    sizer.add(&listener_increase, 0, SizerFlag::All, 4);
    sizer.add(&listener_decrease, 0, SizerFlag::All, 4);
    sizer.add(&listener_peak_increase, 0, SizerFlag::All, 4);
    sizer.add(&incoming_chat, 0, SizerFlag::All, 4);
    sizer.add(&outgoing_chat, 0, SizerFlag::All, 4);
    sizer.add(&output_check, 0, SizerFlag::All, 8);
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    {
        let dialog = dialog.clone();
        ok.on_click(move |_| dialog.end_modal(ID_OK));
    }
    {
        let dialog = dialog.clone();
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    if dialog.show_modal() == ID_OK {
        set_source_kind(
            app,
            scene_index,
            source_index,
            SourceKindConfig::SoundEvents(SoundEventsSourceConfig {
                // Unused for now; preserved so a path set by an earlier build
                // survives until the Preferences pack picker can show it.
                pack_path: current.pack_path,
                listener_increase: listener_increase.get_value(),
                listener_decrease: listener_decrease.get_value(),
                listener_peak_increase: listener_peak_increase.get_value(),
                incoming_chat: incoming_chat.get_value(),
                outgoing_chat: outgoing_chat.get_value(),
                output_to_stream: output_check.get_value(),
            }),
        );
    }
    dialog.destroy();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tts::engines;

    /// The whole point of the label: "we have never asked" and "we asked and
    /// there were none" are different answers, and neither is a number.
    #[test]
    fn voice_status_distinguishes_unfetched_from_empty() {
        let unfetched = voice_status_text(engines::AZURE, None);
        assert!(
            unfetched.contains("refreshing automatically"),
            "{unfetched}"
        );
        assert!(unfetched.contains("Azure"), "{unfetched}");

        let empty = voice_status_text(engines::AZURE, Some(0));
        assert!(empty.contains("no voices"), "{empty}");
        assert!(!empty.contains("refreshing automatically"), "{empty}");
    }

    #[test]
    fn voice_status_names_the_engine_and_the_count() {
        assert_eq!(
            voice_status_text(engines::ELEVENLABS, Some(42)),
            "42 voices available for ElevenLabs."
        );
        assert_eq!(
            voice_status_text(engines::STAR, Some(1)),
            "1 voice available for Star."
        );
    }

    /// The pitch slider's name is derived from this, and `TtsSourceConfig`'s
    /// docs record the same split from the other side.
    #[test]
    fn only_the_ssml_engines_support_pitch() {
        for engine in [engines::EDGE, engines::AZURE, engines::GOOGLE, engines::AWS] {
            assert!(pitch_is_supported(engine), "{engine}");
        }
        for engine in [
            engines::SAPI,
            engines::OPENAI,
            engines::GTTS,
            engines::ELEVENLABS,
            engines::STAR,
        ] {
            assert!(!pitch_is_supported(engine), "{engine}");
        }
    }
}
