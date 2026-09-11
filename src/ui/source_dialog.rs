//! The shell every source edit dialog shares, and the Effects page inside it.
//!
//! A source's settings used to be five unrelated dialogs — two of them stock
//! `SingleChoiceDialog`s with no panel of their own. Built-in effects have to
//! be reachable on *every* source, including kinds that do not exist yet, so
//! the shell is here rather than repeated per kind: [`Shell::new`] builds the
//! dialog, its two-page `Notebook` and the shared OK/Cancel row, and each
//! kind's dialog fills in the first page and reads its own controls back.
//!
//! The buttons live on the dialog rather than on a page, because they confirm
//! the kind settings, and switching to Effects must not take OK away.
//!
//! They do not govern the Effects page, which applies as it goes: a ducker is
//! committed when *its own* settings dialog is confirmed, in the same way the
//! Sends dialog applies a level as it is moved. Adjusting an effect is
//! something you do by ear against live audio, and an OK two dialogs up is too
//! far from the sound to be the thing that commits it. Applying goes through
//! `App::sync_source_effects`, never a whole-list re-sync — that would respawn
//! every capture thread in the app for a slider nudge.
//!
//! `ok` is a [`super::ok_button`], so it carries the private `ID_CONFIRM` and
//! a confirm handler may `return` without closing the dialog — which is what
//! the Desktop Audio page's refusal depends on.

use super::App;
use crate::config::{DuckerConfig, EffectConfig, SourceConfig};
use crate::state::{ListEdit, move_down, move_up};
use crate::t;
use std::cell::RefCell;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown when a source has no effects yet. See [`super::list`].
fn no_effects() -> String {
    t!("No effects")
}

/// The dialog, its notebook and the shared buttons.
pub struct Shell {
    pub dialog: Dialog,
    pub notebook: Notebook,
    pub ok: Button,
    pub cancel: Button,
}

impl Shell {
    pub fn new(parent: &Frame, title: &str, width: i32, height: i32, resizable: bool) -> Self {
        let style = if resizable {
            DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder
        } else {
            DialogStyle::DefaultDialogStyle
        };
        let dialog = Dialog::builder(parent, title)
            .with_style(style)
            .with_size(width, height)
            .build();
        let notebook = Notebook::builder(&dialog).build();
        let ok = super::ok_button(&dialog, &t!("OK"));
        // `ID_CANCEL` is what wx maps Escape to; without it Escape does nothing.
        let cancel = Button::builder(&dialog)
            .with_id(ID_CANCEL)
            .with_label(&t!("Cancel"))
            .build();
        Shell {
            dialog,
            notebook,
            ok,
            cancel,
        }
    }

    /// Adds the kind's own settings as the first, selected page.
    pub fn add_settings_page<W: WxWidget>(&self, page: &W, label: &str) {
        self.notebook.add_page(page, label, true, None);
    }

    /// Shows the dialog with `first` focused rather than the notebook's tab
    /// row.
    ///
    /// wx hands the initial focus to the first control in tab order, which for
    /// a tabbed dialog is the tab row itself. That is a fine place to be if you
    /// came to change tabs and a poor one otherwise: a screen-reader user
    /// arrives hearing "Application tab" and has to Tab past it before reaching
    /// anything they can set. Every kind names the control it wants instead.
    ///
    /// Focus is claimed twice, and both are needed. The direct call is what
    /// works once wx has realised the controls; but `::SetFocus` on a window
    /// that is not visible yet is a no-op on Windows, and the dialog is not
    /// shown until `show_modal`, at which point wx gives focus to the first
    /// control in tab order itself. So a one-shot timer takes it again from
    /// inside the modal loop, where the window really is up. It is stopped on
    /// the way out so it cannot outlive the window that owns it.
    pub fn show_modal_focused<W: WxWidget + Copy + 'static>(&self, first: &W) -> i32 {
        first.set_focus();
        let timer = Timer::new(&self.dialog);
        let first = *first;
        timer.on_tick(move |_| first.set_focus());
        // Windows clamps this to its minimum timer period; the point is "as
        // soon as the loop is running", not a delay anyone could perceive.
        timer.start(0, true);
        let code = self.dialog.show_modal();
        timer.stop();
        code
    }

    /// Lays the notebook and buttons out, and wires Cancel. Call once, after
    /// both pages exist.
    pub fn finish(&self) {
        let buttons = BoxSizer::builder(Orientation::Horizontal).build();
        buttons.add(&self.ok, 0, SizerFlag::All, 4);
        buttons.add(&self.cancel, 0, SizerFlag::All, 4);
        let sizer = BoxSizer::builder(Orientation::Vertical).build();
        sizer.add(&self.notebook, 1, SizerFlag::Expand | SizerFlag::All, 4);
        sizer.add_sizer(&buttons, 0, SizerFlag::AlignRight, 0);
        self.dialog.set_sizer(sizer, true);
        let dialog = self.dialog;
        self.cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }
}

