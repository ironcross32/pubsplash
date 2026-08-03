//! Setup streaming services dialog: service list, credentials, connect/disconnect.

use super::{App, show_error};
use crate::config::{MAIN_SITE_URL, SiteConfig, StreamingServiceType};
use crate::net::NetCommand;
use crate::secret::Secret;
use std::rc::Rc;
use wxdragon::prelude::*;

/// Shown when no services are configured. See [`super::list`].
const NO_SERVICES: &str = "No services";

pub fn show(app: &Rc<App>, frame: &Frame) {
    let dialog = Dialog::builder(frame, "Setup streaming services")
        .with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
        .with_size(560, 640)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();

    let services_label = StaticText::builder(&panel)
        .with_label("Streaming services")
        .build();
    let services_list = ListBox::builder(&panel).build();
    super::native_acc::install(&services_list, "Streaming services");
    super::help::tag(
        &services_list,
        "dialog.connect.siteList",
        "Configured streaming services list",
    );
    let service_buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let add_service = Button::builder(&panel).with_label("&Add service").build();
    let rename_service = Button::builder(&panel)
        .with_label("Rena&me service")
        .build();
    let remove_service = Button::builder(&panel)
        .with_label("&Remove service")
        .build();
    super::help::tag(&add_service, "dialog.connect.addSite", "Add service button");
    super::help::tag(
        &rename_service,
        "dialog.connect.renameSite",
        "Rename service button",
    );
    super::help::tag(
        &remove_service,
        "dialog.connect.removeSite",
        "Remove service button",
    );
    service_buttons.add(&add_service, 0, SizerFlag::All, 4);
    service_buttons.add(&rename_service, 0, SizerFlag::All, 4);
    service_buttons.add(&remove_service, 0, SizerFlag::All, 4);

    let service_type = RadioBox::builder(&panel, &["Audiopub", "Icecast"])
        .with_label("Service type")
        .with_style(RadioBoxStyle::SpecifyRows)
        .with_major_dimension(1)
        .build();
    super::native_acc::install_radio_box(&service_type, "Service type");
    super::help::tag(
        &service_type,
        "dialog.connect.serviceType",
        "Service type selector",
    );

    let url_label = StaticText::builder(&panel)
        .with_label("Audiopub URL")
        .build();
    let url_input = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&url_input, "Audiopub URL");
    super::help::tag(
        &url_input,
        "dialog.connect.url",
        "Audiopub URL for the selected service",
    );
    let email_label = StaticText::builder(&panel).with_label("Email").build();
    let email_input = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&email_input, "Email");
    super::help::tag(
        &email_input,
        "dialog.connect.email",
        "Email address for the selected service",
    );
    let password_label = StaticText::builder(&panel).with_label("Password").build();
    let password_input = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::Password)
        .build();
    super::set_accessible_name(&password_input, "Password");
    super::help::tag(
        &password_input,
        "dialog.connect.password",
        "Password for the selected service",
    );

    let server_label = StaticText::builder(&panel)
        .with_label("Icecast server")
        .build();
    let server_input = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&server_input, "Icecast server");
    super::help::tag(
        &server_input,
        "dialog.connect.icecastServer",
        "Icecast server for the selected service",
    );
    let port_label = StaticText::builder(&panel)
        .with_label("Icecast port")
        .build();
    let port_input = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&port_input, "Icecast port");
    super::help::tag(
        &port_input,
        "dialog.connect.icecastPort",
        "Icecast port for the selected service",
    );
    let mount_label = StaticText::builder(&panel)
        .with_label("Icecast mount point")
        .build();
    let mount_input = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&mount_input, "Icecast mount point");
    super::help::tag(
        &mount_input,
        "dialog.connect.icecastMount",
        "Icecast mount point for the selected service",
    );
    let username_label = StaticText::builder(&panel)
        .with_label("Icecast username")
        .build();
    let username_input = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&username_input, "Icecast username");
    super::help::tag(
        &username_input,
        "dialog.connect.icecastUsername",
        "Icecast username for the selected service",
    );
    let icecast_password_label = StaticText::builder(&panel)
        .with_label("Icecast password")
        .build();
    let icecast_password_input = TextCtrl::builder(&panel)
        .with_style(TextCtrlStyle::Password)
        .build();
    super::set_accessible_name(&icecast_password_input, "Icecast password");
    super::help::tag(
        &icecast_password_input,
        "dialog.connect.icecastPassword",
        "Icecast password for the selected service",
    );

    let connect_button = Button::builder(&panel).with_label("&Connect").build();
    super::help::tag(
        &connect_button,
        "dialog.connect.connectButton",
        "Connect or disconnect button",
    );
    // `ID_CANCEL` is what wx maps Escape to, and it emulates a click, so
    // Escape saves the fields just like pressing Close does. `dismiss_button`
    // makes it the default item too, so Enter closes and saves as well.
    //
    // Connect deliberately stays off the default item: it starts or stops a live
    // connection, which is not something a stray Enter in the service list or a
    // password field should ever do.
    let close_button = super::dismiss_button(&panel, "C&lose");

    sizer.add(&services_label, 0, SizerFlag::All, 4);
    sizer.add(&services_list, 1, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&service_buttons, 0, SizerFlag::Expand, 0);
    sizer.add(&service_type, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&url_label, 0, SizerFlag::All, 4);
    sizer.add(&url_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&email_label, 0, SizerFlag::All, 4);
    sizer.add(&email_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&password_label, 0, SizerFlag::All, 4);
    sizer.add(&password_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&server_label, 0, SizerFlag::All, 4);
    sizer.add(&server_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&port_label, 0, SizerFlag::All, 4);
    sizer.add(&port_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&mount_label, 0, SizerFlag::All, 4);
    sizer.add(&mount_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&username_label, 0, SizerFlag::All, 4);
    sizer.add(&username_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&icecast_password_label, 0, SizerFlag::All, 4);
    sizer.add(
        &icecast_password_input,
        0,
        SizerFlag::Expand | SizerFlag::All,
        4,
    );
    sizer.add(&connect_button, 0, SizerFlag::All, 8);
    sizer.add(&close_button, 0, SizerFlag::All, 8);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    let update_field_visibility = {
        move || {
            let audiopub = service_type.get_selection() != 1;
            url_label.show(audiopub);
            url_input.show(audiopub);
            email_label.show(audiopub);
            email_input.show(audiopub);
            password_label.show(audiopub);
            password_input.show(audiopub);
            server_label.show(!audiopub);
            server_input.show(!audiopub);
            port_label.show(!audiopub);
            port_input.show(!audiopub);
            mount_label.show(!audiopub);
            mount_input.show(!audiopub);
            username_label.show(!audiopub);
            username_input.show(!audiopub);
            icecast_password_label.show(!audiopub);
            icecast_password_input.show(!audiopub);
            panel.layout();
        }
    };

    let refresh_services = {
        let app = app.clone();
        move |select_id: Option<&str>| {
            let config = app.config.borrow();
            let services = &config.connection.sites;
            let labels: Vec<String> = services
                .iter()
                .map(|service| {
                    let connected =
                        app.run.borrow().connected_service.as_deref() == Some(service.id.as_str());
                    let mut label = match service.service_type {
                        StreamingServiceType::Audiopub => {
                            format!("{} (Audiopub)", service.display_name())
                        }
                        StreamingServiceType::Icecast => {
                            format!("{} (Icecast)", service.display_name())
                        }
                    };
                    if service.is_main() {
                        label.push_str(" (main)");
                    }
                    if connected {
                        label.push_str(" (connected)");
                    }
                    label
                })
                .collect();
            super::list::fill(&services_list, &labels, NO_SERVICES);
            if !services.is_empty() {
                let select_index = services
                    .iter()
                    .position(|service| {
                        Some(service.id.as_str()) == select_id
                            || Some(service.url.as_str()) == select_id
                    })
                    .unwrap_or(0);
                services_list.set_selection(select_index as u32, true);
            }
        }
    };

    let update_actions = {
        let app = app.clone();
        move || {
            let config = app.config.borrow();
            let enabled = super::list::selection(&services_list, config.connection.sites.len())
                .and_then(|index| config.connection.sites.get(index))
                .map(|service| !service.is_main())
                .unwrap_or(false);
            rename_service.enable(enabled);
            remove_service.enable(enabled);
        }
    };

    let clear_fields = {
        move || {
            url_input.set_value("");
            email_input.set_value("");
            password_input.set_value("");
            server_input.set_value("");
            port_input.set_value("");
            mount_input.set_value("");
            username_input.set_value("");
            icecast_password_input.set_value("");
        }
    };

    let load_fields = {
        let app = app.clone();
        let update_actions = update_actions.clone();
        move || {
            let config = app.config.borrow();
            let Some(index) = super::list::selection(&services_list, config.connection.sites.len())
            else {
                clear_fields();
                update_actions();
                return;
            };
            if let Some(service) = config.connection.sites.get(index) {
                service_type.set_selection(match service.service_type {
                    StreamingServiceType::Audiopub => 0,
                    StreamingServiceType::Icecast => 1,
                });
                service_type.enable(!service.is_main());
                url_input.set_value(&service.url);
                email_input.set_value(&service.email);
                password_input.set_value(service.password.as_str());
                server_input.set_value(&service.icecast_server);
                port_input.set_value(&service.icecast_port.to_string());
                mount_input.set_value(&service.icecast_mount);
                username_input.set_value(&service.icecast_username);
                icecast_password_input.set_value(service.icecast_password.as_str());
            }
            drop(config);
            update_field_visibility();
            update_actions();
        }
    };

    let update_connect_label = {
        let app = app.clone();
        move || {
            // While any connection exists the button reads Disconnect, even
            // if a different service is highlighted.
            let connected = app.run.borrow().connected_service.is_some();
            connect_button.set_label(if connected { "Dis&connect" } else { "&Connect" });
        }
    };

    let initial_service = {
        let config = app.config.borrow();
        config
            .connection
            .last_used_site
            .clone()
            .unwrap_or_else(|| MAIN_SITE_URL.to_string())
    };
    refresh_services(Some(&initial_service));
    load_fields();
    update_connect_label();

    {
        let load_fields = load_fields.clone();
        services_list
            .clone()
            .on_selection_changed(move |_| load_fields());
    }

    {
        service_type
            .clone()
            .on_selected(move |_| update_field_visibility());
    }

    // Save typed fields into the selected service on Connect and Close.
    let save_fields = {
        let app = app.clone();
        move || {
            let count = app.config.borrow().connection.sites.len();
            let index = super::list::selection(&services_list, count)?;
            let mut config = app.config.borrow_mut();
            let service = config.connection.sites.get_mut(index)?;
            if !service.is_main() {
                service.service_type = if service_type.get_selection() == 1 {
                    StreamingServiceType::Icecast
                } else {
                    StreamingServiceType::Audiopub
                };
            } else {
                service.service_type = StreamingServiceType::Audiopub;
                service.nickname = "Audiopub".to_string();
                service.url = MAIN_SITE_URL.to_string();
            }
            service.url = url_input.get_value().trim().to_string();
            if service.is_main() {
                service.url = MAIN_SITE_URL.to_string();
            }
            service.email = email_input.get_value().trim().to_string();
            service.password = Secret::new(password_input.get_value());
            service.icecast_server = server_input.get_value().trim().to_string();
            service.icecast_port = port_input.get_value().trim().parse().unwrap_or(0);
            service.icecast_mount = mount_input.get_value().trim().to_string();
            service.icecast_username = username_input.get_value().trim().to_string();
            service.icecast_password = Secret::new(icecast_password_input.get_value());
            let id = service.id.clone();
            drop(config);
            app.save_config();
            Some(id)
        }
    };

    {
        let app = app.clone();
        let dialog_for_add = dialog;
        let refresh_services = refresh_services.clone();
        let load_fields = load_fields.clone();
        add_service.on_click(move |_| {
            let Some((nickname, service_type)) = prompt_new_service(&dialog_for_add) else {
                return;
            };
            let id = {
                let mut config = app.config.borrow_mut();
                let id = config.connection.next_service_id();
                let service = match service_type {
                    StreamingServiceType::Audiopub => SiteConfig::audiopub(id.clone(), nickname),
                    StreamingServiceType::Icecast => SiteConfig::icecast(id.clone(), nickname),
                };
                config.connection.sites.push(service);
                id
            };
            app.save_config();
            refresh_services(Some(&id));
            load_fields();
        });
    }

    {
        let app = app.clone();
        let services_list_for_rename = services_list;
        let dialog_for_rename = dialog;
        let refresh_services = refresh_services.clone();
        rename_service.on_click(move |_| {
            let count = app.config.borrow().connection.sites.len();
            let Some(index) = super::list::selection(&services_list_for_rename, count) else {
                return;
            };
            let (id, current_name) = {
                let config = app.config.borrow();
                let Some(service) = config.connection.sites.get(index) else {
                    return;
                };
                if service.is_main() {
                    show_error(
                        &dialog_for_rename,
                        "Rename service",
                        "The main Audiopub service cannot be renamed.",
                    );
                    return;
                }
                (service.id.clone(), service.display_name())
            };
            let entry = TextEntryDialog::builder(
                &dialog_for_rename,
                "New service nickname:",
                "Rename service",
            )
            .with_default_value(&current_name)
            .build();
            if entry.show_modal() == ID_OK
                && let Some(name) = entry.get_value()
            {
                let name = name.trim();
                if name.is_empty() {
                    show_error(&dialog_for_rename, "Rename service", "Enter a nickname.");
                    return;
                }
                if let Some(service) = app.config.borrow_mut().connection.site_mut(&id) {
                    service.nickname = name.to_string();
                }
                app.save_config();
                refresh_services(Some(&id));
            }
        });
    }

    {
        let app = app.clone();
        let services_list_for_remove = services_list;
        let dialog_for_remove = dialog;
        let refresh_services = refresh_services.clone();
        let load_fields = load_fields.clone();
        remove_service.on_click(move |_| {
            let count = app.config.borrow().connection.sites.len();
            let Some(index) = super::list::selection(&services_list_for_remove, count) else {
                return;
            };
            {
                let mut config = app.config.borrow_mut();
                let Some(service) = config.connection.sites.get(index) else {
                    return;
                };
                if service.is_main() {
                    drop(config);
                    show_error(
                        &dialog_for_remove,
                        "Remove service",
                        "The main Audiopub service cannot be removed.",
                    );
                    return;
                }
                config.connection.sites.remove(index);
            }
            app.save_config();
            refresh_services(None);
            load_fields();
        });
    }

    {
        let app = app.clone();
        let update_connect_label = update_connect_label.clone();
        let save_fields = save_fields.clone();
        let dialog_for_connect = dialog;
        connect_button.on_click(move |_| {
            let connected = app.run.borrow().connected_service.is_some();
            if connected {
                app.net.send(NetCommand::Disconnect);
                app.run.borrow_mut().connected_service = None;
                update_connect_label();
                return;
            }
            let Some(id) = save_fields() else { return };
            let profile = {
                let config = app.config.borrow();
                let Some(service) = config.connection.site(&id) else {
                    return;
                };
                match super::service_profile_from_site(service) {
                    Ok(profile) => profile,
                    Err(message) => {
                        drop(config);
                        show_error(&dialog_for_connect, "Connect", &message);
                        return;
                    }
                }
            };
            app.run.borrow_mut().connecting = true;
            app.net.send(NetCommand::Connect { profile });
            // Result arrives via the pump; the dialog may already be closed
            // by then, which is fine - the main window reports the outcome.
        });
    }

    {
        let dialog_for_close = dialog;
        let save_fields = save_fields.clone();
        close_button.on_click(move |_| {
            let _ = save_fields();
            dialog_for_close.end_modal(ID_CANCEL);
        });
    }

    // Register so connection results arriving on the pump report inside
    // this dialog and return focus to the connect button.
    *app.connect_ui.borrow_mut() = Some(super::ConnectUi {
        dialog,
        connect_button,
    });
    dialog.show_modal();
    *app.connect_ui.borrow_mut() = None;
    dialog.destroy();
}

