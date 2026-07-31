// A GUI app: without this a release build opens a console window next to the
// frame, which is clutter on screen and a stray stop for screen-reader focus.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Standalone Sound Pack Manager.
#[path = "../soundpack.rs"]
mod soundpack;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
};
use wxdragon::prelude::*;

#[derive(Default)]
struct ManagerState {
    project: Option<PathBuf>,
    source_paths: HashMap<soundpack::SoundKind, String>,
    updating_source_path: bool,
}

#[derive(Clone)]
struct TabControls {
    sounds: ListBox,
    source_path: TextCtrl,
    browse: Button,
    test: Button,
}

fn main() {
    let _ = wxdragon::main(|_| {
        let frame = Frame::builder()
            .with_title("Pubsplash Sound Pack Manager")
            .with_size(Size::new(820, 500))
            .build();
        let state = Rc::new(RefCell::new(ManagerState::default()));

        let root = Panel::builder(&frame).build();
        let outer = BoxSizer::builder(Orientation::Vertical).build();

        let toolbar = BoxSizer::builder(Orientation::Horizontal).build();
        let new_project = Button::builder(&root).with_label("&New...").build();
        let open_project = Button::builder(&root).with_label("&Open...").build();
        let save = Button::builder(&root).with_label("&Save").build();
        let compile = Button::builder(&root).with_label("&Compile...").build();
        toolbar.add(&new_project, 0, SizerFlag::All, 4);
        toolbar.add(&open_project, 0, SizerFlag::All, 4);
        toolbar.add(&save, 0, SizerFlag::All, 4);
        toolbar.add(&compile, 0, SizerFlag::All, 4);
        outer.add_sizer(&toolbar, 0, SizerFlag::All, 2);

        let project_label = StaticText::builder(&root)
            .with_label("No sound pack project is open")
            .build();
        outer.add(&project_label, 0, SizerFlag::Expand | SizerFlag::All, 6);

        let notebook = Notebook::builder(&root).build();
        let interface_tab = build_tab(
            &notebook,
            "Interface sounds",
            &soundpack::SoundKind::INTERFACE,
            &state,
        );
        let stream_tab = build_tab(
            &notebook,
            "Stream events",
            &soundpack::SoundKind::STREAM_EVENTS,
            &state,
        );
        outer.add(&notebook, 1, SizerFlag::Expand | SizerFlag::All, 4);
        root.set_sizer_and_fit(outer, true);

        refresh_all(
            &state,
            &project_label,
            &interface_tab,
            &stream_tab,
            &save,
            &compile,
        );
        wire_project_buttons(
            &frame,
            &state,
            &project_label,
            &interface_tab,
            &stream_tab,
            &new_project,
            &open_project,
            &save,
            &compile,
        );
        wire_tab(
            &frame,
            &state,
            &interface_tab,
            soundpack::SoundKind::INTERFACE,
        );
        wire_tab(
            &frame,
            &state,
            &stream_tab,
            soundpack::SoundKind::STREAM_EVENTS,
        );

        frame.show(true);
    });
}

fn build_tab(
    notebook: &Notebook,
    title: &str,
    kinds: &[soundpack::SoundKind],
    state: &Rc<RefCell<ManagerState>>,
) -> TabControls {
    let panel = Panel::builder(notebook).build();
    let outer = BoxSizer::builder(Orientation::Horizontal).build();

    let sounds = ListBox::builder(&panel).build();
    for kind in kinds {
        sounds.append(kind.label());
    }
    if !kinds.is_empty() {
        sounds.set_selection(0, true);
    }

    let right = BoxSizer::builder(Orientation::Vertical).build();
    let source_label = StaticText::builder(&panel).with_label("Source WAV").build();
    let source_row = BoxSizer::builder(Orientation::Horizontal).build();
    let source_path = TextCtrl::builder(&panel).build();
    let browse = Button::builder(&panel).with_label("&Browse...").build();
    source_row.add(&source_path, 1, SizerFlag::Expand | SizerFlag::All, 4);
    source_row.add(&browse, 0, SizerFlag::All, 4);

    let action_row = BoxSizer::builder(Orientation::Horizontal).build();
    let test = Button::builder(&panel).with_label("&Test").build();
    action_row.add(&test, 0, SizerFlag::All, 4);

    right.add(&source_label, 0, SizerFlag::All, 4);
    right.add_sizer(&source_row, 0, SizerFlag::Expand, 0);
    right.add_sizer(&action_row, 0, SizerFlag::All, 0);

    let controls = TabControls {
        sounds,
        source_path,
        browse,
        test,
    };

    {
        let source_path = controls.source_path.clone();
        controls.browse.clone().on_click(move |_| {
            let dialog = FileDialog::builder(&panel)
                .with_message("Select a WAV file")
                .with_wildcard("WAV files (*.wav)|*.wav")
                .with_style(FileDialogStyle::Open)
                .build();
            if dialog.show_modal() == ID_OK {
                if let Some(path) = dialog.get_path() {
                    source_path.set_value(&path);
                }
            }
        });
    }

    {
        let controls = controls.clone();
        let state = Rc::clone(state);
        let kinds = kinds.to_vec();
        controls.source_path.clone().on_text_changed(move |_| {
            remember_source_path(&state, &controls, &kinds);
        });
    }

    outer.add(&controls.sounds, 1, SizerFlag::Expand | SizerFlag::All, 4);
    outer.add_sizer(&right, 2, SizerFlag::Expand, 0);
    panel.set_sizer_and_fit(outer, true);
    notebook.add_page(&panel, title, false, None);
    controls
}

