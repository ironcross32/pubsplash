//! Scenes and Sources tab: two lists with move/add/rename/delete controls
//! and per-type source edit dialogs.

use super::home::on_sources_changed;
use super::{App, WXK_DELETE, WXK_DOWN, WXK_UP, show_error};
use crate::config::{SoundEventsSourceConfig, SourceConfig, SourceKindConfig, TtsSourceConfig};
use crate::soundpack::StreamEvent;
use crate::state::{ListEdit, move_down, move_up};
use std::rc::Rc;
use wxdragon::prelude::*;

pub fn build(app: &Rc<App>, panel: &Panel) -> (ListBox, ListBox) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // --- Scenes ---
    let scenes_label = StaticText::builder(panel).with_label("Scenes").build();
    let scenes_list = ListBox::builder(panel).build();
    super::set_accessible_name(&scenes_list, "Scenes");
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
    super::set_accessible_name(&sources_list, "Sources in selected scene");
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
            let Some(index) = list.get_selection().map(|i| i as usize) else {
                return;
            };
            super::sends::edit_sends(&app, selected_scene_index(&app), index);
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

/// The scene currently highlighted in the scenes-tab list (defaults to the
/// active scene when nothing is selected).
fn selected_scene_index(app: &Rc<App>) -> usize {
    app.widgets(|w| w.scenes_list.get_selection())
        .flatten()
        .map(|i| i as usize)
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
        w.scenes_list.clear();
        for scene in &config.scenes.scenes {
            let label = if scene.is_default {
                format!("{} (default)", scene.name)
            } else {
                scene.name.clone()
            };
            w.scenes_list.append(&label);
        }
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
/// anyone arrowing through it. The same in-place treatment
/// `home::relabel_source_strips` gives the mixer strips applies here: the diff
/// is what does the work, since `set_string` still raises a name change for
/// the item it touches.
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
        if w.sources_list.get_count() as usize == labels.len() {
            // Same sources, possibly renamed: touch only what differs.
            for (index, label) in labels.iter().enumerate() {
                let index = index as u32;
                if w.sources_list.get_string(index).as_deref() != Some(label.as_str()) {
                    w.sources_list.set_string(index, label);
                }
            }
            return;
        }
        // The list itself changed, so the focus context genuinely has too.
        let selected = w.sources_list.get_selection();
        w.sources_list.clear();
        for label in &labels {
            w.sources_list.append(label);
        }
        if let Some(index) = selected {
            if index < w.sources_list.get_count() {
                w.sources_list.set_selection(index, true);
            }
        }
    });
}

fn after_scene_edit(app: &Rc<App>) {
    app.save_config();
    refresh_scenes_list(app);
    // The sources list is refreshed last, after `on_sources_changed` has
    // re-resolved Application sources: an edit may have named a new
    // application, and the labels read the resolved process out of that cache.
    on_sources_changed(app);
    refresh_sources_list(app);
}

