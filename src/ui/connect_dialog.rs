//! Configure Audio Pub dialog: site list, credentials, connect/disconnect.

use super::{App, show_error};
use crate::config::{MAIN_SITE_URL, SiteConfig};
use crate::net::NetCommand;
use std::rc::Rc;
use wxdragon::prelude::*;

pub fn show(app: &Rc<App>, frame: &Frame) {
    let dialog = Dialog::builder(frame, "Configure Audio Pub")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(520, 480)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let sites_label = StaticText::builder(&panel).with_label("Sites").build();
    let sites_list = ListBox::builder(&panel).build();
    super::set_accessible_name(&sites_list, "Sites");
    let site_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let add_site = Button::builder(&panel).with_label("&Add site").build();
    let remove_site = Button::builder(&panel).with_label("&Remove site").build();
    site_buttons.add(&add_site, 0, SizerFlag::All, 4);
    site_buttons.add(&remove_site, 0, SizerFlag::All, 4);

    let email_label = StaticText::builder(&panel).with_label("Email").build();
    let email_input = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&email_input, "Email");
    let password_label = StaticText::builder(&panel).with_label("Password").build();
    let password_input = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::Password)
        .build();
    super::set_accessible_name(&password_input, "Password");

    let connect_button = Button::builder(&panel).with_label("&Connect").build();
    let close_button = Button::builder(&panel).with_label("C&lose").build();

    sizer.add(&sites_label, 0, SizerFlag::All, 4);
    sizer.add(&sites_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&site_buttons, 0, SizerFlag::Expand, 0);
    sizer.add(&email_label, 0, SizerFlag::All, 4);
    sizer.add(&email_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&password_label, 0, SizerFlag::All, 4);
    sizer.add(&password_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&connect_button, 0, SizerFlag::All, 8);
    sizer.add(&close_button, 0, SizerFlag::All, 8);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    let refresh_sites = {
        let app = app.clone();
        let sites_list = sites_list.clone();
        move |select_url: Option<&str>| {
            let config = app.config.borrow();
            sites_list.clear();
            let mut select_index = 0u32;
            for (i, site) in config.connection.sites.iter().enumerate() {
                let connected = app.run.borrow().connected_site.as_deref() == Some(site.url.as_str());
                let mut label = site.url.clone();
                if site.url == MAIN_SITE_URL {
                    label.push_str(" (main)");
                }
                if connected {
                    label.push_str(" (connected)");
                }
                sites_list.append(&label);
                if Some(site.url.as_str()) == select_url {
                    select_index = i as u32;
                }
            }
            if sites_list.get_count() > 0 {
                sites_list.set_selection(select_index, true);
            }
        }
    };

    let load_credentials = {
        let app = app.clone();
        let sites_list = sites_list.clone();
        let email_input = email_input.clone();
        let password_input = password_input.clone();
        move || {
            let config = app.config.borrow();
            let Some(index) = sites_list.get_selection().map(|i| i as usize) else {
                return;
            };
            if let Some(site) = config.connection.sites.get(index) {
                email_input.set_value(&site.email);
                password_input.set_value(&site.password);
            }
        }
    };

    let update_connect_label = {
        let app = app.clone();
        let connect_button = connect_button.clone();
        move || {
            // While any connection exists the button reads Disconnect, even
            // if a different site is highlighted.
            let connected = app.run.borrow().connected_site.is_some();
            connect_button.set_label(if connected { "Dis&connect" } else { "&Connect" });
        }
    };

    let initial_site = {
        let config = app.config.borrow();
        config
            .connection
            .last_used_site
            .clone()
            .unwrap_or_else(|| MAIN_SITE_URL.to_string())
    };
    refresh_sites(Some(&initial_site));
    load_credentials();
    update_connect_label();

    // Selection change loads that site's credentials.
    {
        let load_credentials = load_credentials.clone();
        sites_list.clone().on_selection_changed(move |_| load_credentials());
    }

    // Save typed credentials into the selected site as the user types is
    // fiddly; instead persist on Connect and on Close via this helper.
    let save_fields = {
        let app = app.clone();
        let sites_list = sites_list.clone();
        let email_input = email_input.clone();
        let password_input = password_input.clone();
        move || {
            let Some(index) = sites_list.get_selection().map(|i| i as usize) else {
                return None;
            };
            let mut config = app.config.borrow_mut();
            let Some(site) = config.connection.sites.get_mut(index) else {
                return None;
            };
            site.email = email_input.get_value().trim().to_string();
            site.password = password_input.get_value();
            let url = site.url.clone();
            drop(config);
            app.save_config();
            Some(url)
        }
    };

    {
        let app = app.clone();
        let dialog_for_add = dialog.clone();
        let refresh_sites = refresh_sites.clone();
        add_site.on_click(move |_| {
            let entry = TextEntryDialog::builder(
                &dialog_for_add,
                "URL of the Audio Pub instance (https://...):",
                "Add site",
            )
            .build();
            if entry.show_modal() == ID_OK {
                if let Some(url) = entry.get_value() {
                    let url = url.trim().to_string();
                    if url.is_empty() || !url.starts_with("http") {
                        show_error(&dialog_for_add, "Add site", "Enter a full URL starting with http(s)://");
                        return;
                    }
                    let mut config = app.config.borrow_mut();
                    if config.connection.site(&url).is_none() {
                        config.connection.sites.push(SiteConfig {
                            url: url.clone(),
                            ..Default::default()
                        });
                    }
                    drop(config);
                    app.save_config();
                    refresh_sites(Some(&url));
                }
            }
        });
    }

    {
        let app = app.clone();
        let sites_list_for_remove = sites_list.clone();
        let dialog_for_remove = dialog.clone();
        let refresh_sites = refresh_sites.clone();
        let load_credentials = load_credentials.clone();
        remove_site.on_click(move |_| {
            let Some(index) = sites_list_for_remove.get_selection().map(|i| i as usize) else {
                return;
            };
            {
                let mut config = app.config.borrow_mut();
                let Some(site) = config.connection.sites.get(index) else {
                    return;
                };
                if site.url == MAIN_SITE_URL {
                    drop(config);
                    show_error(
                        &dialog_for_remove,
                        "Remove site",
                        "The main Audio Pub site cannot be removed.",
                    );
                    return;
                }
                config.connection.sites.remove(index);
            }
            app.save_config();
            refresh_sites(None);
            load_credentials();
        });
    }

    {
        let app = app.clone();
        let update_connect_label = update_connect_label.clone();
        let save_fields = save_fields.clone();
        let dialog_for_connect = dialog.clone();
        connect_button.on_click(move |_| {
            let connected = app.run.borrow().connected_site.is_some();
            if connected {
                app.net.send(NetCommand::Disconnect);
                app.run.borrow_mut().connected_site = None;
                update_connect_label();
                return;
            }
            let Some(url) = save_fields() else { return };
            let (email, password) = {
                let config = app.config.borrow();
                let Some(site) = config.connection.site(&url) else {
                    return;
                };
                (site.email.clone(), site.password.clone())
            };
            if email.is_empty() || password.is_empty() {
                show_error(
                    &dialog_for_connect,
                    "Connect",
                    "Enter your email and password first.",
                );
                return;
            }
            app.run.borrow_mut().connecting = true;
            app.net.send(NetCommand::Connect {
                site_url: url,
                email,
                password,
            });
            // Result arrives via the pump; the dialog may already be closed
            // by then, which is fine - the main window reports the outcome.
        });
    }

    {
        let dialog_for_close = dialog.clone();
        let save_fields = save_fields.clone();
        close_button.on_click(move |_| {
            let _ = save_fields();
            dialog_for_close.end_modal(ID_CANCEL);
        });
    }

    // Register so connection results arriving on the pump report inside
    // this dialog and return focus to the connect button.
    *app.connect_ui.borrow_mut() = Some(super::ConnectUi {
        dialog: dialog.clone(),
        connect_button: connect_button.clone(),
    });
    dialog.show_modal();
    *app.connect_ui.borrow_mut() = None;
    dialog.destroy();
}
