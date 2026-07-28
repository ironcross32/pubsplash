//! Configure Audio Pub dialog: site list, credentials, connect/disconnect.

use super::{App, show_error};
use crate::config::{MAIN_SITE_URL, SiteConfig};
use crate::net::NetCommand;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown when no sites are configured. See [`super::list`].
const NO_SITES: &str = "No sites";

pub fn show(app: &Rc<App>, frame: &Frame) {
    let dialog = Dialog::builder(frame, "Configure Audio Pub")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(520, 480)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let sites_label = StaticText::builder(&panel).with_label("Sites").build();
    let sites_list = ListBox::builder(&panel).build();
    super::native_acc::install(&sites_list, "Sites");
    super::help::tag(
        &sites_list,
        "dialog.connect.siteList",
        "Configured Audio Pub sites list",
    );
    let site_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let add_site = Button::builder(&panel).with_label("&Add site").build();
    let remove_site = Button::builder(&panel).with_label("&Remove site").build();
    super::help::tag(&add_site, "dialog.connect.addSite", "Add site button");
    super::help::tag(
        &remove_site,
        "dialog.connect.removeSite",
        "Remove site button",
    );
    site_buttons.add(&add_site, 0, SizerFlag::All, 4);
    site_buttons.add(&remove_site, 0, SizerFlag::All, 4);

    let email_label = StaticText::builder(&panel).with_label("Email").build();
    let email_input = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&email_input, "Email");
    super::help::tag(
        &email_input,
        "dialog.connect.email",
        "Email address for the selected site",
    );
    let password_label = StaticText::builder(&panel).with_label("Password").build();
    let password_input = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::Password)
        .build();
    super::set_accessible_name(&password_input, "Password");
    super::help::tag(
        &password_input,
        "dialog.connect.password",
        "Password for the selected site",
    );

    let connect_button = Button::builder(&panel).with_label("&Connect").build();
    super::help::tag(
        &connect_button,
        "dialog.connect.connectButton",
        "Connect or disconnect button",
    );
    // `ID_CANCEL` is what wx maps Escape to, and it *emulates a click*, so
    // Escape saves the fields just like pressing Close does.
    let close_button = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("C&lose")
        .build();

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
            let sites = &config.connection.sites;
            let labels: Vec<String> = sites
                .iter()
                .map(|site| {
                    let connected =
                        app.run.borrow().connected_site.as_deref() == Some(site.url.as_str());
                    let mut label = site.url.clone();
                    if site.url == MAIN_SITE_URL {
                        label.push_str(" (main)");
                    }
                    if connected {
                        label.push_str(" (connected)");
                    }
                    label
                })
                .collect();
            super::list::fill(&sites_list, &labels, NO_SITES);
            if !sites.is_empty() {
                let select_index = sites
                    .iter()
                    .position(|site| Some(site.url.as_str()) == select_url)
                    .unwrap_or(0);
                sites_list.set_selection(select_index as u32, true);
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
            let Some(index) = super::list::selection(&sites_list, config.connection.sites.len())
            else {
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
        sites_list
            .clone()
            .on_selection_changed(move |_| load_credentials());
    }

    // Save typed credentials into the selected site as the user types is
    // fiddly; instead persist on Connect and on Close via this helper.
    let save_fields = {
        let app = app.clone();
        let sites_list = sites_list.clone();
        let email_input = email_input.clone();
        let password_input = password_input.clone();
        move || {
            let count = app.config.borrow().connection.sites.len();
            let Some(index) = super::list::selection(&sites_list, count) else {
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
                        show_error(
                            &dialog_for_add,
                            "Add site",
                            "Enter a full URL starting with http(s)://",
                        );
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
            let count = app.config.borrow().connection.sites.len();
            let Some(index) = super::list::selection(&sites_list_for_remove, count) else {
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