fn move_scene(app: &Rc<App>, list: &ListBox, up: bool) {
    let Some(index) = list.get_selection().map(|i| i as usize) else {
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
    let Some(index) = list.get_selection().map(|i| i as usize) else {
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
    let Some(index) = list.get_selection().map(|i| i as usize) else {
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
    let Some(index) = list.get_selection().map(|i| i as usize) else {
        return;
    };
    let scene_index = selected_scene_index(app);
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
        after_scene_edit(app);
        app.widgets(|w| w.sources_list.set_selection(new_index as u32, true));
    }
}

fn remove_source(app: &Rc<App>, list: &ListBox) {
    let Some(index) = list.get_selection().map(|i| i as usize) else {
        return;
    };
    let scene_index = selected_scene_index(app);
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
    after_scene_edit(app);
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
    after_scene_edit(app);
    app.widgets(|w| w.sources_list.set_selection(new_index as u32, true));
    // Open the parameter dialog right away for types that need setup.
    if let Some(w) = app.widgets(|w| w.sources_list.clone()) {
        edit_source(app, &w);
    }
}

fn edit_source(app: &Rc<App>, list: &ListBox) {
    let Some(index) = list.get_selection().map(|i| i as usize) else {
        return;
    };
    let scene_index = selected_scene_index(app);
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
    after_scene_edit(app);
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
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(440, 520)
        .build();
    // Voice fetches and previews finish on the pump, which keeps running
    // inside this dialog's modal loop — and can outlive the dialog if the user
    // closes it mid-request. Cleared just before `destroy()`, so those
    // callbacks bail instead of touching freed widgets.
    let alive = Rc::new(std::cell::Cell::new(true));
    let panel = Panel::builder(&dialog).build();
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
    fill_voice_choice(&voice_choice, &voices, selected_id, &current.voice);
    // How many voices this engine has, as a real label rather than only an
    // accessible name — and refreshed with the engine, so it can never report
    // the previous engine's count.
    let voice_count_label = StaticText::builder(&panel).build();
    update_voice_status(&voice_count_label, &voice_choice, selected_id);

    let fetch_voices = Button::builder(&panel)
        .with_label("&Get available voices")
        .build();
    super::set_accessible_name(&fetch_voices, "Get available voices");
    super::help::tag(
        &fetch_voices,
        "dialog.ttsSource.fetchVoices",
        "Get available voices button",
    );

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
    // Named per engine: not every engine has a pitch control, and a slider
    // that silently does nothing is worse than one that says so.
    set_pitch_name(&pitch_slider, selected_id);

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
    super::help::tag(
        &preview,
        "dialog.ttsSource.preview",
        "Preview voice button",
    );

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let ok = Button::builder(&panel).with_label("OK").build();
    let cancel = Button::builder(&panel).with_label("Cancel").build();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);

    sizer.add(&engine_label, 0, SizerFlag::All, 4);
    sizer.add(&engine_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&voice_label, 0, SizerFlag::All, 4);
    sizer.add(&voice_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&voice_count_label, 0, SizerFlag::All, 4);
    sizer.add(&fetch_voices, 0, SizerFlag::All, 4);
    sizer.add(&volume_label, 0, SizerFlag::All, 4);
    sizer.add(&volume_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&rate_label, 0, SizerFlag::All, 4);
    sizer.add(&rate_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&pitch_label, 0, SizerFlag::All, 4);
    sizer.add(&pitch_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&output_check, 0, SizerFlag::All, 8);
    sizer.add(&preview, 0, SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

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
    // The voice picked per engine, for this dialog only. Without it, arrowing
    // *through* an engine on the way to another one threw away the voice you
    // had already chosen there.
    let voice_memory: Rc<std::cell::RefCell<std::collections::HashMap<&'static str, String>>> =
        Rc::new(std::cell::RefCell::new(
            [(selected_id, current.voice.clone())].into_iter().collect(),
        ));
    let pitch_supported = Rc::new(std::cell::Cell::new(pitch_is_supported(selected_id)));

    let apply_engine: Rc<dyn Fn()> = {
        let voice_choice = voice_choice.clone();
        let voice_count_label = voice_count_label.clone();
        let pitch_slider = pitch_slider.clone();
        let voices = voices.clone();
        let selected_engine = selected_engine.clone();
        let applied = applied.clone();
        let voice_memory = voice_memory.clone();
        let pitch_supported = pitch_supported.clone();
        Rc::new(move || {
            let engine = selected_engine();
            if engine == applied.get() {
                return;
            }
            let wanted = voice_memory
                .borrow()
                .get(engine)
                .cloned()
                .unwrap_or_default();
            voice_choice.freeze();
            fill_voice_choice(&voice_choice, &voices, engine, &wanted);
            voice_choice.thaw();
            update_voice_status(&voice_count_label, &voice_choice, engine);
            let supported = pitch_is_supported(engine);
            if supported != pitch_supported.replace(supported) {
                set_pitch_name(&pitch_slider, engine);
            }
            applied.set(engine);
        })
    };

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
        let voice_choice = voice_choice.clone();
        let voices = voices.clone();
        let applied = applied.clone();
        let voice_memory = voice_memory.clone();
        let settle = settle.clone();
        engine_choice.clone().on_selection_changed(move |_| {
            // Remember what was picked for the engine we are leaving, then do
            // nothing else until the user settles.
            voice_memory
                .borrow_mut()
                .insert(applied.get(), selected_voice(&voice_choice, &voices));
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

    {
        let voice_choice = voice_choice.clone();
        let fetch_voices_btn = fetch_voices.clone();
        let voices = voices.clone();
        let selected_engine = selected_engine.clone();
        let app = app.clone();
        let panel = panel.clone();
        let alive = alive.clone();
        let apply_engine = apply_engine.clone();
        let voice_count_label = voice_count_label.clone();
        fetch_voices.on_click(move |_| {
            // The picker may have changed within the settle window; fetching
            // the previous engine's voices would be a wasted round trip.
            apply_engine();
            let engine = selected_engine();
            crate::tts::forget_voices(engine);
            // Blocking the UI thread on a voice fetch would freeze the dialog
            // for as long as the service takes, so the button reports its own
            // progress and the work happens off-thread.
            fetch_voices_btn.set_label("Fetching voices…");
            fetch_voices_btn.enable(false);
            super::set_accessible_name(&fetch_voices_btn, "Fetching voices");

            let speech = app.config.borrow().speech.clone();
            let (sender, receiver) = crossbeam_channel::bounded(1);
            std::thread::Builder::new()
                .name("tts-voice-fetch".into())
                .spawn(move || {
                    let result = match crate::tts::engines::build(engine, &speech) {
                        Some(built) => built.voices(),
                        None => Ok(crate::tts::cached_voices(engine).unwrap_or_default()),
                    };
                    let _ = sender.send(result);
                })
                .ok();

            let voice_choice = voice_choice.clone();
            let voices = voices.clone();
            let button = fetch_voices_btn.clone();
            let panel = panel.clone();
            let alive = alive.clone();
            let voice_count_label = voice_count_label.clone();
            // wxdragon has no cross-thread post, so the result is collected on
            // the next idle tick rather than by blocking here.
            super::run_when_ready(move || {
                if !alive.get() {
                    return true;
                }
                let Ok(result) = receiver.try_recv() else {
                    return false;
                };
                button.set_label("&Get available voices");
                button.enable(true);
                super::set_accessible_name(&button, "Get available voices");
                match result {
                    Ok(fetched) if fetched.is_empty() => {
                        crate::tts::store_voices(engine, fetched);
                        update_voice_status(&voice_count_label, &voice_choice, engine);
                        super::show_info(
                            &panel,
                            "Voices",
                            "That engine reported no voices.",
                        );
                    }
                    Ok(fetched) => {
                        crate::tts::store_voices(engine, fetched);
                        fill_voice_choice(&voice_choice, &voices, engine, "");
                        update_voice_status(&voice_count_label, &voice_choice, engine);
                    }
                    Err(error) => {
                        super::show_error(&panel, "Voices", &error.to_string());
                    }
                }
                true
            });
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
        preview.on_click(move |_| {
            apply_engine();
            let engine = selected_engine();
            let synth = crate::tts::engine::SynthRequest {
                text: "Pubsplash text to speech is working.".into(),
                voice: selected_voice(&voice_choice, &voices),
                rate: rate_slider.value().clamp(-10, 10),
                volume: volume_slider.value().clamp(0, 100) as u32,
                pitch: pitch_slider.value().clamp(-50, 50),
            };
            preview_voice(&app, &panel, engine, synth, &alive);
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
        set_source_kind(
            app,
            scene_index,
            source_index,
            SourceKindConfig::Tts(TtsSourceConfig {
                engine: selected_engine().to_string(),
                voice: selected_voice(&voice_choice, &voices),
                volume: volume_slider.value().clamp(0, 100) as u32,
                rate: rate_slider.value().clamp(-10, 10),
                pitch: pitch_slider.value().clamp(-50, 50),
                output_to_stream: output_check.get_value(),
            }),
        );
    }
    alive.set(false);
    settle.stop();
    dialog.destroy();
}

/// How long the engine picker waits after the last keypress before rebuilding
/// the voice list. Long enough to arrow through all nine engines without
/// stopping, short enough to feel immediate once you land.
const SETTLE_MS: i32 = 300;

/// Whether an engine honours the pitch slider at all.
fn pitch_is_supported(engine: &str) -> bool {
    use crate::tts::engines;
    matches!(engine, engines::EDGE | engines::AZURE | engines::GOOGLE)
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
        None => format!("Voices for {name} not fetched yet. Use Get available voices."),
    }
}

/// Refreshes the count label, and the picker's accessible name from the same
/// text so the spoken and the visible answer can never disagree.
fn update_voice_status(label: &StaticText, choice: &Choice, engine: &str) {
    let text = voice_status_text(engine, crate::tts::voice_count(engine));
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
    wanted: &str,
) {
    let fetched = crate::tts::cached_voices(engine).unwrap_or_default();
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
fn set_pitch_name(slider: &Slider, engine: &str) {
    super::set_accessible_name(
        slider,
        if pitch_is_supported(engine) {
            "Voice pitch"
        } else {
            "Voice pitch, not supported by this engine"
        },
    );
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
    if !crate::tts::engines::is_network(engine) {
        // SAPI speaks locally on its own thread; there is nothing to report.
        app.speaker.speak(crate::tts::speaker::SpeakRequest {
            engine: engine.to_string(),
            synth,
            source_name: String::new(),
            to_stream: false,
            speech: app.config.borrow().speech.clone(),
        });
        return;
    }

    let speech = app.config.borrow().speech.clone();
    let (sender, receiver) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("tts-preview".into())
        .spawn(move || {
            let result = match crate::tts::engines::build(engine, &speech) {
                Some(built) => built.synth(&synth.truncated(speech.max_chars())),
                None => Ok(Vec::new()),
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
            Ok(samples) if samples.is_empty() => super::show_error(
                &parent,
                "Preview voice",
                "The engine returned no audio.",
            ),
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
    let ok = Button::builder(&panel).with_label("OK").build();
    let cancel = Button::builder(&panel).with_label("Cancel").build();
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
        assert!(unfetched.contains("not fetched"), "{unfetched}");
        assert!(unfetched.contains("Azure"), "{unfetched}");

        let empty = voice_status_text(engines::AZURE, Some(0));
        assert!(empty.contains("no voices"), "{empty}");
        assert!(!empty.contains("not fetched"), "{empty}");
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
        for engine in [engines::EDGE, engines::AZURE, engines::GOOGLE] {
            assert!(pitch_is_supported(engine), "{engine}");
        }
        for engine in [
            engines::SAPI,
            engines::OPENAI,
            engines::GTTS,
            engines::AWS,
            engines::ELEVENLABS,
            engines::STAR,
        ] {
            assert!(!pitch_is_supported(engine), "{engine}");
        }
    }
}
