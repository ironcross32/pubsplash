//! The Keybinds tab in Preferences, and its Add/Edit binding dialog.
//!
//! The list is a **catalogue**, not a view of the config: every action that can
//! be bound gets a row, reading its chord or "Unassigned" after the name. That
//! is what makes an unbound action discoverable by arrowing rather than by
//! knowing it exists. `crate::keybind::catalogue` builds it; the runtime half
//! that actually catches the keys is [`super::keybinds`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wxdragon::prelude::*;

use super::{App, WXK_DELETE, show_error};
use crate::keybind::{
    BindAction, Category, Chord, Specifier, Targets, VK_F1, VK_F6, catalogue, row_label,
};

/// Shown when the catalogue is empty, which cannot happen (the fixed actions are
/// always there) — but every list that could ever empty keeps one, because NVDA
/// announces a zero-item list as "Unknown". See [`super::list`].
const NO_KEYBINDS: &str = "No actions";

pub fn build_tab(app: &Rc<App>, dialog: &Dialog, panel: &Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // The list takes its announced name from this label, not from an accessible
    // name — see `native_acc`. It must stay, and must stay immediately in front.
    let list_label = StaticText::builder(panel).with_label("Keybinds").build();
    let list = ListBox::builder(panel).build();
    super::native_acc::install(&list, "Keybinds");
    super::help::tag(&list, "dialog.preferences.keybinds.list", "Keybinds list");

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    // Mnemonics are dialog-wide, not per tab: `::IsDialogMessage` searches the
    // whole Preferences dialog, so these avoid the letters the other four tabs
    // already claim (a, b, i, k, l, m, n, r) — which is why Add binding is
    // ALT+G and not the obvious ALT+A or ALT+B.
    let add = Button::builder(panel).with_label("Add bindin&g...").build();
    let edit = Button::builder(panel)
        .with_label("&Edit binding...")
        .build();
    let remove = Button::builder(panel).with_label("Remove bin&ding").build();
    let reset = Button::builder(panel)
        .with_label("Reset to de&faults")
        .build();
    super::help::tag(
        &add,
        "dialog.preferences.keybinds.add",
        "Add binding button",
    );
    super::help::tag(
        &edit,
        "dialog.preferences.keybinds.edit",
        "Edit binding button",
    );
    super::help::tag(
        &remove,
        "dialog.preferences.keybinds.remove",
        "Remove binding button",
    );
    super::help::tag(
        &reset,
        "dialog.preferences.keybinds.reset",
        "Reset keybinds to defaults button",
    );
    for button in [&add, &edit, &remove, &reset] {
        buttons.add(button, 0, SizerFlag::All, 4);
    }

    sizer.add(&list_label, 0, SizerFlag::All, 4);
    sizer.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);

    // What each widget row means. Rebuilt with the list, so the two can never
    // drift apart.
    let rows: Rc<RefCell<Vec<BindAction>>> = Rc::new(RefCell::new(Vec::new()));

    // Refills the list and selects `select`, or keeps the row index otherwise.
    // Every caller is a deliberate edit, which is the only time a list in this
    // app may move its own selection.
    let refresh: Rc<dyn Fn(Option<&BindAction>)> = {
        let app = app.clone();
        let list = list.clone();
        let rows = rows.clone();
        Rc::new(move |select: Option<&BindAction>| {
            let (actions, labels) = {
                let config = app.config.borrow();
                let actions = catalogue(&Targets::from_config(&config));
                let labels = actions
                    .iter()
                    .map(|action| row_label(action, config.keybinds.find(action)))
                    .collect::<Vec<_>>();
                (actions, labels)
            };
            let previous = list.get_selection();
            let index = select
                .and_then(|want| actions.iter().position(|a| a == want))
                .or_else(|| previous.map(|i| (i as usize).min(actions.len().saturating_sub(1))))
                .unwrap_or(0);
            *rows.borrow_mut() = actions;
            super::list::fill(&list, &labels, NO_KEYBINDS);
            if !labels.is_empty() {
                list.set_selection(index as u32, true);
            }
        })
    };
    refresh(None);

    // The action the user is standing on, or `None` on the placeholder row.
    let selected: Rc<dyn Fn() -> Option<BindAction>> = {
        let list = list.clone();
        let rows = rows.clone();
        Rc::new(move || {
            let rows = rows.borrow();
            super::list::selection(&list, rows.len()).map(|i| rows[i].clone())
        })
    };

    {
        let app = app.clone();
        let dialog = dialog.clone();
        let refresh = refresh.clone();
        add.on_click(move |_| {
            if let Some(action) = edit_dialog(&app, &dialog, None) {
                refresh(Some(&action));
            }
        });
    }
    {
        let app = app.clone();
        let dialog = dialog.clone();
        let refresh = refresh.clone();
        let selected = selected.clone();
        edit.on_click(move |_| {
            let Some(current) = selected() else {
                return;
            };
            if let Some(action) = edit_dialog(&app, &dialog, Some(current)) {
                refresh(Some(&action));
            }
        });
    }

    let do_remove = {
        let app = app.clone();
        let refresh = refresh.clone();
        let selected = selected.clone();
        move || {
            let Some(action) = selected() else {
                return;
            };
            // Already unassigned: nothing to remove, and saying so beats silence.
            if app.config.borrow().keybinds.find(&action).is_none() {
                super::help::announce("That action has no binding");
                return;
            }
            app.config.borrow_mut().keybinds.remove(&action);
            app.save_config();
            super::keybinds::reload(&app.config.borrow());
            refresh(Some(&action));
            super::help::announce(&format!("{} unassigned", action.label()));
        }
    };
    {
        let do_remove = do_remove.clone();
        remove.on_click(move |_| do_remove());
    }
    list.clone()
        .on_key_down(move |event| match super::key_of(&event) {
            Some((WXK_DELETE, _)) => do_remove(),
            _ => event.skip(true),
        });

    {
        let app = app.clone();
        let dialog = dialog.clone();
        let refresh = refresh.clone();
        reset.on_click(move |_| {
            let confirm = MessageDialog::builder(
                &dialog,
                "Discard every keybinding and restore the defaults, F9 for streaming and F10 for recording?",
                "Reset to defaults",
            )
            .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
            .build();
            if confirm.show_modal() != ID_YES {
                return;
            }
            app.config.borrow_mut().keybinds = crate::keybind::KeybindsConfig::default();
            app.save_config();
            super::keybinds::reload(&app.config.borrow());
            refresh(None);
            super::help::announce("Keybinds reset to defaults");
        });
    }
}

