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

pub fn refresh_sources_list(app: &Rc<App>) {
    let scene_index = selected_scene_index(app);
    app.widgets(|w| {
        let config = app.config.borrow();
        let selected = w.sources_list.get_selection();
        w.sources_list.clear();
        if let Some(scene) = config.scenes.scenes.get(scene_index) {
            let ctx = app.name_context(&scene.sources);
            for label in crate::source_name::list_labels(&scene.sources, &ctx) {
                w.sources_list.append(&label);
            }
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
        .with_size(400, 360)
        .build();
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
    for name in &engines {
        engine_choice.append(name);
    }
    let engine_index = engines
        .iter()
        .position(|e| *e == current.engine)
        .unwrap_or(0);
    engine_choice.set_selection(engine_index as u32);

    let voice_label = StaticText::builder(&panel).with_label("Voice").build();
    let voice_choice = Choice::builder(&panel).build();
    super::set_accessible_name(&voice_choice, "Voice");
    super::help::tag(&voice_choice, "dialog.ttsSource.voice", "TTS voice choice");
    let voices = crate::tts::voices_for(&current.engine);
    voice_choice.append("Default voice");
    for voice in &voices {
        voice_choice.append(voice);
    }
    let voice_index = voices
        .iter()
        .position(|v| *v == current.voice)
        .map(|i| i + 1)
        .unwrap_or(0);
    voice_choice.set_selection(voice_index as u32);

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

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let ok = Button::builder(&panel).with_label("OK").build();
    let cancel = Button::builder(&panel).with_label("Cancel").build();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);

    sizer.add(&engine_label, 0, SizerFlag::All, 4);
    sizer.add(&engine_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&voice_label, 0, SizerFlag::All, 4);
    sizer.add(&voice_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&volume_label, 0, SizerFlag::All, 4);
    sizer.add(&volume_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&rate_label, 0, SizerFlag::All, 4);
    sizer.add(&rate_slider, 0, SizerFlag::Expand | SizerFlag::All, 4);
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
        let engine = engines
            .get(
                engine_choice
                    .get_selection()
                    .map(|i| i as usize)
                    .unwrap_or(0),
            )
            .map(|s| s.to_string())
            .unwrap_or_else(|| "sapi".into());
        let voice_selection = voice_choice
            .get_selection()
            .map(|i| i as usize)
            .unwrap_or(0);
        let voice = if voice_selection == 0 {
            String::new()
        } else {
            voices.get(voice_selection - 1).cloned().unwrap_or_default()
        };
        set_source_kind(
            app,
            scene_index,
            source_index,
            SourceKindConfig::Tts(TtsSourceConfig {
                engine,
                voice,
                volume: volume_slider.value().clamp(0, 100) as u32,
                rate: rate_slider.value().clamp(-10, 10),
                output_to_stream: output_check.get_value(),
            }),
        );
    }
    dialog.destroy();
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