#[allow(clippy::too_many_arguments)]
fn wire_project_buttons(
    frame: &Frame,
    state: &Rc<RefCell<ManagerState>>,
    project_label: &StaticText,
    interface_tab: &TabControls,
    stream_tab: &TabControls,
    new_project: &Button,
    open_project: &Button,
    save: &Button,
    compile: &Button,
) {
    {
        let frame = frame.clone();
        let state = Rc::clone(state);
        let project_label = project_label.clone();
        let interface_tab = interface_tab.clone();
        let stream_tab = stream_tab.clone();
        let save = save.clone();
        let compile = compile.clone();
        new_project.on_click(move |_| {
            let Some((name, parent)) = show_new_project_dialog(&frame) else {
                return;
            };
            match soundpack::create_named_project(&parent, &name) {
                Ok(path) => {
                    let mut state_mut = state.borrow_mut();
                    state_mut.project = Some(path);
                    state_mut.source_paths.clear();
                    drop(state_mut);
                    refresh_all(
                        &state,
                        &project_label,
                        &interface_tab,
                        &stream_tab,
                        &save,
                        &compile,
                    );
                }
                Err(err) => show_error(&frame, &err),
            }
        });
    }
    {
        let frame = frame.clone();
        let state = Rc::clone(state);
        let project_label = project_label.clone();
        let interface_tab = interface_tab.clone();
        let stream_tab = stream_tab.clone();
        let save = save.clone();
        let compile = compile.clone();
        open_project.on_click(move |_| {
            let dialog = DirDialog::builder(&frame, "Open a sound pack project folder", "").build();
            if dialog.show_modal() != ID_OK {
                return;
            }
            let Some(path) = dialog.get_path().map(PathBuf::from) else {
                return;
            };
            match soundpack::read_project_manifest(&path) {
                Ok(_) => {
                    let mut state_mut = state.borrow_mut();
                    state_mut.project = Some(path);
                    state_mut.source_paths.clear();
                    drop(state_mut);
                    refresh_all(
                        &state,
                        &project_label,
                        &interface_tab,
                        &stream_tab,
                        &save,
                        &compile,
                    );
                }
                Err(err) => show_error(&frame, &err),
            }
        });
    }
    {
        let frame = frame.clone();
        let state = Rc::clone(state);
        let project_label = project_label.clone();
        let interface_tab = interface_tab.clone();
        let stream_tab = stream_tab.clone();
        let save_button = save.clone();
        let compile = compile.clone();
        save.on_click(move |_| {
            remember_source_path(&state, &interface_tab, &soundpack::SoundKind::INTERFACE);
            remember_source_path(&state, &stream_tab, &soundpack::SoundKind::STREAM_EVENTS);
            let Some(project) = state.borrow().project.clone() else {
                show_error(&frame, "Open or create a sound pack project first.");
                return;
            };
            let assignments = collect_assignments(&state, &project);
            match soundpack::save_single_variants(&project, &assignments) {
                Ok(count) => {
                    state.borrow_mut().source_paths.clear();
                    refresh_all(
                        &state,
                        &project_label,
                        &interface_tab,
                        &stream_tab,
                        &save_button,
                        &compile,
                    );
                    show_info(&frame, &format!("Saved {count} sound(s) to the project."));
                }
                Err(err) => show_error(&frame, &err),
            }
        });
    }
    {
        let frame = frame.clone();
        let state = Rc::clone(state);
        let project_label = project_label.clone();
        let interface_tab = interface_tab.clone();
        let stream_tab = stream_tab.clone();
        let save = save.clone();
        let compile_for_refresh = compile.clone();
        compile.on_click(move |_| {
            let Some(project) = state.borrow().project.clone() else {
                show_error(&frame, "Open or create a sound pack project first.");
                return;
            };
            let dialog = FileDialog::builder(&frame)
                .with_message("Compile sound pack")
                .with_wildcard("Pubsplash sound packs (*.pspack)|*.pspack")
                .with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
                .build();
            if dialog.show_modal() != ID_OK {
                return;
            }
            let Some(mut output) = dialog.get_path().map(PathBuf::from) else {
                return;
            };
            if output.extension().and_then(|e| e.to_str()) != Some("pspack") {
                output.set_extension("pspack");
            }
            match soundpack::compile_and_bump(&project, &output) {
                Ok(revision) => {
                    refresh_all(
                        &state,
                        &project_label,
                        &interface_tab,
                        &stream_tab,
                        &save,
                        &compile_for_refresh,
                    );
                    show_info(
                        &frame,
                        &format!("Compiled revision {revision} to {}", output.display()),
                    );
                }
                Err(err) => show_error(&frame, &err),
            }
        });
    }
}

