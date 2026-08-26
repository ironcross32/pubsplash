//! Picks the executable an Application source captures.
//!
//! The list is the point of this. Typing a process name from memory is the one
//! thing a screen-reader user cannot check before committing to it, and a wrong
//! guess produces a source that is silent with no audible complaint. So the
//! default view is the short list of apps that have actually made a sound (see
//! `audio::app_list`), a checkbox widens it to every app with a window, and
//! typing a name by hand survives as the escape hatch for an app that has not
//! been started yet.
//!
//! It builds straight into the Application source's settings page rather than
//! being a dialog of its own. Choosing the application *is* what that page is
//! for, so putting it behind a button meant a second window to open and a
//! second one to confirm before the real choice was even on screen.
//!
//! A name typed by hand is folded back into the list as a selected row rather
//! than being kept somewhere separate, so there is exactly one place the page's
//! answer comes from: whatever row is selected.

use crate::audio::app_list::{self, AppCandidate};
use std::cell::RefCell;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown when no application matches the current view. See [`super::list`].
const NO_APPLICATIONS: &str = "No applications";

/// The application chooser, once built into a page.
pub struct Chooser {
    /// The control the page should focus when it opens.
    pub list: ListBox,
    selected: Rc<dyn Fn() -> Option<String>>,
}

impl Chooser {
    /// The chosen executable name, or `None` when the list is empty.
    pub fn selected(&self) -> Option<String> {
        (self.selected)()
    }
}

/// Builds the chooser into `page`, appending its controls to `sizer`.
///
/// `dialog` is the window the "Type a name" entry belongs to, and `current` is
/// the source's configured name (empty for a brand-new source). A configured
/// app that is not running is offered as its own row, so the setting stays
/// visible and reachable rather than silently disappearing when it is closed.
pub fn build(page: &Panel, sizer: &BoxSizer, dialog: &Dialog, current: &str) -> Chooser {
    let intro = StaticText::builder(page)
        .with_label("Which application should this source capture?")
        .build();
    let list = ListBox::builder(page).build();
    super::native_acc::install(&list, "Running applications");
    super::help::tag(&list, "dialog.appPicker.list", "Running applications list");
    let sound_only = CheckBox::builder(page)
        .with_label("Only show apps that have played sound")
        .build();
    sound_only.set_value(true);
    super::set_accessible_name(&sound_only, "Only show apps that have played sound");
    super::help::tag(
        &sound_only,
        "dialog.appPicker.soundOnly",
        "Only show apps that have played sound checkbox",
    );

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let refresh = Button::builder(page).with_label("Refresh").build();
    let type_name = Button::builder(page).with_label("Type a name...").build();
    super::set_accessible_name(&refresh, "Refresh");
    super::set_accessible_name(&type_name, "Type a name");
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
    buttons.add(&refresh, 0, SizerFlag::All, 4);
    buttons.add(&type_name, 0, SizerFlag::All, 4);

    sizer.add(&intro, 0, SizerFlag::All, 8);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&sound_only, 0, SizerFlag::All, 8);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);

    // Enumerated once per Refresh, not once per view: re-enumerating for the
    // checkbox would let the two views disagree about what is playing.
    let apps: Rc<RefCell<Vec<AppCandidate>>> = Rc::new(RefCell::new(app_list::list_apps()));
    // The exe name behind each visible row, parallel to the ListBox.
    let shown: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    // The configured name, which a typed name replaces. Held rather than copied
    // so the typed one becomes an ordinary row like any other.
    let configured = Rc::new(RefCell::new(current.trim().to_string()));

    let repopulate: Rc<dyn Fn(Option<String>)> = {
        let apps = apps.clone();
        let shown = shown.clone();
        let configured = configured.clone();
        Rc::new(move |keep: Option<String>| {
            let apps = apps.borrow();
            let configured = configured.borrow().clone();
            let only_sounding = sound_only.get_value();
            let mut rows: Vec<(String, String)> = Vec::new();
            // The configured app goes first when it is not among the running
            // ones, so the current setting is always visible and reachable.
            if !configured.is_empty()
                && !apps
                    .iter()
                    .any(|a| crate::audio::device::name_matches(&configured, &a.exe))
            {
                rows.push((configured.clone(), format!("{configured} (not running)")));
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
            let keep = keep.or_else(|| (!configured.is_empty()).then(|| configured.clone()));
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
        })
    };
    repopulate(None);

    // The selection is remembered across a view change or a refresh, so the
    // checkbox does not silently move what the page would commit to.
    let selected: Rc<dyn Fn() -> Option<String>> = {
        let shown = shown.clone();
        Rc::new(move || {
            let shown = shown.borrow();
            let index = super::list::selection(&list, shown.len())?;
            shown.get(index).cloned()
        })
    };

    {
        let repopulate = repopulate.clone();
        let selected = selected.clone();
        sound_only
            .clone()
            .on_toggled(move |_| repopulate(selected()));
    }
    {
        let repopulate = repopulate.clone();
        let selected = selected.clone();
        let apps = apps.clone();
        refresh.on_click(move |_| {
            let keep = selected();
            *apps.borrow_mut() = app_list::list_apps();
            repopulate(keep);
        });
    }
    {
        let repopulate = repopulate.clone();
        let configured = configured.clone();
        let dialog = *dialog;
        type_name.on_click(move |_| {
            let current = configured.borrow().clone();
            let Some(name) = type_a_name(&dialog, &current) else {
                return;
            };
            // Folded into the list as the configured row and selected there, so
            // the page has one answer and the user can hear what it is.
            *configured.borrow_mut() = name.clone();
            repopulate(Some(name));
        });
    }

    Chooser { list, selected }
}

/// The manual entry fallback, for an app that is not running yet.
fn type_a_name(parent: &Dialog, current: &str) -> Option<String> {
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