/// Builds the Effects page into the shell's notebook.
///
/// `siblings` is the scene's source list as it stands, used both to label the
/// effects and to offer the ducker something to listen to; `source_index` is
/// which of them is being edited, so it can be kept out of its own key list.
///
/// Every change is written and applied as it is made — see the module docs.
pub fn add_effects_page(
    app: &Rc<App>,
    shell: &Shell,
    scene_index: usize,
    siblings: &[SourceConfig],
    source_index: usize,
    effects: Vec<EffectConfig>,
) {
    let page = Panel::builder(&shell.notebook).build();
    shell.notebook.add_page(&page, &t!("Effects"), false, None);
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // The list takes its accessible name from the control in front of it, so
    // this label is not decoration; see `super::native_acc`.
    let effects_label = t!("Effects");
    let label = StaticText::builder(&page)
        .with_label(&effects_label)
        .build();
    let list = ListBox::builder(&page).build();
    super::native_acc::install(&list, &effects_label);
    super::help::tag(&list, "dialog.source.effects.list", "Effects list");

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let add = Button::builder(&page).with_label(&t!("&Add")).build();
    let edit = Button::builder(&page).with_label(&t!("Edit...")).build();
    let remove = Button::builder(&page).with_label(&t!("Remove")).build();
    let up = Button::builder(&page).with_label(&t!("Move up")).build();
    let down = Button::builder(&page).with_label(&t!("Move down")).build();
    super::help::tag(&add, "dialog.source.effects.add", "Add effect button");
    super::help::tag(&edit, "dialog.source.effects.edit", "Edit effect button");
    super::help::tag(
        &remove,
        "dialog.source.effects.remove",
        "Remove effect button",
    );
    super::help::tag(&up, "dialog.source.effects.moveUp", "Move effect up button");
    super::help::tag(
        &down,
        "dialog.source.effects.moveDown",
        "Move effect down button",
    );
    for b in [&add, &edit, &remove, &up, &down] {
        buttons.add(b, 0, SizerFlag::All, 4);
    }

    sizer.add(&label, 0, SizerFlag::All, 4);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    page.set_sizer(sizer, true);

    let effects = Rc::new(RefCell::new(effects));
    // The names the ducker may listen to, and the labels they read as. Built
    // once: the scene cannot change while this modal is up.
    let keys = Rc::new(key_choices(app, siblings, source_index));

    // Writes the list through to the config and the engine, then refills the
    // control. `select` names the row to land on afterwards, and is only ever
    // `Some` for a deliberate edit — the one time a list in this app may move
    // its own selection.
    let refresh: Rc<dyn Fn(Option<u32>)> = {
        let effects = effects.clone();
        let keys = keys.clone();
        let app = app.clone();
        Rc::new(move |select: Option<u32>| {
            apply(&app, scene_index, source_index, &effects.borrow());
            let labels: Vec<String> = effects
                .borrow()
                .iter()
                .enumerate()
                .map(|(i, effect)| row_label(i, effect, &keys))
                .collect();
            super::list::sync(&list, &labels, &no_effects());
            if let Some(row) = select
                && (row as usize) < labels.len()
            {
                list.set_selection(row, true);
            }
        })
    };
    // Straight to `fill`: `refresh` writes through, and the page opening is not
    // an edit.
    {
        let labels: Vec<String> = effects
            .borrow()
            .iter()
            .enumerate()
            .map(|(i, effect)| row_label(i, effect, &keys))
            .collect();
        super::list::fill(&list, &labels, &no_effects());
    }

    let dialog = shell.dialog;

    let add_effect: Rc<dyn Fn()> = {
        let effects = effects.clone();
        let keys = keys.clone();
        let refresh = refresh.clone();
        Rc::new(move || {
            // A picker even with one entry today: the whole point of a series
            // of built-in effects is that the second one is a row in
            // `available` and an arm in `configure`, and nothing else.
            let available = available_effects();
            let labels: Vec<&str> = available.iter().map(|e| e.type_display_name()).collect();
            let picker = SingleChoiceDialog::builder(
                &dialog,
                &t!("What kind of effect?"),
                &t!("Add effect"),
                &labels,
            )
            .build();
            super::native_acc::install_in_dialog(&picker, &t!("What kind of effect?"));
            picker.set_selection(0);
            let chosen = (picker.show_modal() == ID_OK)
                .then(|| picker.get_selection())
                .and_then(|row| available.get(row.max(0) as usize).cloned());
            picker.destroy();
            let Some(fresh) = chosen else {
                return;
            };
            // Configured straight away, the way adding a source opens its
            // settings: an unconfigured ducker listens to nothing and would sit
            // in the list doing nothing with no hint that it needs a visit.
            let Some(configured) = configure(&dialog, &fresh, &keys) else {
                return;
            };
            let row = {
                let mut effects = effects.borrow_mut();
                effects.push(configured);
                effects.len() as u32 - 1
            };
            refresh(Some(row));
        })
    };

    let edit_effect: Rc<dyn Fn()> = {
        let effects = effects.clone();
        let keys = keys.clone();
        let refresh = refresh.clone();
        Rc::new(move || {
            let len = effects.borrow().len();
            let Some(index) = super::list::selection(&list, len) else {
                return;
            };
            // Cloned out from under the borrow: the dialog below pumps events.
            let current = effects.borrow()[index].clone();
            let Some(configured) = configure(&dialog, &current, &keys) else {
                return;
            };
            effects.borrow_mut()[index] = configured;
            refresh(Some(index as u32));
        })
    };

    let remove_effect: Rc<dyn Fn()> = {
        let effects = effects.clone();
        let refresh = refresh.clone();
        Rc::new(move || {
            let len = effects.borrow().len();
            let Some(index) = super::list::selection(&list, len) else {
                return;
            };
            let remaining = {
                let mut effects = effects.borrow_mut();
                effects.remove(index);
                effects.len()
            };
            // Land on what took its place, or on the new last row.
            let select = (remaining > 0).then(|| index.min(remaining - 1) as u32);
            refresh(select);
        })
    };

    let move_effect: Rc<dyn Fn(bool)> = {
        let effects = effects.clone();
        let refresh = refresh.clone();
        Rc::new(move |towards_start: bool| {
            let len = effects.borrow().len();
            let Some(index) = super::list::selection(&list, len) else {
                return;
            };
            let changed = {
                let mut effects = effects.borrow_mut();
                if towards_start {
                    move_up(&mut effects, index)
                } else {
                    move_down(&mut effects, index)
                }
            };
            if changed == ListEdit::Changed {
                let row = if towards_start { index - 1 } else { index + 1 };
                refresh(Some(row as u32));
            }
        })
    };

    {
        let add_effect = add_effect.clone();
        add.on_click(move |_| add_effect());
    }
    {
        let edit_effect = edit_effect.clone();
        edit.on_click(move |_| edit_effect());
    }
    {
        let remove_effect = remove_effect.clone();
        remove.on_click(move |_| remove_effect());
    }
    {
        let move_effect = move_effect.clone();
        up.on_click(move |_| move_effect(true));
    }
    {
        let move_effect = move_effect.clone();
        down.on_click(move |_| move_effect(false));
    }
    {
        let edit_effect = edit_effect.clone();
        list.on_item_double_clicked(move |_| edit_effect());
    }
    {
        // The same chords the Scenes and Sources lists use, so reordering is
        // one habit across the app.
        let move_effect = move_effect.clone();
        let remove_effect = remove_effect.clone();
        list.on_key_down(move |event| match super::key_of(&event) {
            Some((super::WXK_UP, true)) => move_effect(true),
            Some((super::WXK_DOWN, true)) => move_effect(false),
            Some((super::WXK_DELETE, _)) => remove_effect(),
            _ => event.skip(true),
        });
    }
}