fn wire_tab<const N: usize>(
    frame: &Frame,
    state: &Rc<RefCell<ManagerState>>,
    controls: &TabControls,
    kinds: [soundpack::SoundKind; N],
) {
    wire_tab_slice(frame, state, controls, &kinds);
}

fn wire_tab_slice(
    frame: &Frame,
    state: &Rc<RefCell<ManagerState>>,
    controls: &TabControls,
    kinds: &[soundpack::SoundKind],
) {
    {
        let controls = controls.clone();
        let state = Rc::clone(state);
        let kinds = kinds.to_vec();
        controls.sounds.clone().on_selection_changed(move |_| {
            refresh_tab(&state, &controls, &kinds);
        });
    }
    {
        let frame = frame.clone();
        let controls = controls.clone();
        controls.test.clone().on_click(move |_| {
            let typed = controls.source_path.get_value();
            let trimmed = typed.trim();
            if trimmed.is_empty() {
                show_error(&frame, "Choose or type a WAV path to test.");
                return;
            }
            if let Err(err) = test_play(Path::new(trimmed)) {
                show_error(&frame, &err);
            }
        });
    }
}

fn show_new_project_dialog(frame: &Frame) -> Option<(String, PathBuf)> {
    let dialog = Dialog::builder(frame, "New sound pack project")
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(560, 220)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let name_label = StaticText::builder(&panel).with_label("Pack name").build();
    let name = TextCtrl::builder(&panel).build();

    let folder_label = StaticText::builder(&panel)
        .with_label("Parent folder")
        .build();
    let folder_row = BoxSizer::builder(Orientation::Horizontal).build();
    let folder = TextCtrl::builder(&panel).build();
    let browse = Button::builder(&panel).with_label("&Browse...").build();
    folder_row.add(&folder, 1, SizerFlag::Expand | SizerFlag::All, 4);
    folder_row.add(&browse, 0, SizerFlag::All, 4);

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    // The id-plus-`set_default()` pair is what puts Enter on this button; see
    // `ui::ok_button` in the main binary, which this hand-rolls because a
    // standalone bin cannot reach that module. `ID_CONFIRM` rather than `ID_OK`
    // for the reason given there: `wxDialogBase` binds `wxID_OK` to a handler
    // that ends the dialog, and the click propagates to it, which would close
    // this dialog behind the three validation `return`s below.
    const ID_CONFIRM: i32 = 2301;
    let ok = Button::builder(&panel)
        .with_id(ID_CONFIRM)
        .with_label("OK")
        .build();
    let cancel = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("Cancel")
        .build();
    ok.set_default();
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);

    sizer.add(&name_label, 0, SizerFlag::All, 4);
    sizer.add(&name, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&folder_label, 0, SizerFlag::All, 4);
    sizer.add_sizer(&folder_row, 0, SizerFlag::Expand, 0);
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    {
        let panel = panel.clone();
        let folder = folder.clone();
        browse.on_click(move |_| {
            let picker = DirDialog::builder(&panel, "Choose a parent folder", "")
                .with_style(DirDialogStyle::MustExist.bits())
                .build();
            if picker.show_modal() == ID_OK {
                if let Some(path) = picker.get_path() {
                    folder.set_value(&path);
                }
            }
        });
    }
    {
        let dialog = dialog.clone();
        let panel = panel.clone();
        let name = name.clone();
        let folder = folder.clone();
        ok.on_click(move |_| {
            if let Err(err) = soundpack::sanitize_pack_name(&name.get_value()) {
                show_error(&panel, &err);
                return;
            }
            let parent = folder.get_value();
            if parent.trim().is_empty() {
                show_error(&panel, "Choose a parent folder.");
                return;
            }
            if !Path::new(parent.trim()).is_dir() {
                show_error(&panel, "The parent folder does not exist.");
                return;
            }
            dialog.end_modal(ID_OK);
        });
    }
    {
        let dialog = dialog.clone();
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    let result = if dialog.show_modal() == ID_OK {
        Some((name.get_value(), PathBuf::from(folder.get_value().trim())))
    } else {
        None
    };
    dialog.destroy();
    result
}