/// The Add/Edit binding dialog. `initial` pre-selects an action when editing.
///
/// Returns the action whose binding changed, so the caller can put the list
/// selection back on it.
fn edit_dialog(app: &Rc<App>, parent: &Dialog, initial: Option<BindAction>) -> Option<BindAction> {
    let editing = initial.is_some();
    let title = if editing {
        "Edit binding"
    } else {
        "Add binding"
    };
    let dialog = Dialog::builder(parent, title)
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(460, 380)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let targets = Targets::from_config(&app.config.borrow());

    let category_label = StaticText::builder(&panel).with_label("Category").build();
    let category_choice = Choice::builder(&panel).build();
    super::set_accessible_name(&category_choice, "Category");
    super::help::tag(
        &category_choice,
        "dialog.keybind.category",
        "Binding category choice",
    );
    for category in Category::ALL {
        category_choice.append(category.label());
    }

    let action_label = StaticText::builder(&panel).with_label("Action").build();
    let action_choice = Choice::builder(&panel).build();
    super::set_accessible_name(&action_choice, "Action");
    super::help::tag(
        &action_choice,
        "dialog.keybind.action",
        "Binding action choice",
    );

    // Only some actions need a target. This pair is shown and hidden as the
    // action changes, and its label and accessible name are rewritten to say
    // which kind of target it is asking for.
    let specifier_label = StaticText::builder(&panel).with_label("Scene").build();
    let specifier_choice = Choice::builder(&panel).build();
    super::set_accessible_name(&specifier_choice, "Scene");
    super::help::tag(
        &specifier_choice,
        "dialog.keybind.specifier",
        "Binding scene, source or bus choice",
    );

    let shortcut_label = StaticText::builder(&panel).with_label("Shortcut").build();
    // Deliberately **not** `TextCtrlStyle::ReadOnly`, however read-only this
    // field is in spirit: `wxTextCtrl::AcceptsFocusFromKeyboard` returns false
    // for a single-line read-only control, so wx drops it out of the Tab order
    // entirely and there is no way to reach it with the keyboard — which is the
    // one thing this control must support. (The read-only chat view escapes that
    // because it is `MultiLine`, which wx exempts.)
    //
    // Nothing can be typed in regardless: while this field has focus the hook in
    // `ui::keybinds` swallows every key except TAB. `on_kill_focus` below puts
    // the chord's label back anyway, so even a mouse-driven paste cannot leave
    // stray text behind.
    let shortcut_input = TextCtrl::builder(&panel).with_value("None").build();
    super::set_accessible_name(&shortcut_input, "Shortcut, None");
    super::help::tag(
        &shortcut_input,
        "dialog.keybind.shortcut",
        "Shortcut capture field",
    );

    let global_check = CheckBox::builder(&panel).with_label("&Global").build();
    super::set_accessible_name(&global_check, "Global");
    super::help::tag(
        &global_check,
        "dialog.keybind.global",
        "Global binding checkbox",
    );

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let ok_button = Button::builder(&panel).with_label("OK").build();
    // `ID_CANCEL` is what wx maps Escape to; without it Escape does nothing.
    let cancel_button = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("Cancel")
        .build();
    buttons.add(&ok_button, 0, SizerFlag::All, 4);
    buttons.add(&cancel_button, 0, SizerFlag::All, 4);

    sizer.add(&category_label, 0, SizerFlag::All, 4);
    sizer.add(&category_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&action_label, 0, SizerFlag::All, 4);
    sizer.add(&action_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&specifier_label, 0, SizerFlag::All, 4);
    sizer.add(&specifier_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&shortcut_label, 0, SizerFlag::All, 4);
    sizer.add(&shortcut_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&global_check, 0, SizerFlag::All, 8);
    sizer.add_sizer(&buttons, 0, SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    // --- wiring ---------------------------------------------------------

    // The action templates currently in the Action dropdown, so a selection
    // index can be turned back into an action.
    let templates: Rc<RefCell<Vec<BindAction>>> = Rc::new(RefCell::new(Vec::new()));
    let chord = Rc::new(Cell::new(Chord::default()));

    // Shows or hides the specifier row for whichever action is selected, and
    // re-labels it. The accessible name is re-set every time because the control
    // has changed meaning — a screen reader would otherwise keep calling a list
    // of buses "Scene".
    let sync_specifier: Rc<dyn Fn(Option<&str>)> = {
        let panel = panel.clone();
        let action_choice = action_choice.clone();
        let specifier_label = specifier_label.clone();
        let specifier_choice = specifier_choice.clone();
        let templates = templates.clone();
        let targets = targets.clone();
        Rc::new(move |select: Option<&str>| {
            let kind = action_choice
                .get_selection()
                .and_then(|i| templates.borrow().get(i as usize).cloned())
                .map(|a| a.specifier_kind())
                .unwrap_or(Specifier::None);
            panel.freeze();
            if kind != Specifier::None {
                let names = targets.for_specifier(kind);
                specifier_choice.clear();
                for name in names {
                    specifier_choice.append(name);
                }
                let index = select
                    .and_then(|want| names.iter().position(|n| n == want))
                    .unwrap_or(0);
                if !names.is_empty() {
                    specifier_choice.set_selection(index as u32);
                }
                specifier_label.set_label(kind.label());
                super::set_accessible_name(&specifier_choice, kind.label());
            }
            specifier_label.show(kind != Specifier::None);
            specifier_choice.show(kind != Specifier::None);
            // After the sizer is installed, so the hidden row's space is given
            // back rather than left as a gap.
            panel.layout();
            panel.thaw();
        })
    };

    // Refills the Action dropdown for a category, then re-syncs the specifier.
    let sync_actions: Rc<dyn Fn(Option<&BindAction>)> = {
        let category_choice = category_choice.clone();
        let action_choice = action_choice.clone();
        let templates = templates.clone();
        let sync_specifier = sync_specifier.clone();
        Rc::new(move |select: Option<&BindAction>| {
            let category = category_choice
                .get_selection()
                .map(|i| Category::ALL[i as usize])
                .unwrap_or(Category::Streaming);
            let actions = category.actions();
            action_choice.clear();
            for action in &actions {
                action_choice.append(action.template_label());
            }
            // Compare by template: the wanted action carries a name, the list
            // holds blanks.
            let index = select
                .and_then(|want| {
                    actions
                        .iter()
                        .position(|a| a.with_specifier("") == want.with_specifier(""))
                })
                .unwrap_or(0);
            *templates.borrow_mut() = actions;
            action_choice.set_selection(index as u32);
            sync_specifier(select.and_then(|a| a.specifier()));
        })
    };

    // Seed from `initial` when editing, otherwise the first action of the first
    // category.
    {
        let category = initial
            .as_ref()
            .map(|a| a.category())
            .unwrap_or(Category::Streaming);
        let index = Category::ALL
            .iter()
            .position(|c| *c == category)
            .unwrap_or(0);
        category_choice.set_selection(index as u32);
        sync_actions(initial.as_ref());
    }
    if let Some(action) = &initial
        && let Some(bind) = app.config.borrow().keybinds.find(action)
    {
        chord.set(bind.key);
        shortcut_input.set_value(&bind.key.label());
        super::set_accessible_name(&shortcut_input, &format!("Shortcut, {}", bind.key.label()));
        global_check.set_value(bind.global);
    }

    {
        let sync_actions = sync_actions.clone();
        category_choice.clone().on_selection_changed(move |_| {
            sync_actions(None);
        });
    }
    {
        let sync_specifier = sync_specifier.clone();
        action_choice.clone().on_selection_changed(move |_| {
            sync_specifier(None);
        });
    }

    // --- capture --------------------------------------------------------
    //
    // The keys themselves are taken by the app-wide hook (see `ui::keybinds`),
    // which is the only place they can be seen: `::IsDialogMessage` eats ESC,
    // ENTER, the arrows and CTRL+letter during wx's message pre-processing,
    // before any handler on this control could run. The hook parks the chord in
    // an atomic; this polls it from the pump.
    let alive = Rc::new(Cell::new(true));
    {
        shortcut_input.clone().on_set_focus(move |event| {
            super::keybinds::begin_capture();
            event.skip(true);
        });
    }
    {
        let chord = chord.clone();
        let shortcut_input = shortcut_input.clone();
        shortcut_input.clone().on_kill_focus(move |event| {
            super::keybinds::end_capture();
            // The chord is the truth; the field is only its display. Rewriting
            // it on the way out discards anything that reached the control by a
            // route the hook does not cover, such as a paste from its own mouse
            // context menu.
            let label = chord.get().label();
            if shortcut_input.get_value() != label {
                shortcut_input.set_value(&label);
            }
            event.skip(true);
        });
    }
    {
        let shortcut_input = shortcut_input.clone();
        let chord = chord.clone();
        let alive = alive.clone();
        super::run_when_ready(move || {
            if !alive.get() {
                return true;
            }
            let Some(captured) = super::keybinds::take_captured() else {
                return false;
            };
            chord.set(captured);
            let label = captured.label();
            shortcut_input.set_value(&label);
            super::set_accessible_name(&shortcut_input, &format!("Shortcut, {label}"));
            // A `TextCtrl` value change is not reliably spoken, and this is the
            // one moment the user needs to hear what they just pressed.
            super::help::announce(&label);
            false
        });
    }

    // --- confirm --------------------------------------------------------

    let result: Rc<RefCell<Option<BindAction>>> = Rc::new(RefCell::new(None));
    {
        let app = app.clone();
        let dialog_for_ok = dialog.clone();
        let action_choice = action_choice.clone();
        let specifier_choice = specifier_choice.clone();
        let global_check = global_check.clone();
        let templates = templates.clone();
        let chord = chord.clone();
        let result = result.clone();
        let initial = initial.clone();
        ok_button.on_click(move |_| {
            let Some(template) = action_choice
                .get_selection()
                .and_then(|i| templates.borrow().get(i as usize).cloned())
            else {
                show_error(&dialog_for_ok, "Add binding", "Choose an action first.");
                return;
            };
            let action = match template.specifier_kind() {
                Specifier::None => template,
                kind => {
                    let Some(name) = specifier_choice.get_string_selection() else {
                        show_error(
                            &dialog_for_ok,
                            "Add binding",
                            &format!(
                                "Choose which {} this binding applies to. If the list is empty, there are none to choose from yet.",
                                kind.label().to_lowercase()
                            ),
                        );
                        return;
                    };
                    template.with_specifier(&name)
                }
            };
            let chord = chord.get();
            let global = global_check.get_value();

            if !chord.is_empty() {
                if chord.vk == VK_F1 || chord.vk == VK_F6 {
                    show_error(
                        &dialog_for_ok,
                        "Add binding",
                        "F1 and F6 are reserved: F1 speaks the help for whatever has focus, and F6 moves between the lists on a tab. Choose another key.",
                    );
                    return;
                }
                if global && chord.is_plain_character() {
                    show_error(
                        &dialog_for_ok,
                        "Add binding",
                        "A global shortcut needs CTRL, ALT or SHIFT, or a function key. On its own that key would be swallowed in every application you type in.",
                    );
                    return;
                }
                // One chord, one action. Offer to take it rather than refusing.
                let conflict = app
                    .config
                    .borrow()
                    .keybinds
                    .conflict(chord, Some(&action))
                    .cloned();
                if let Some(other) = conflict {
                    let ask = MessageDialog::builder(
                        &dialog_for_ok,
                        &format!(
                            "{} is already bound to \"{}\". Move it to \"{}\"?",
                            chord.label(),
                            other.label(),
                            action.label()
                        ),
                        "Shortcut already in use",
                    )
                    .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
                    .build();
                    if ask.show_modal() != ID_YES {
                        return;
                    }
                    app.config.borrow_mut().keybinds.remove(&other);
                }
            }

            // Editing an action and then picking a different one must not leave
            // the original bound as well.
            if let Some(original) = &initial
                && *original != action
            {
                app.config.borrow_mut().keybinds.remove(original);
            }
            app.config
                .borrow_mut()
                .keybinds
                .set(action.clone(), chord, global);
            app.save_config();
            super::keybinds::reload(&app.config.borrow());
            *result.borrow_mut() = Some(action);
            dialog_for_ok.end_modal(ID_OK);
        });
    }
    {
        let dialog_for_cancel = dialog.clone();
        cancel_button.on_click(move |_| dialog_for_cancel.end_modal(ID_CANCEL));
    }

    dialog.show_modal();
    // Whatever happens, the hook must stop stealing keys and the parked poll
    // must stop touching widgets that are about to be destroyed.
    super::keybinds::end_capture();
    alive.set(false);
    dialog.destroy();
    result.borrow().clone()
}