fn prompt_new_service(parent: &Dialog) -> Option<(String, StreamingServiceType)> {
    let dialog = Dialog::builder(parent, "Add streaming service")
        .with_style(DialogStyle::DefaultDialogStyle)
        .with_size(360, 220)
        .build();
    let panel = Panel::builder(&dialog).build();
    let sizer = BoxSizer::builder(Orientation::Vertical).build();
    let nickname_label = StaticText::builder(&panel).with_label("Nickname").build();
    let nickname_input = TextCtrl::builder(&panel).build();
    super::set_accessible_name(&nickname_input, "Nickname");
    super::help::tag(
        &nickname_input,
        "dialog.connect.nickname",
        "Streaming service nickname",
    );
    let service_type = RadioBox::builder(&panel, &["Audiopub", "Icecast"])
        .with_label("Service type")
        .with_style(RadioBoxStyle::SpecifyRows)
        .with_major_dimension(1)
        .build();
    service_type.set_selection(0);
    super::native_acc::install_radio_box(&service_type, "Service type");
    super::help::tag(
        &service_type,
        "dialog.connect.addServiceType",
        "New service type selector",
    );
    let buttons = BoxSizer::builder(Orientation::Horizontal).build();
    let ok = super::ok_button(&panel, "OK");
    let cancel = Button::builder(&panel)
        .with_id(ID_CANCEL)
        .with_label("Cancel")
        .build();
    {
        ok.on_click(move |_| dialog.end_modal(ID_OK));
    }
    {
        cancel.on_click(move |_| dialog.end_modal(ID_CANCEL));
    }
    buttons.add(&ok, 0, SizerFlag::All, 4);
    buttons.add(&cancel, 0, SizerFlag::All, 4);
    sizer.add(&nickname_label, 0, SizerFlag::All, 4);
    sizer.add(&nickname_input, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add(&service_type, 0, SizerFlag::Expand | SizerFlag::All, 4);
    sizer.add_sizer(&buttons, 0, SizerFlag::Expand, 0);
    panel.set_sizer(sizer, true);
    let dialog_sizer = BoxSizer::builder(Orientation::Vertical).build();
    dialog_sizer.add(&panel, 1, SizerFlag::Expand, 0);
    dialog.set_sizer(dialog_sizer, true);

    loop {
        if dialog.show_modal() != ID_OK {
            dialog.destroy();
            return None;
        }
        let nickname = nickname_input.get_value().trim().to_string();
        if nickname.is_empty() {
            show_error(&dialog, "Add service", "Enter a nickname.");
            continue;
        }
        let kind = if service_type.get_selection() == 1 {
            StreamingServiceType::Icecast
        } else {
            StreamingServiceType::Audiopub
        };
        dialog.destroy();
        return Some((nickname, kind));
    }
}