fn refresh_all(
    state: &Rc<RefCell<ManagerState>>,
    project_label: &StaticText,
    interface_tab: &TabControls,
    stream_tab: &TabControls,
    save: &Button,
    compile: &Button,
) {
    let project = state.borrow().project.clone();
    save.enable(project.is_some());
    compile.enable(project.is_some());
    if let Some(project) = &project {
        let manifest = soundpack::read_project_manifest(project);
        let (name, revision) = manifest
            .map(|m| (m.name, m.revision.to_string()))
            .unwrap_or_else(|_| ("?".into(), "?".into()));
        project_label.set_label(&format!(
            "Project: {name} at {} (revision {revision})",
            project.display()
        ));
    } else {
        project_label.set_label("No sound pack project is open");
    }
    refresh_tab(state, interface_tab, &soundpack::SoundKind::INTERFACE);
    refresh_tab(state, stream_tab, &soundpack::SoundKind::STREAM_EVENTS);
}

fn refresh_tab(
    state: &Rc<RefCell<ManagerState>>,
    controls: &TabControls,
    kinds: &[soundpack::SoundKind],
) {
    let (project, source_paths) = {
        let state = state.borrow();
        (state.project.clone(), state.source_paths.clone())
    };
    let Some(project) = project else {
        set_source_path_value(state, controls, "");
        set_tab_enabled(controls, false);
        return;
    };
    set_tab_enabled(controls, true);
    let Some(kind) = selected_sound(&controls.sounds, kinds) else {
        set_source_path_value(state, controls, "");
        controls.test.enable(false);
        return;
    };
    let path = source_paths
        .get(&kind)
        .cloned()
        .or_else(|| first_project_variant(&project, kind).map(|path| path.display().to_string()))
        .unwrap_or_default();
    set_source_path_value(state, controls, &path);
    controls.test.enable(true);
}

fn collect_assignments(
    state: &Rc<RefCell<ManagerState>>,
    project: &Path,
) -> HashMap<soundpack::SoundKind, PathBuf> {
    let source_paths = state.borrow().source_paths.clone();
    let mut assignments = HashMap::new();
    for sound in soundpack::SoundKind::ALL {
        if let Some(path) = source_paths.get(&sound) {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                assignments.insert(sound, PathBuf::from(trimmed));
            }
        } else if let Some(path) = first_project_variant(project, sound) {
            assignments.insert(sound, path);
        }
    }
    assignments
}

fn first_project_variant(project: &Path, sound: soundpack::SoundKind) -> Option<PathBuf> {
    soundpack::project_variants(project, sound)
        .ok()?
        .into_iter()
        .next()
}

fn set_source_path_value(state: &Rc<RefCell<ManagerState>>, controls: &TabControls, value: &str) {
    state.borrow_mut().updating_source_path = true;
    controls.source_path.set_value(value);
    state.borrow_mut().updating_source_path = false;
}

fn remember_source_path(
    state: &Rc<RefCell<ManagerState>>,
    controls: &TabControls,
    kinds: &[soundpack::SoundKind],
) {
    let Some(kind) = selected_sound(&controls.sounds, kinds) else {
        return;
    };
    let mut state = state.borrow_mut();
    if state.updating_source_path {
        return;
    }
    state
        .source_paths
        .insert(kind, controls.source_path.get_value());
}

fn set_tab_enabled(controls: &TabControls, enabled: bool) {
    controls.sounds.enable(enabled);
    controls.source_path.enable(enabled);
    controls.browse.enable(enabled);
    controls.test.enable(enabled);
}

fn selected_sound(list: &ListBox, kinds: &[soundpack::SoundKind]) -> Option<soundpack::SoundKind> {
    let index = list.get_selection().unwrap_or(0) as usize;
    kinds.get(index).copied()
}

fn test_play(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let escaped = path.display().to_string().replace('\'', "''");
    let script = format!(
        "$player = New-Object System.Media.SoundPlayer '{}'; $player.PlaySync()",
        escaped
    );
    Command::new("powershell.exe")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn show_error(parent: &dyn WxWidget, message: &str) {
    MessageDialog::builder(parent, message, "Sound Pack Manager")
        .with_style(MessageDialogStyle::OK)
        .build()
        .show_modal();
}

fn show_info(parent: &dyn WxWidget, message: &str) {
    MessageDialog::builder(parent, message, "Sound Pack Manager")
        .with_style(MessageDialogStyle::OK)
        .build()
        .show_modal();
}