/// Writes one source's effects to the settings file and to the engine.
///
/// The indices came from a list selection taken before the dialog opened, so
/// they can go stale while it is up; dropping the edit silently is the one
/// outcome that must not happen quietly.
fn apply(app: &Rc<App>, scene_index: usize, source_index: usize, effects: &[EffectConfig]) {
    {
        let mut config = app.config.borrow_mut();
        match config
            .scenes
            .scenes
            .get_mut(scene_index)
            .and_then(|scene| scene.sources.get_mut(source_index))
        {
            Some(source) => source.effects = effects.to_vec(),
            None => {
                log::error!(
                    "Discarding an effects edit: scene {scene_index}, source {source_index} no longer exists"
                );
                return;
            }
        }
    }
    app.save_config();
    app.sync_source_effects(scene_index, source_index);
}

/// Every built-in effect that can be added, in the order the picker offers
/// them, each carrying its own defaults.
fn available_effects() -> Vec<EffectConfig> {
    vec![EffectConfig::Ducker(DuckerConfig::default())]
}

/// Opens the settings dialog belonging to whichever effect this is.
fn configure(parent: &Dialog, effect: &EffectConfig, keys: &[KeyChoice]) -> Option<EffectConfig> {
    match effect {
        EffectConfig::Ducker(config) => {
            super::ducker_dialog::edit(parent, config, keys).map(EffectConfig::Ducker)
        }
    }
}

