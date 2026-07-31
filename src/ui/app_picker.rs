//! Picks the executable an Application source captures.
//!
//! The list is the point of this dialog. Typing a process name from memory is
//! the one thing a screen-reader user cannot check before committing to it, and
//! a wrong guess produces a source that is silent with no audible complaint. So
//! the default view is the short list of apps that have actually made a sound
//! (see `audio::app_list`), a checkbox widens it to every app with a window, and
//! typing a name by hand survives as the escape hatch for an app that has not
//! been started yet.

use crate::audio::app_list::{self, AppCandidate};
use std::cell::RefCell;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown when no application matches the current view. See [`super::list`].
const NO_APPLICATIONS: &str = "No applications";

/// What the user asked for.
pub enum Pick {
    /// Capture this executable.
    App(String),
    /// Enter a name by hand instead; the caller opens that dialog once this one
    /// is gone, so no modal is ever nested inside another.
    TypeAName,
    Cancelled,
}

/// Shows the picker.
///
/// `current` is the source's configured name (empty for a brand-new source). A
/// configured app that is not running is offered as its own row so the setting
/// stays visible and selecting it is a harmless no-op.
pub fn pick_application(frame: &Frame, current: &str) -> Pick {
    let dialog = Dialog::builder(frame, "Application source")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(460, 420)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let intro = StaticText::builder(&panel)
        .with_label("Which application should this source capture?")
        .build();
    let list = ListBox::builder(&panel).build();
    super::native_acc::install(&list, "Running applications");
    super::help::tag(&list, "dialog.appPicker.list", "Running applications list");
    let sound_only = CheckBox::builder(&panel)
        .with_label("Only show apps that have &played sound")
        .build();
    sound_only.set_value(true);
    super::set_accessible_name(&sound_only, "Only show apps that have played sound");
    super::help::tag(
        &sound_only,
        "dialog.appPicker.soundOnly",
        "Only show apps that have played sound checkbox",
    );

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    // `ui::ok_button` carries the id-plus-`set_default()` rule Enter depends on;
    // see its doc comment for why either half alone is silently useless.
    let select = super::ok_button(&panel, "&Select");
    let refresh = Button::builder(&panel).with_label("&Refresh").build();
    let type_name = Button::builder(&panel)
        .with_label("&Type a name...")
        .build();
    let cancel = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("&Cancel")
        .build();
    super::help::tag(
        &select,
        "dialog.appPicker.select",
        "Select highlighted application button",
    );
    super::help::tag(
        &refresh,
        "dialog.appPicker.refresh",
        "Refresh the application list button",
    );
    super::help::tag(
        &type_name,
        "dialog.appPicker.typeName",
        "Type an application name button",
    );
    for b in [&select, &refresh, &type_name, &cancel] {
        buttons.add(b, 0, SizerFlag::All, 4);
    }

    sizer.add(&intro, 0, SizerFlag::All, 8);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&sound_only, 0, SizerFlag::All, 8);
    sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    // Enumerated once per Refresh, not once per view: re-enumerating for the
    // checkbox would let the two views disagree about what is playing.
    let apps: Rc<RefCell<Vec<AppCandidate>>> = Rc::new(RefCell::new(app_list::list_apps()));
    // The exe name behind each visible row, parallel to the ListBox.
    let shown: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    // The answer, resolved and stored *while the dialog is still up*.
    //
    // Reading the ListBox after `show_modal` returns is what the first version
    // did, and it lost the user's choice: by then the dialog has been hidden and
    // the handle may no longer resolve, so `get_selection` answers `None` and the
    // dialog looks like it committed while saving nothing. Every commit path now
    // writes here first and `end_modal` second.
    let picked: Rc<RefCell<Option<Pick>>> = Rc::new(RefCell::new(None));
    let current = current.trim().to_string();

    let repopulate = {
        let apps = apps.clone();
        let shown = shown.clone();
        let list = list.clone();
        let sound_only = sound_only.clone();
        let intro = intro.clone();
        let current = current.clone();
        move |keep: Option<String>| {
            let apps = apps.borrow();
            let only_sounding = sound_only.get_value();
            let mut rows: Vec<(String, String)> = Vec::new();
            // The configured app goes first when it is not among the running
            // ones, so the current setting is always visible and reachable.
            if !current.is_empty()
                && !apps
                    .iter()
                    .any(|a| crate::audio::device::name_matches(&current, &a.exe))
            {
                rows.push((current.clone(), format!("{current} (not running)")));
            }
            for app in apps.iter() {
                if only_sounding && !app.has_audio {
                    continue;
                }
                rows.push((
                    app.exe.clone(),
                    format!("{} ({})", app.display_name, app.exe),
                ));
            }

            let labels: Vec<String> = rows.iter().map(|(_, label)| label.clone()).collect();
            super::list::fill(&list, &labels, NO_APPLICATIONS);
            let keep = keep.or_else(|| (!current.is_empty()).then(|| current.clone()));
            let index = keep
                .and_then(|want| {
                    rows.iter()
                        .position(|(exe, _)| crate::audio::device::name_matches(&want, exe))
                })
                .unwrap_or(0);
            if !rows.is_empty() {
                list.set_selection(index as u32, true);
            }
            intro.set_label(if rows.is_empty() {
                "No applications found. Use Type a name to enter one."
            } else {
                "Which application should this source capture?"
            });
            *shown.borrow_mut() = rows.into_iter().map(|(exe, _)| exe).collect();
        }
    };
    repopulate(None);

    // The selection is remembered across a view change or a refresh, so the
    // checkbox does not silently move what Select would commit to.
    let selected_exe = {
        let shown = shown.clone();
        let list = list.clone();
        move || -> Option<String> {
            let shown = shown.borrow();
            let index = super::list::selection(&list, shown.len())?;
            shown.get(index).cloned()
        }
    };

    {
        let repopulate = repopulate.clone();
        let selected_exe = selected_exe.clone();
        sound_only
            .clone()
            .on_toggled(move |_| repopulate(selected_exe()));
    }
    {
        let repopulate = repopulate.clone();
        let selected_exe = selected_exe.clone();
        let apps = apps.clone();
        refresh.on_click(move |_| {
            let keep = selected_exe();
            *apps.borrow_mut() = app_list::list_apps();
            repopulate(keep);
        });
    }
    // Commits the highlighted row. First writer wins, so a duplicated event
    // (Enter reaching both the list and the affirmative button, say) cannot
    // overwrite the answer with a staler one.
    let commit = {
        let picked = picked.clone();
        let selected_exe = selected_exe.clone();
        let dialog = dialog.clone();
        move || {
            let exe = selected_exe();
            log::debug!("app picker: committing {exe:?}");
            if let Some(exe) = exe {
                let mut picked = picked.borrow_mut();
                if picked.is_none() {
                    *picked = Some(Pick::App(exe));
                }
            }
            dialog.end_modal(ID_OK);
        }
    };
    {
        let commit = commit.clone();
        select.on_click(move |_| commit());
    }
    // Double-clicking a row is the other way people expect to choose from a list.
    {
        let commit = commit.clone();
        list.clone().on_item_double_clicked(move |_| commit());
    }
    {
        let dialog = dialog.clone();
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }
    {
        let dialog = dialog.clone();
        let picked = picked.clone();
        type_name.on_click(move |_| {
            *picked.borrow_mut() = Some(Pick::TypeAName);
            dialog.end_modal(ID_OK);
        });
    }

    let code = dialog.show_modal();
    let outcome = picked.borrow_mut().take();
    dialog.destroy();
    log::debug!(
        "app picker: show_modal returned {code}, outcome {}",
        match &outcome {
            Some(Pick::App(exe)) => exe.as_str(),
            Some(Pick::TypeAName) => "<type a name>",
            _ => "<cancelled>",
        }
    );
    match outcome {
        Some(Pick::App(exe)) if !exe.trim().is_empty() => Pick::App(exe),
        Some(Pick::TypeAName) => Pick::TypeAName,
        _ => Pick::Cancelled,
    }
}

/// The manual entry fallback, for an app that is not running yet. Called by the
/// caller once the picker is gone, never nested inside it.
pub fn type_a_name(parent: &Frame, current: &str) -> Option<String> {
    let entry = TextEntryDialog::builder(
        parent,
        "Name of the application to capture (for example: firefox):",
        "Application source",
    )
    .with_default_value(current)
    .build();
    let value = if entry.show_modal() == ID_OK {
        entry.get_value().map(|v| v.trim().to_string())
    } else {
        None
    };
    entry.destroy();
    value.filter(|v| !v.is_empty())
}
