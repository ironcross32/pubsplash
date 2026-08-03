//! The add/edit dialog for a Mastodon announcement template, its Help window,
//! and the one-shot dialog used when a stream resumes.
//!
//! Validation runs **inside the OK handler and returns without closing** on a
//! bad template, which is the whole point: the user gets the warning, dismisses
//! it, and is back in the same dialog with their text and their caret where they
//! left them, rather than facing an empty box. That only works because
//! `super::ok_button` uses `ID_CONFIRM` rather than `ID_OK` — under `ID_OK`,
//! `wxDialogBase`'s own handler would close the dialog behind us. See the doc
//! comment on `ui::ok_button`.

use super::App;
use crate::mastodon::{self, Template, TemplateKind};
use std::cell::RefCell;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Pre-filled into the one-shot dialog when a stream resumes.
pub const RESUME_DEFAULT: &str = "I've resumed my #Audiopub stream! Tune in at {url}";

/// Shows the add/edit dialog. `existing` is `None` for Add.
pub fn edit(parent: &dyn WxWidget, existing: Option<&Template>) -> Option<Template> {
    let adding = existing.is_none();
    let caption = if adding {
        "Add template"
    } else {
        "Edit template"
    };
    let dialog = Dialog::builder(parent, caption)
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(520, 340)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // Built from `TemplateKind::ALL` so the item order and the index-to-kind
    // mapping below cannot drift apart.
    let kind_labels: Vec<&str> = TemplateKind::ALL.iter().map(|k| k.label()).collect();
    let kind_choice = RadioBox::builder(&panel, &kind_labels)
        .with_label("Announcement type")
        .with_style(RadioBoxStyle::SpecifyRows)
        .with_major_dimension(1)
        .build();
    let current_kind = existing.map(|t| t.kind).unwrap_or_default();
    kind_choice.set_selection(
        TemplateKind::ALL
            .iter()
            .position(|k| *k == current_kind)
            .unwrap_or(0) as i32,
    );
    // A radio box's items are real child windows, so `set_accessible_name` can
    // only reach the group. `install_radio_box` hands the whole group back to
    // OLEACC, which names each button from its own text.
    super::native_acc::install_radio_box(&kind_choice, "Announcement type");
    super::help::tag(
        &kind_choice,
        "dialog.mastodonTemplate.kind",
        "Announcement type selector",
    );

    let text_label = StaticText::builder(&panel).with_label("Template").build();
    let text_input = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::MultiLine)
        .with_value(existing.map(|t| t.text.as_str()).unwrap_or(""))
        .build();
    super::set_accessible_name(&text_input, "Template");
    super::help::tag(
        &text_input,
        "dialog.mastodonTemplate.text",
        "Template text input",
    );

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let help_button = Button::builder(&panel).with_label("Help").build();
    super::help::tag(
        &help_button,
        "dialog.mastodonTemplate.help",
        "Token help button",
    );
    let ok = super::ok_button(&panel, "OK");
    // `ID_CANCEL` is what wx maps Escape to; without it Escape does nothing.
    let cancel = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("Cancel")
        .build();
    buttons.add(&help_button, 0, SizerFlag::All, 4);
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);

    sizer.add(&kind_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&text_label, 0, SizerFlag::All, 4);
    sizer.add(&text_input, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    {
        let dialog = dialog.clone();
        help_button.on_click(move |_| show_help(&dialog));
    }

    let result: Rc<RefCell<Option<Template>>> = Rc::new(RefCell::new(None));
    {
        let dialog = dialog.clone();
        let kind_choice = kind_choice.clone();
        let text_input = text_input.clone();
        let result = result.clone();
        ok.on_click(move |_| {
            let text = text_input.get_value().trim().to_string();
            if let Err(error) = mastodon::validate(&text) {
                // No `end_modal`: the dialog stays open behind the warning with
                // the user's text intact, so a typo is one correction away
                // rather than a retype.
                super::show_warning(&dialog, caption, &error.to_string());
                text_input.set_focus();
                return;
            }
            let kind = TemplateKind::ALL
                .get(kind_choice.get_selection().max(0) as usize)
                .copied()
                .unwrap_or_default();
            *result.borrow_mut() = Some(Template::new(kind, text));
            dialog.end_modal(ID_OK);
        });
    }
    {
        let dialog = dialog.clone();
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    dialog.show_modal();
    dialog.destroy();
    let template = result.borrow().clone();
    template
}

/// The token reference. Read-only but selectable, so the user can copy a token
/// straight out of it.
pub fn show_help(parent: &dyn WxWidget) {
    let dialog = Dialog::builder(parent, "Template tokens")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(560, 420)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // `MultiLine` as well as `ReadOnly`: wx drops a *single-line* read-only
    // `TextCtrl` out of the Tab order entirely, and this one has to be
    // reachable to be readable. Multi-line also keeps ENTER inside the field
    // instead of firing the default button, so arrowing and selecting behave
    // the way they do in any other text.
    let text = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .with_value(&mastodon::help_text())
        .build();
    super::set_accessible_name(&text, "Template tokens");
    super::help::tag(
        &text,
        "dialog.mastodonTokens.text",
        "Template token reference",
    );

    let close = super::dismiss_button(&panel, "Close");
    {
        let dialog = dialog.clone();
        close.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    sizer.add(&text, 1, SizerFlag::Expand | SizerFlag::All, 8);
    sizer.add(&close, 0, SizerFlag::All, 8);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    dialog.show_modal();
    dialog.destroy();
}

/// The one-shot template asked for when a stream resumes.
///
/// Pre-filled and pre-selected so the suggested wording can be replaced by just
/// typing, the way the Set stream info fields work.
pub fn prompt_one_shot(parent: &dyn WxWidget) -> Option<String> {
    let dialog = Dialog::builder(parent, "Post about the resumed stream")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(520, 300)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let label = StaticText::builder(&panel).with_label("Post").build();
    let input = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::MultiLine)
        .with_value(RESUME_DEFAULT)
        .build();
    super::set_accessible_name(&input, "Post");
    super::help::tag(&input, "dialog.mastodonResume.text", "Resumed stream post");
    input.select_all();
    {
        let input = input.clone();
        input.clone().on_set_focus(move |event| {
            input.select_all();
            event.skip(true);
        });
    }

    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let help_button = Button::builder(&panel).with_label("Help").build();
    super::help::tag(
        &help_button,
        "dialog.mastodonResume.help",
        "Token help button on the resumed stream post",
    );
    let ok = super::ok_button(&panel, "OK");
    let cancel = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("Cancel")
        .build();
    buttons.add(&help_button, 0, SizerFlag::All, 4);
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);

    sizer.add(&label, 0, SizerFlag::All, 4);
    sizer.add(&input, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::All, 4);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    {
        let dialog = dialog.clone();
        help_button.on_click(move |_| show_help(&dialog));
    }

    let result: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    {
        let dialog = dialog.clone();
        let input = input.clone();
        let result = result.clone();
        ok.on_click(move |_| {
            let text = input.get_value().trim().to_string();
            // Same rule as a saved template: a bad token is corrected here, not
            // posted as literal braces.
            if let Err(error) = mastodon::validate(&text) {
                super::show_warning(&dialog, "Post about the resumed stream", &error.to_string());
                input.set_focus();
                return;
            }
            *result.borrow_mut() = Some(text);
            dialog.end_modal(ID_OK);
        });
    }
    {
        let dialog = dialog.clone();
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }

    dialog.show_modal();
    dialog.destroy();
    let text = result.borrow().clone();
    text
}

/// Saves an edited template list: sorts it the way the list shows it, writes it
/// and flushes. Sorting happens here — on an edit — and never on a timer, so a
/// list a screen reader is sitting in is only ever rebuilt deliberately.
pub fn store(app: &Rc<App>, templates: Vec<Template>) {
    let mut templates = templates;
    mastodon::sort(&mut templates);
    app.config.borrow_mut().mastodon.templates = templates;
    app.save_config();
}

#[cfg(test)]
mod tests {
    use crate::mastodon;

    #[test]
    fn the_resume_suggestion_is_a_valid_template() {
        assert_eq!(mastodon::validate(super::RESUME_DEFAULT), Ok(()));
        assert!(super::RESUME_DEFAULT.contains("{url}"));
    }
}