/// How one effect reads in the list.
fn row_label(index: usize, effect: &EffectConfig, keys: &[KeyChoice]) -> String {
    match effect {
        EffectConfig::Ducker(config) => {
            let listening = keys
                .iter()
                .find(|k| k.name == config.key)
                .map(|k| k.label.as_str());
            match listening {
                Some(label) => format!(
                    "{}. Auto-ducker, listening to {label}, ducks to {}%",
                    index + 1,
                    config.duck_to
                ),
                // Covers both "not configured yet" and "the source it listened
                // to is gone" — in either case it does nothing at all, and
                // saying so here is the only way the user finds that out.
                None => format!("{}. Auto-ducker, listening to nothing", index + 1),
            }
        }
    }
}

/// One source a ducker may listen to: the identity key it is stored under, and
/// the name the user knows it by.
pub struct KeyChoice {
    pub name: String,
    pub label: String,
}

/// The other sources in this scene, in list order.
///
/// A source may not key off itself — it would duck whenever it made a sound,
/// which is a feedback loop with a gain control on it — so it is left out here
/// rather than filtered later.
fn key_choices(app: &Rc<App>, siblings: &[SourceConfig], source_index: usize) -> Vec<KeyChoice> {
    let ctx = app.name_context(siblings);
    let labels = crate::source_name::list_labels(siblings, &ctx);
    siblings
        .iter()
        .zip(labels)
        .enumerate()
        .filter(|(i, _)| *i != source_index)
        .map(|(_, (source, label))| KeyChoice {
            name: source.name.clone(),
            label,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Vec<KeyChoice> {
        vec![KeyChoice {
            name: "Microphone".into(),
            label: "Microphone (Headset)".into(),
        }]
    }

    fn ducker(key: &str) -> EffectConfig {
        EffectConfig::Ducker(DuckerConfig {
            key: key.into(),
            duck_to: 25,
            ..Default::default()
        })
    }

    #[test]
    fn a_configured_ducker_names_what_it_listens_to() {
        assert_eq!(
            row_label(0, &ducker("Microphone"), &keys()),
            "1. Auto-ducker, listening to Microphone (Headset), ducks to 25%"
        );
    }

    /// Rows are numbered from one, because the number is what tells the user
    /// the processing order.
    #[test]
    fn rows_are_numbered_from_one() {
        assert!(row_label(2, &ducker("Microphone"), &keys()).starts_with("3. "));
    }

    /// Both "never configured" and "the source it listened to has been
    /// deleted" land here, and both mean the effect does nothing. Saying so in
    /// the row is the only way the user finds out.
    #[test]
    fn an_unusable_key_says_it_is_listening_to_nothing() {
        assert_eq!(
            row_label(0, &ducker(""), &keys()),
            "1. Auto-ducker, listening to nothing"
        );
        assert_eq!(
            row_label(0, &ducker("Deleted source"), &keys()),
            "1. Auto-ducker, listening to nothing"
        );
    }
}
