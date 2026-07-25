//! Standalone Sound Pack Manager.
#[path = "../soundpack.rs"]
mod soundpack;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
};
use wxdragon::prelude::*;

#[derive(Default)]
struct ManagerState {
    project: Option<PathBuf>,
}

#[derive(Clone)]
struct TabControls {
    sounds: ListBox,
    source_path: TextCtrl,
    browse: Button,
    add: Button,
    variants: ListBox,
    remove: Button,
    test: Button,
}

fn main() {
    let _ = wxdragon::main(|_| {
        let frame = Frame::builder()
            .with_title("Pubsplash Sound Pack Manager")
            .with_size(Size::new(820, 560))
            .build();
        let state = Rc::new(RefCell::new(ManagerState::default()));

        let root = Panel::builder(&frame).build();
        let outer = BoxSizer::builder(Orientation::Vertical).build();

        let toolbar = BoxSizer::builder(Orientation::Horizontal).build();
        let new_project = Button::builder(&root).with_label("&New...").build();
        let open_project = Button::builder(&root).with_label("&Open...").build();
        let compile = Button::builder(&root).with_label("&Compile...").build();
        toolbar.add(&new_project, 0, SizerFlag::All, 4);
        toolbar.add(&open_project, 0, SizerFlag::All, 4);
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
        );
        let stream_tab = build_tab(
            &notebook,
            "Stream events",
            &soundpack::SoundKind::STREAM_EVENTS,
        );
        outer.add(&notebook, 1, SizerFlag::Expand | SizerFlag::All, 4);
        root.set_sizer_and_fit(outer, true);

        refresh_all(
            &state,
            &project_label,
            &interface_tab,
            &stream_tab,
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

fn build_tab(notebook: &Notebook, title: &str, kinds: &[soundpack::SoundKind]) -> TabControls {
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
    let add = Button::builder(&panel).with_label("&Add Variant").build();
    source_row.add(&source_path, 1, SizerFlag::Expand | SizerFlag::All, 4);
    source_row.add(&browse, 0, SizerFlag::All, 4);
    source_row.add(&add, 0, SizerFlag::All, 4);

    let variants_label = StaticText::builder(&panel)
        .with_label("Variants in project")
        .build();
    let variants = ListBox::builder(&panel).build();
    let action_row = BoxSizer::builder(Orientation::Horizontal).build();
    let remove = Button::builder(&panel)
        .with_label("&Remove Variant")
        .build();
    let test = Button::builder(&panel).with_label("&Test").build();
    action_row.add(&remove, 0, SizerFlag::All, 4);
    action_row.add(&test, 0, SizerFlag::All, 4);

    right.add(&source_label, 0, SizerFlag::All, 4);
    right.add_sizer(&source_row, 0, SizerFlag::Expand, 0);
    right.add(&variants_label, 0, SizerFlag::All, 4);
    right.add(&variants, 1, SizerFlag::Expand | SizerFlag::All, 4);
    right.add_sizer(&action_row, 0, SizerFlag::All, 0);

    let controls = TabControls {
        sounds,
        source_path,
        browse,
        add,
        variants,
        remove,
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
        let controls_for_add = controls.clone();
        let kinds_for_add = kinds.to_vec();
        let panel_for_add = panel.clone();
        controls.add.clone().on_click(move |_| {
            let Some(project) = current_project_from_parent(&panel_for_add) else {
                return;
            };
            let Some(kind) = selected_sound(&controls_for_add.sounds, &kinds_for_add) else {
                return;
            };
            let source = controls_for_add.source_path.get_value();
            if source.trim().is_empty() {
                show_error(&panel_for_add, "Choose or type a WAV path first.");
                return;
            }
            match soundpack::add_variant(&project, kind, Path::new(source.trim())) {
                Ok(_) => refresh_tab(&Some(project), &controls_for_add, &kinds_for_add),
                Err(err) => show_error(&panel_for_add, &err),
            }
        });
    }

    outer.add(&controls.sounds, 1, SizerFlag::Expand | SizerFlag::All, 4);
    outer.add_sizer(&right, 2, SizerFlag::Expand, 0);
    panel.set_sizer_and_fit(outer, true);
    notebook.add_page(&panel, title, false, None);
    controls
}

thread_local! {
    static CURRENT_PROJECT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

fn current_project_from_parent(_parent: &dyn WxWidget) -> Option<PathBuf> {
    CURRENT_PROJECT.with(|p| p.borrow().clone())
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
    compile: &Button,
) {
    {
        let frame = frame.clone();
        let state = Rc::clone(state);
        let project_label = project_label.clone();
        let interface_tab = interface_tab.clone();
        let stream_tab = stream_tab.clone();
        let compile = compile.clone();
        new_project.on_click(move |_| {
            let dialog =
                DirDialog::builder(&frame, "Choose a new sound pack project folder", "").build();
            if dialog.show_modal() != ID_OK {
                return;
            }
            let Some(path) = dialog.get_path().map(PathBuf::from) else {
                return;
            };
            match soundpack::create_project(&path) {
                Ok(()) => {
                    state.borrow_mut().project = Some(path.clone());
                    CURRENT_PROJECT.with(|p| *p.borrow_mut() = Some(path));
                    refresh_all(
                        &state,
                        &project_label,
                        &interface_tab,
                        &stream_tab,
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
                    state.borrow_mut().project = Some(path.clone());
                    CURRENT_PROJECT.with(|p| *p.borrow_mut() = Some(path));
                    refresh_all(
                        &state,
                        &project_label,
                        &interface_tab,
                        &stream_tab,
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
            refresh_tab(&state.borrow().project, &controls, &kinds);
        });
    }
    {
        let controls = controls.clone();
        let state = Rc::clone(state);
        let kinds = kinds.to_vec();
        controls.variants.clone().on_selection_changed(move |_| {
            if let Some(path) = selected_variant_path(&state.borrow().project, &controls, &kinds) {
                controls.source_path.set_value(&path.display().to_string());
            }
        });
    }
    {
        let frame = frame.clone();
        let controls = controls.clone();
        let state = Rc::clone(state);
        let kinds = kinds.to_vec();
        controls.remove.clone().on_click(move |_| {
            let Some(project) = state.borrow().project.clone() else {
                show_error(&frame, "Open or create a sound pack project first.");
                return;
            };
            let Some(kind) = selected_sound(&controls.sounds, &kinds) else {
                return;
            };
            let Some(index) = controls.variants.get_selection().map(|i| i as usize) else {
                show_error(&frame, "Select a variant to remove.");
                return;
            };
            match soundpack::remove_variant(&project, kind, index) {
                Ok(()) => refresh_tab(&Some(project), &controls, &kinds),
                Err(err) => show_error(&frame, &err),
            }
        });
    }
    {
        let frame = frame.clone();
        let controls = controls.clone();
        let state = Rc::clone(state);
        let kinds = kinds.to_vec();
        controls.test.clone().on_click(move |_| {
            let path =
                selected_variant_path(&state.borrow().project, &controls, &kinds).or_else(|| {
                    let typed = controls.source_path.get_value();
                    let trimmed = typed.trim();
                    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
                });
            let Some(path) = path else {
                show_error(&frame, "Select or type a WAV path to test.");
                return;
            };
            if let Err(err) = test_play(&path) {
                show_error(&frame, &err);
            }
        });
    }
}

fn refresh_all(
    state: &Rc<RefCell<ManagerState>>,
    project_label: &StaticText,
    interface_tab: &TabControls,
    stream_tab: &TabControls,
    compile: &Button,
) {
    let project = state.borrow().project.clone();
    compile.enable(project.is_some());
    CURRENT_PROJECT.with(|p| *p.borrow_mut() = project.clone());
    if let Some(project) = &project {
        let revision = soundpack::read_project_manifest(project)
            .map(|m| m.revision.to_string())
            .unwrap_or_else(|_| "?".into());
        project_label.set_label(&format!(
            "Project: {} (revision {revision})",
            project.display()
        ));
    } else {
        project_label.set_label("No sound pack project is open");
    }
    refresh_tab(&project, interface_tab, &soundpack::SoundKind::INTERFACE);
    refresh_tab(&project, stream_tab, &soundpack::SoundKind::STREAM_EVENTS);
}

fn refresh_tab(project: &Option<PathBuf>, controls: &TabControls, kinds: &[soundpack::SoundKind]) {
    controls.variants.clear();
    let Some(project) = project else {
        set_tab_enabled(controls, false);
        return;
    };
    set_tab_enabled(controls, true);
    let Some(kind) = selected_sound(&controls.sounds, kinds) else {
        controls.remove.enable(false);
        controls.test.enable(false);
        return;
    };
    let variants = soundpack::project_variants(project, kind).unwrap_or_default();
    for path in &variants {
        controls.variants.append(&path.display().to_string());
    }
    if !variants.is_empty() {
        controls.variants.set_selection(0, true);
        controls
            .source_path
            .set_value(&variants[0].display().to_string());
    }
    controls.remove.enable(!variants.is_empty());
    controls.test.enable(true);
}

fn set_tab_enabled(controls: &TabControls, enabled: bool) {
    controls.sounds.enable(enabled);
    controls.source_path.enable(enabled);
    controls.browse.enable(enabled);
    controls.add.enable(enabled);
    controls.variants.enable(enabled);
    controls.remove.enable(enabled);
    controls.test.enable(enabled);
}

fn selected_sound(list: &ListBox, kinds: &[soundpack::SoundKind]) -> Option<soundpack::SoundKind> {
    let index = list.get_selection().unwrap_or(0) as usize;
    kinds.get(index).copied()
}

fn selected_variant_path(
    project: &Option<PathBuf>,
    controls: &TabControls,
    kinds: &[soundpack::SoundKind],
) -> Option<PathBuf> {
    let project = project.as_ref()?;
    let kind = selected_sound(&controls.sounds, kinds)?;
    let index = controls.variants.get_selection()? as usize;
    soundpack::project_variants(project, kind)
        .ok()?
        .get(index)
        .cloned()
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
