//! The Mastodon tab of Preferences: link an account, choose when Pubsplash
//! announces a stream, and keep a library of templates.
//!
//! Three `group_box` groups, and **every control is parented on the group's
//! `StaticBox`, not on the tab panel** — that is what makes a screen reader
//! announce "Account", "Announcements" or "Templates" as focus enters, and wx
//! asserts once per control if it is done the other way.
//!
//! No `&` mnemonics anywhere in here. Mnemonics are dialog-wide — wx's
//! `::IsDialogMessage` searches the whole Preferences dialog, including the tabs
//! that are not showing — and the letters this tab would naturally want (A for
//! Add, E for Edit, R for Remove) are already spoken for by the VST and Keybinds
//! tabs. The buttons are all Tab-reachable, which is what matters.

use super::{App, WXK_DELETE, show_error};
use crate::mastodon::{self, Template};
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown when the user has no templates of their own. See [`super::list`].
const NO_TEMPLATES: &str = "No templates";

pub fn build_tab(app: &Rc<App>, dialog: &Dialog, panel: &Panel) {
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    // --- Account --------------------------------------------------------

    let (account_group, account_box) = super::group_box(panel, "Account");

    let server_label = StaticText::builder(&account_box)
        .with_label("Server")
        .build();
    let server_input = TextCtrl::builder(&account_box)
        .with_value(&display_server(&app.config.borrow().mastodon.instance))
        .build();
    super::set_accessible_name(&server_input, "Server");
    super::help::tag(
        &server_input,
        "dialog.preferences.mastodon.server",
        "Mastodon server input",
    );
    account_group.add(&server_label, 0, SizerFlag::All, 4);
    account_group.add(&server_input, 0, SizerFlag::Expand | SizerFlag::All, 4);

    let status_label = StaticText::builder(&account_box)
        .with_label("Linked account")
        .build();
    // `MultiLine` as well as `ReadOnly`: wx drops a single-line read-only
    // `TextCtrl` out of the Tab order, and a status the user cannot reach is a
    // status they cannot read.
    let status = TextCtrl::builder(&account_box)
        .with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly)
        .with_size(Size::new(-1, 48))
        .build();
    super::set_accessible_name(&status, "Linked account");
    super::help::tag(
        &status,
        "dialog.preferences.mastodon.status",
        "Linked Mastodon account",
    );
    account_group.add(&status_label, 0, SizerFlag::All, 4);
    account_group.add(&status, 0, SizerFlag::Expand | SizerFlag::All, 4);

    let account_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let authorize = Button::builder(&account_box)
        .with_label("Authorize")
        .build();
    super::help::tag(
        &authorize,
        "dialog.preferences.mastodon.authorize",
        "Authorize with Mastodon button",
    );
    let unlink = Button::builder(&account_box).with_label("Unlink").build();
    super::help::tag(
        &unlink,
        "dialog.preferences.mastodon.unlink",
        "Unlink the Mastodon account button",
    );
    account_buttons.add(&authorize, 0, SizerFlag::All, 4);
    account_buttons.add(&unlink, 0, SizerFlag::All, 4);
    account_group.add_sizer(&account_buttons, 0, SizerFlag::Expand, 0);

    // --- Announcements ---------------------------------------------------

    let (announce_group, announce_box) = super::group_box(panel, "Announcements");

    let start_check = CheckBox::builder(&announce_box)
        .with_label("Post to Mastodon when I start a new stream")
        .build();
    super::set_accessible_name(&start_check, "Post to Mastodon when I start a new stream");
    super::help::tag(
        &start_check,
        "dialog.preferences.mastodon.postOnStart",
        "Post to Mastodon when I start a new stream checkbox",
    );
    start_check.set_value(app.config.borrow().mastodon.post_on_start);
    announce_group.add(&start_check, 0, SizerFlag::All, 4);

    let periodic_check = CheckBox::builder(&announce_box)
        .with_label("Make periodic still-streaming Mastodon posts")
        .build();
    super::set_accessible_name(
        &periodic_check,
        "Make periodic still-streaming Mastodon posts",
    );
    super::help::tag(
        &periodic_check,
        "dialog.preferences.mastodon.periodic",
        "Make periodic still-streaming Mastodon posts checkbox",
    );
    periodic_check.set_value(app.config.borrow().mastodon.periodic);
    announce_group.add(&periodic_check, 0, SizerFlag::All, 4);

    // Immediately after the checkbox in the sizer, which is what puts it next in
    // the Tab order — creation order is Tab order.
    let interval_choice = Choice::builder(&announce_box).build();
    super::set_accessible_name(&interval_choice, "How often to post");
    super::help::tag(
        &interval_choice,
        "dialog.preferences.mastodon.interval",
        "Still-streaming post interval choice",
    );
    for (_, label) in mastodon::INTERVALS {
        interval_choice.append(label);
    }
    let stored = app.config.borrow().mastodon.interval_minutes;
    interval_choice.set_selection(
        mastodon::INTERVALS
            .iter()
            .position(|(minutes, _)| *minutes == stored)
            .unwrap_or(0) as u32,
    );
    interval_choice.enable(periodic_check.get_value());
    announce_group.add(&interval_choice, 0, SizerFlag::Expand | SizerFlag::All, 4);

    // --- Templates -------------------------------------------------------

    let (templates_group, templates_box) = super::group_box(panel, "Templates");

    // The list takes its accessible name from this static text, so the label
    // must carry exactly the string `native_acc::install` is given below.
    let templates_label = StaticText::builder(&templates_box)
        .with_label("Templates")
        .build();
    let templates_list = ListBox::builder(&templates_box).build();
    // Never `set_accessible_name` on a list — see `ui/native_acc.rs`, which
    // documents at length why that makes every row announce twice.
    super::native_acc::install(&templates_list, "Templates");
    super::help::tag(
        &templates_list,
        "dialog.preferences.mastodon.templateList",
        "Mastodon announcement templates list",
    );
    templates_group.add(&templates_label, 0, SizerFlag::All, 4);
    templates_group.add(&templates_list, 1, SizerFlag::Expand | SizerFlag::All, 4);

    let template_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let add = Button::builder(&templates_box).with_label("Add").build();
    super::help::tag(
        &add,
        "dialog.preferences.mastodon.addTemplate",
        "Add template button",
    );
    let edit = Button::builder(&templates_box).with_label("Edit").build();
    super::help::tag(
        &edit,
        "dialog.preferences.mastodon.editTemplate",
        "Edit template button",
    );
    let remove = Button::builder(&templates_box).with_label("Remove").build();
    super::help::tag(
        &remove,
        "dialog.preferences.mastodon.removeTemplate",
        "Remove template button",
    );
    template_buttons.add(&add, 0, SizerFlag::All, 4);
    template_buttons.add(&edit, 0, SizerFlag::All, 4);
    template_buttons.add(&remove, 0, SizerFlag::All, 4);
    templates_group.add_sizer(&template_buttons, 0, SizerFlag::Expand, 0);

    sizer.add_sizer(&account_group, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&announce_group, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&templates_group, 1, SizerFlag::Expand | SizerFlag::All, 4);
    panel.set_sizer(sizer, true);

    // --- wiring ----------------------------------------------------------

    // Redraws the account status and the enabled state of everything that needs
    // an account. Called after linking and unlinking, never on a timer.
    let refresh_account: Rc<dyn Fn()> = {
        let app = app.clone();
        Rc::new(move || {
            let config = app.config.borrow();
            let linked = config.mastodon.is_linked();
            let text = if linked {
                format!(
                    "{} on {}",
                    config.mastodon.account,
                    display_server(&config.mastodon.instance)
                )
            } else {
                "Not linked. Enter your server and choose Authorize.".to_string()
            };
            let server = display_server(&config.mastodon.instance);
            drop(config);
            status.set_value(&text);
            // The accessible name has to be re-set, not just the value: it is
            // what a screen reader reads, and it has changed meaning.
            super::set_accessible_name(&status, &format!("Linked account, {text}"));
            if linked && server_input.get_value().trim() != server {
                server_input.set_value(&server);
            }
            unlink.enable(linked);
        })
    };
    refresh_account();

    // The list is rebuilt only on a deliberate edit. `select` names the row to
    // land on afterwards, so adding a template leaves focus on the one just
    // added rather than wherever the old indices happened to point.
    let refresh_templates: super::list::Refresh<Template> = {
        let app = app.clone();
        Rc::new(move |select: Option<&Template>| {
            let templates = app.config.borrow().mastodon.templates.clone();
            let labels: Vec<String> = templates.iter().map(|t| t.list_label()).collect();
            super::list::fill(&templates_list, &labels, NO_TEMPLATES);
            if templates.is_empty() {
                return;
            }
            let index = select
                .and_then(|want| templates.iter().position(|t| t == want))
                .unwrap_or(0);
            templates_list.set_selection(index as u32, true);
        })
    };
    refresh_templates(None);

    {
        let app = app.clone();
        start_check.clone().on_toggled(move |_| {
            app.config.borrow_mut().mastodon.post_on_start = start_check.get_value();
            app.save_config();
        });
    }
    {
        let app = app.clone();
        periodic_check.clone().on_toggled(move |_| {
            let on = periodic_check.get_value();
            app.config.borrow_mut().mastodon.periodic = on;
            app.save_config();
            interval_choice.enable(on);
        });
    }
    {
        let app = app.clone();
        interval_choice.clone().on_selection_changed(move |_| {
            let Some(index) = interval_choice.get_selection() else {
                return;
            };
            let Some((minutes, _)) = mastodon::INTERVALS.get(index as usize) else {
                return;
            };
            app.config.borrow_mut().mastodon.interval_minutes = *minutes;
            app.save_config();
        });
    }

    // The server field is only a staging area for Authorize — it is not saved as
    // it is typed, because a half-typed host would replace the instance of an
    // account that is already linked.

    {
        let app = app.clone();
        let dialog = *dialog;
        let refresh_account = refresh_account.clone();
        authorize.on_click(move |_| {
            let typed = server_input.get_value();
            let instance = match crate::mastodon::api::normalize_instance(&typed) {
                Ok(instance) => instance,
                Err(error) => {
                    show_error(&dialog, "Authorize", &error.to_string());
                    server_input.set_focus();
                    return;
                }
            };
            if let Some(link) = super::mastodon_post::authorize(&dialog, &instance) {
                {
                    let mut config = app.config.borrow_mut();
                    config.mastodon.instance = link.instance;
                    config.mastodon.client_id = link.client_id;
                    config.mastodon.client_secret = link.client_secret;
                    config.mastodon.access_token = link.access_token;
                    config.mastodon.account = link.account.clone();
                }
                app.save_config();
                // Straight to disk rather than on the next pump tick: the user
                // just spent a browser round-trip on this, and a crash before
                // the tick would make them do it again.
                app.flush_config();
                refresh_account();
                super::show_info(
                    &dialog,
                    "Authorize",
                    &format!("Pubsplash is linked to {}.", link.account),
                );
            } else {
                // Whatever went wrong, the address is the thing the user would
                // change; leave the caret where they can retype it. The same
                // courtesy the rejection above already gets.
                server_input.set_focus();
            }
        });
    }

    {
        let app = app.clone();
        let dialog = *dialog;
        let refresh_account = refresh_account.clone();
        unlink.on_click(move |_| {
            let (instance, client_id, secret, token, account) = {
                let config = app.config.borrow();
                (
                    config.mastodon.instance.clone(),
                    config.mastodon.client_id.clone(),
                    config.mastodon.client_secret.clone(),
                    config.mastodon.access_token.clone(),
                    config.mastodon.account.clone(),
                )
            };
            if token.is_empty() {
                return;
            }
            let ask = MessageDialog::builder(
                &dialog,
                &format!(
                    "Remove the authorization for {account}? \
                     Pubsplash will stop posting, and you will have to authorize again to resume."
                ),
                "Unlink",
            )
            .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
            .build();
            let answer = ask.show_modal();
            ask.destroy();
            if answer != ID_YES {
                return;
            }
            // Local first, and unconditionally: whatever the server says, the
            // user asked to be unlinked and the token must not survive it.
            app.config.borrow_mut().mastodon.unlink();
            app.save_config();
            app.flush_config();
            crate::mastodon::net::revoke_in_background(instance, client_id, secret, token);
            refresh_account();
        });
    }

    {
        let app = app.clone();
        let dialog = *dialog;
        let refresh_templates = refresh_templates.clone();
        add.on_click(move |_| {
            let Some(template) = super::mastodon_templates::edit(&dialog, None) else {
                return;
            };
            let mut templates = app.config.borrow().mastodon.templates.clone();
            templates.push(template.clone());
            super::mastodon_templates::store(&app, templates);
            refresh_templates(Some(&template));
        });
    }

    {
        let app = app.clone();
        let dialog = *dialog;
        let refresh_templates = refresh_templates.clone();
        edit.on_click(move |_| {
            let existing = app.config.borrow().mastodon.templates.clone();
            // `list::selection` is the only way a row index becomes a data
            // index; it is what keeps the "No templates" placeholder from
            // indexing past the end.
            let Some(index) = super::list::selection(&templates_list, existing.len()) else {
                return;
            };
            let Some(template) = super::mastodon_templates::edit(&dialog, Some(&existing[index]))
            else {
                return;
            };
            let mut templates = existing;
            templates[index] = template.clone();
            super::mastodon_templates::store(&app, templates);
            refresh_templates(Some(&template));
        });
    }

    let do_remove = {
        let app = app.clone();
        let refresh_templates = refresh_templates.clone();
        Rc::new(move || {
            let mut templates = app.config.borrow().mastodon.templates.clone();
            let Some(index) = super::list::selection(&templates_list, templates.len()) else {
                return;
            };
            templates.remove(index);
            super::mastodon_templates::store(&app, templates);
            refresh_templates(None);
        })
    };
    {
        let do_remove = do_remove.clone();
        remove.on_click(move |_| do_remove());
    }
    templates_list
        .clone()
        .on_key_down(move |event| match super::key_of(&event) {
            Some((WXK_DELETE, _)) => do_remove(),
            _ => event.skip(true),
        });
}

/// The host, as the user typed it and as they want to read it back.
fn display_server(instance: &str) -> String {
    instance
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_is_shown_without_its_scheme() {
        assert_eq!(display_server("https://mastodon.social"), "mastodon.social");
        assert_eq!(
            display_server("https://mastodon.social/"),
            "mastodon.social"
        );
        assert_eq!(display_server(""), "");
    }

    /// The interval dropdown is built from this table, and a stored value that
    /// is not in it would leave the dropdown with nothing selected.
    #[test]
    fn every_stored_interval_has_a_row() {
        for (minutes, label) in mastodon::INTERVALS {
            assert!(!label.is_empty());
            assert_eq!(mastodon::clamp_interval(*minutes), *minutes);
        }
        assert_eq!(mastodon::INTERVALS.len(), 6);
    }
}
