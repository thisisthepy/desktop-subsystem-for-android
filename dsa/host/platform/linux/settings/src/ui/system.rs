//! The pages about how Android meets this computer: Device identity, Network and Advanced.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use super::common::*;
use crate::i18n::tr;
use crate::model::{AdbAutoConnectState, Identity, Settings};
use crate::view;

/// This computer, as its firmware and system report it (its name, maker and model, then its
/// specifications, which cannot be changed), and then the names and the build fingerprint
/// Android reports to apps, which can.
pub fn identity(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());
    // This computer: its name large, its maker and model under it, as GNOME's About shows the
    // device.
    let banner = gtk::Box::new(gtk::Orientation::Vertical, 4);
    banner.set_margin_bottom(6);
    let computer_name = gtk::Label::builder()
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .build();
    computer_name.add_css_class("title-1");
    let make_model = gtk::Label::builder()
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .build();
    make_model.add_css_class("dim-label");
    banner.append(&computer_name);
    banner.append(&make_model);
    let banner_group = group("", None);
    banner_group.add(&banner);
    page.add(&banner_group);
    let specs = group(
        &tr("Device specifications"),
        Some(&tr("These follow this computer and are read again each time Android starts, so a renamed computer is followed from the next start. Android's own Settings shows the device name and the chip; the model name is shown only here, as Android's Settings has no place for it.")),
    );
    let copy = gtk::Button::with_label(&tr("Copy"));
    copy.add_css_class("flat");
    specs.set_header_suffix(Some(&copy));
    let spec_rows: Rc<Vec<(&'static str, adw::ActionRow)>> = Rc::new(
        crate::model::ABOUT_KEYS
            .iter()
            .map(|key| {
                let row = adw::ActionRow::builder()
                    .title(view::identity_label(key))
                    .subtitle_selectable(true)
                    .use_markup(false)
                    .build();
                row.add_css_class("property");
                specs.add(&row);
                (*key, row)
            })
            .collect(),
    );
    page.add(&specs);

    let intro = group(
        &tr("What Android reports"),
        Some(&tr("The names and build fingerprint Android reports to apps. Leave a box empty to keep the value shown in it. Android reads these when it starts, so a change applies from its next start.")),
    );
    let warning = action_row(
        &tr("Apps may notice"),
        &tr("Changing the model or the fingerprint can change how apps behave: some check the device before they run, and payment and integrity checks may fail."),
    );
    warning.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
    intro.add(&warning);
    page.add(&intro);

    let names = group(&tr("Device"), None);
    let print = group(&tr("Build fingerprint"), None);
    let keys: Vec<&'static str> = dsa_host_core::device_identity::Field::ALL
        .iter()
        .map(|f| f.key())
        .collect();
    // Where each default came from, beside its box.
    let origins: Rc<Vec<(&'static str, gtk::Label)>> = Rc::new(
        keys.iter()
            .map(|key| {
                let label = gtk::Label::new(None);
                label.add_css_class("dim-label");
                label.add_css_class("caption");
                (*key, label)
            })
            .collect(),
    );
    let entries: Rc<Vec<(&'static str, adw::EntryRow)>> = Rc::new(
        keys.iter()
            .zip(origins.iter())
            .map(|(key, (_, origin))| {
                let row = adw::EntryRow::builder()
                    .title(view::identity_label(key))
                    .use_markup(false)
                    .build();
                row.add_suffix(origin);
                if key.starts_with("fingerprint") {
                    print.add(&row);
                } else {
                    names.add(&row);
                }
                (*key, row)
            })
            .collect(),
    );
    page.add(&names);
    page.add(&print);
    let (preview_row, preview) = value_row(&tr("Fingerprint from the next start"), "");
    print.add(&preview_row);

    let actions = group("", None);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    buttons.set_halign(gtk::Align::End);
    let restart = button(&tr("Restart Android now"));
    let reset = button(&tr("Reset to defaults"));
    let save = button(&tr("Save"));
    save.add_css_class("suggested-action");
    buttons.append(&restart);
    buttons.append(&reset);
    buttons.append(&save);
    actions.add(&buttons);
    page.add(&actions);

    let known: Rc<RefCell<Option<Identity>>> = Rc::default();
    let update_preview = {
        let (entries, known, preview, save) = (
            entries.clone(),
            known.clone(),
            preview.clone(),
            save.clone(),
        );
        Rc::new(move || {
            let known = known.borrow();
            let value = |key: &str| {
                let typed = entries
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, e)| e.text().to_string())
                    .unwrap_or_default();
                if typed.is_empty() {
                    known
                        .as_ref()
                        .map(|i| i.effective(key).to_string())
                        .unwrap_or_default()
                } else {
                    typed
                }
            };
            preview.set_label(&view::fingerprint_preview(value));
            let mut valid = true;
            for (key, entry) in entries.iter() {
                match view::identity_problem(key, &entry.text()) {
                    Some(_) => {
                        entry.add_css_class("error");
                        valid = false;
                    }
                    None => entry.remove_css_class("error"),
                }
            }
            save.set_sensitive(valid);
        })
    };
    for (_, entry) in entries.iter() {
        let update_preview = update_preview.clone();
        entry.connect_changed(move |_| update_preview());
    }
    let show: Rc<dyn Fn(Result<Identity, crate::api::Failure>)> = {
        let (page, entries, known, update_preview, origins, spec_rows) = (
            page.clone(),
            entries.clone(),
            known.clone(),
            update_preview.clone(),
            origins.clone(),
            spec_rows.clone(),
        );
        let (computer_name, make_model) = (computer_name.clone(), make_model.clone());
        Rc::new(move |r: Result<Identity, crate::api::Failure>| {
            page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
            let Ok(identity) = r else { return };
            for (key, origin) in origins.iter() {
                origin.set_label(&view::origin_label(identity.origin(key)));
            }
            for (key, row) in spec_rows.iter() {
                row.set_subtitle(&match identity.of_computer(key) {
                    "" => tr("Not reported"),
                    value => value.to_string(),
                });
            }
            computer_name.set_label(identity.of_computer("device_name"));
            make_model.set_label(&identity.computer_make_model());
            for (key, entry) in entries.iter() {
                entry.set_text(identity.chosen(key));
                // The value Android reports shows where nothing is chosen.
                entry.set_tooltip_text(Some(identity.effective(key)));
                if identity.chosen(key).is_empty() {
                    entry.set_title(&format!(
                        "{} ({})",
                        view::identity_label(key),
                        identity.effective(key)
                    ));
                } else {
                    entry.set_title(&view::identity_label(key));
                }
            }
            *known.borrow_mut() = Some(identity);
            update_preview();
        })
    };
    // Copy puts every specification on the clipboard, a row per line.
    {
        let known = known.clone();
        copy.connect_clicked(move |button| {
            if let Some(identity) = known.borrow().as_ref() {
                let text = identity.about_text(view::identity_label, &tr("Not reported"));
                button.clipboard().set_text(&text);
            }
        });
    }
    let restart_hint = {
        let (ctx, restart) = (ctx.clone(), restart.clone());
        Rc::new(move |saved: &str, saved_restart: &str| {
            let ctx2 = ctx.clone();
            let restart = restart.clone();
            let (saved, saved_restart) = (saved.to_string(), saved_restart.to_string());
            request(
                &ctx,
                |c| c.status(),
                move |r| {
                    let running = r.is_ok_and(|s| view::controls(Some(&s)).can_stop);
                    restart.set_visible(running);
                    ctx2.toast(if running { &saved_restart } else { &saved });
                },
            );
        })
    };
    {
        let (ctx, entries, known, show, hint) = (
            ctx.clone(),
            entries.clone(),
            known.clone(),
            show.clone(),
            restart_hint.clone(),
        );
        save.connect_clicked(move |_| {
            let Some(identity) = known.borrow().clone() else {
                return;
            };
            let changes: Vec<(String, String)> = entries
                .iter()
                .map(|(key, entry)| (key.to_string(), entry.text().to_string()))
                .filter(|(key, value)| identity.chosen(key) != value.as_str())
                .collect();
            let (show, hint) = (show.clone(), hint.clone());
            request(
                &ctx,
                move |c| {
                    let mut last = c.identity()?;
                    for (key, value) in &changes {
                        last = c.set_identity(key, value)?;
                    }
                    Ok(last)
                },
                move |r| {
                    let ok = r.is_ok();
                    show(r);
                    if ok {
                        hint(
                            &tr("Saved. Android uses it from its next start."),
                            &tr("Saved. Restart Android to use it now."),
                        );
                    }
                },
            );
        });
    }
    {
        let (ctx, show, hint) = (ctx.clone(), show.clone(), restart_hint.clone());
        reset.connect_clicked(move |_| {
            let (ctx2, show, hint) = (ctx.clone(), show.clone(), hint.clone());
            confirm(
                &ctx,
                &tr("Reset the device identity?"),
                &tr("Every value goes back to its default: this computer's, or DSA's own where the computer does not say. It applies from the next start of Android."),
                &tr("Reset to defaults"),
                true,
                move || {
                    let (show, hint) = (show.clone(), hint.clone());
                    request(&ctx2, |c| c.reset_identity(), move |r| {
                        let ok = r.is_ok();
                        show(r);
                        if ok {
                            hint(
                                &tr("Back to the defaults. Android uses them from its next start."),
                                &tr("Back to the defaults. Restart Android to use them now."),
                            );
                        }
                    });
                },
            );
        });
    }
    restart.set_visible(false);
    {
        let (ctx, page) = (ctx.clone(), page.clone());
        restart.connect_clicked(move |button| {
            button.set_sensitive(false);
            let (ctx2, page, button) = (ctx.clone(), page.clone(), button.clone());
            request(
                &ctx,
                |c| c.stop().and_then(|_| c.start()),
                move |r| {
                    button.set_sensitive(true);
                    page.show_failure(&tr("Something went wrong"), r.as_ref().err());
                    if r.is_ok() {
                        button.set_visible(false);
                        ctx2.toast(&tr("Android started again with this identity."));
                    }
                },
            );
        });
    }
    request(ctx, |c| c.identity(), move |r| show(r));
    page.root.clone().upcast()
}

/// Android's network, advanced networking, and the root certificates added to this computer
/// that Android trusts.
pub fn network(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());
    let status = group(&tr("Android's network"), None);
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some(&tr("Refresh")));
    refresh.add_css_class("flat");
    status.set_header_suffix(Some(&refresh));
    let mode = action_row("", "");
    mode.set_title(&tr("Connection"));
    status.add(&mode);
    let (_, address) = {
        let (row, label) = value_row(&tr("Address"), "");
        status.add(&row);
        (row, label)
    };
    let (_, dns) = {
        let (row, label) = value_row(&tr("DNS"), "");
        status.add(&row);
        (row, label)
    };
    let (_, gateway) = {
        let (row, label) = value_row(&tr("Gateway"), "");
        status.add(&row);
        (row, label)
    };
    let problem = action_row(&tr("Advanced networking is not in use"), "");
    problem.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
    problem.set_visible(false);
    status.add(&problem);
    page.add(&status);

    let advanced = group(&tr("Advanced"), None);
    let restart = action_row(
        &tr("Restart Android to apply"),
        &tr("The change applies the next time Android starts. Stop Android on the General page, then open an app."),
    );
    restart.set_visible(false);
    // The added root certificates: the switch first, then one row for each certificate, which
    // `certificates` replaces each time the service answers.
    let certificates = Rows::new(group(
        &tr("Trusted certificates"),
        Some(&tr("Root certificates that you or an administrator added to this computer, such as a company's or a security program's. The certificates that come with the system are not listed; Android has its own.")),
    ));
    let certificate_switch: Rc<RefCell<Option<Switch>>> = Rc::default();
    let show: Rc<dyn Fn(Result<Settings, crate::api::Failure>)> = Rc::new({
        let (page, restart) = (page.clone(), restart.clone());
        let (certificates, certificate_switch) = (certificates.clone(), certificate_switch.clone());
        move |r: Result<Settings, crate::api::Failure>| {
            page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
            let Ok(settings) = r else { return };
            if let Some(s) = certificate_switch.borrow().as_ref() {
                s.set(settings.host_certificates);
            }
            certificates.clear();
            if settings.imported_certificates.is_empty() {
                certificates.note(&view::certificates_empty_text());
            }
            for certificate in &settings.imported_certificates {
                let row = action_row(
                    &view::certificate_title(certificate),
                    &view::certificate_detail(certificate),
                );
                row.set_subtitle_selectable(true);
                certificates.add(&row);
            }
            let network = settings.network.as_ref();
            mode.set_subtitle(&view::network_mode_text(network));
            address.set_label(&view::list_or_none(
                network.map(|n| n.addresses.as_slice()).unwrap_or_default(),
            ));
            dns.set_label(&view::network_dns_text(network));
            gateway.set_label(&network.map(|n| n.gateway.clone()).unwrap_or_default());
            let why = network.and_then(|n| n.problem.clone());
            problem.set_visible(why.is_some());
            problem.set_subtitle(why.as_deref().unwrap_or_default());
            restart.set_visible(view::network_restart_needed(
                settings.advanced_networking,
                network,
            ));
        }
    });
    let switch: Rc<RefCell<Option<Switch>>> = Rc::default();
    let advanced_switch = {
        let (ctx, show, slot) = (ctx.clone(), show.clone(), switch.clone());
        Switch::new(
            &tr("Advanced networking"),
            &tr("Off by default. When on, Android joins this computer's network as a device of its own: other devices on the network can reach Android apps, and Android apps can reach them directly. Programs on this computer can always reach a server in Android at the address above."),
            move |on| {
                let (show, slot) = (show.clone(), slot.clone());
                request(&ctx, move |c| c.switch("advanced-networking", on), move |r| {
                    if let (Ok(settings), Some(s)) = (r.as_ref(), slot.borrow().as_ref()) {
                        s.set(settings.advanced_networking);
                    }
                    show(r);
                });
            },
        )
    };
    advanced.add(&advanced_switch.row);
    advanced.add(&restart);
    *switch.borrow_mut() = Some(advanced_switch);
    page.add(&advanced);

    let trust = {
        let (ctx, show, slot) = (ctx.clone(), show.clone(), certificate_switch.clone());
        Switch::new(
            &tr("Trust certificates added to this computer"),
            &tr("On by default. When on, Android apps trust the root certificates listed below as they trust Android's own, so sites and servers that this computer trusts open in Android too. The change applies at once."),
            move |on| {
                let (show, slot) = (show.clone(), slot.clone());
                request(&ctx, move |c| c.set_host_certificates(on), move |r| {
                    // Not changed: the switch goes back to what the service keeps.
                    if let (Err(_), Some(s)) = (r.as_ref(), slot.borrow().as_ref()) {
                        s.set(!on);
                    }
                    show(r);
                });
            },
        )
    };
    trust.set(true);
    certificates.group.add(&trust.row);
    *certificate_switch.borrow_mut() = Some(trust);
    page.add(&certificates.group);
    page.add(&developer(ctx, &page));

    let reload = {
        let (ctx, show, switch) = (ctx.clone(), show.clone(), switch.clone());
        Rc::new(move || {
            let (show, switch) = (show.clone(), switch.clone());
            request(
                &ctx,
                |c| c.settings(),
                move |r| {
                    if let (Ok(settings), Some(s)) = (r.as_ref(), switch.borrow().as_ref()) {
                        s.set(settings.advanced_networking);
                    }
                    show(r);
                },
            );
        })
    };
    {
        let reload = reload.clone();
        refresh.connect_clicked(move |_| reload());
    }
    reload();
    page.root.clone().upcast()
}

/// The user's switches, applied through the service, and what each switch says it does.
struct Switches {
    file_sharing: Switch,
    folders: Vec<(&'static str, Switch)>,
    links: Switch,
    clipboard: Switch,
    arm: Switch,
    freeze: Switch,
    input_trace: Switch,
    resize_trace: Switch,
    root_elevation: Switch,
    root_restart: adw::ActionRow,
    root_log: adw::ActionRow,
    /// Root elevation as the page first read it, to tell when a change waits for the next
    /// start of Android.
    root_at_start: Cell<Option<bool>>,
}

/// Files, links, ARM apps, the clipboard, freezing, the diagnostics, the logs, the command line,
/// the backup of the apps' data and resetting Android's data.
pub fn advanced(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());
    let settings: Rc<RefCell<Option<Settings>>> = Rc::default();
    let switches: Rc<RefCell<Option<Switches>>> = Rc::default();

    // Shows what the service now keeps; every switch follows it.
    let show: Rc<dyn Fn(Result<Settings, crate::api::Failure>)> = {
        let (page, settings, switches) = (page.clone(), settings.clone(), switches.clone());
        Rc::new(move |r: Result<Settings, crate::api::Failure>| {
            page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
            let Ok(now) = r else {
                // Put every switch back where the service has it.
                if let (Some(s), Some(now)) =
                    (switches.borrow().as_ref(), settings.borrow().as_ref())
                {
                    apply(s, now);
                }
                return;
            };
            if let Some(s) = switches.borrow().as_ref() {
                apply(s, &now);
            }
            *settings.borrow_mut() = Some(now);
        })
    };
    let user_switch = |name: &'static str, title: String, subtitle: String| {
        let (ctx, show) = (ctx.clone(), show.clone());
        Switch::new(&title, &subtitle, move |on| {
            let show = show.clone();
            request(&ctx, move |c| c.switch(name, on), move |r| show(r));
        })
    };
    let machine_switch = |name: &'static str, title: String, subtitle: String| {
        let (ctx, show) = (ctx.clone(), show.clone());
        Switch::new(&title, &subtitle, move |on| {
            let show = show.clone();
            request(&ctx, move |c| c.set_machine(name, on), move |r| show(r));
        })
    };

    // Files.
    let files = group(&tr("Files"), None);
    let file_sharing = user_switch(
        "file-sharing",
        tr("Share folders with Android"),
        tr("On by default: the folders chosen below show in Android's shared storage, so Android apps can open and save files there. Android apps reach them only with Android's storage permission, and they can do only what your own account may do."),
    );
    files.add(&file_sharing.row);
    let folders_row = adw::ExpanderRow::builder()
        .title(tr("Shared folders"))
        .subtitle(tr("A folder you clear is taken away from Android at once."))
        .use_markup(false)
        .build();
    let folders: Vec<(&'static str, Switch)> = crate::model::ALL_FOLDERS
        .iter()
        .map(|folder| {
            let folder: &'static str = folder;
            let (ctx, show, settings) = (ctx.clone(), show.clone(), settings.clone());
            let switch = Switch::new(&view::folder_label(folder), "", move |on| {
                let current = settings
                    .borrow()
                    .as_ref()
                    .map(|s| s.shared_folders.clone())
                    .unwrap_or_default();
                let wanted = view::toggle_folder(&current, folder, on);
                let show = show.clone();
                request(
                    &ctx,
                    move |c| c.set_shared_folders(&wanted),
                    move |r| show(r),
                );
            });
            folders_row.add_row(&switch.row);
            (folder, switch)
        })
        .collect();
    files.add(&folders_row);
    let android_files = link_row(
        &tr("Android files"),
        &tr("Opens Android's shared storage in the file manager: the files Android apps saved, such as photos and downloads. Android starts if it is not running. DSA Files in the app grid does the same."),
    );
    files.add(&android_files);
    // A row of its own, so the text wraps inside the group like the other rows.
    files.add(&action_row(
        &tr("Send files to Android apps"),
        &tr("To send a file to an Android app, drag it from the file manager onto the app's window. An APK you drop is installed."),
    ));
    {
        let (ctx, page) = (ctx.clone(), page.clone());
        android_files.connect_activated(move |_| {
            let page = page.clone();
            request(
                &ctx,
                |c| c.open_android_files(),
                move |r| {
                    page.show_failure(&tr("The folder could not be opened"), r.as_ref().err());
                },
            );
        });
    }
    let links = user_switch(
        "links-in-android",
        tr("Open web links in Android apps"),
        tr("Off by default: a web link you tap in an Android app opens in your default browser on this computer. On, it opens in an Android app, as on a phone. A link an installed app has verified as its own opens in that app either way."),
    );
    files.add(&links.row);
    page.add(&files);

    // ARM apps.
    let arm_group = group(&tr("ARM apps"), None);
    let arm = user_switch(
        "arm-translation",
        tr("Run apps built only for ARM"),
        tr("Many Android apps contain code only for ARM processors. They install and run here only through an ARM translator. Android uses the open source translator that comes with it, unless you install another ARM translation package (dsa arm-translation install) or Google's translator. This switch decides whether Android uses a translator at all; it applies from the next start of Android. Only 64-bit ARM apps (arm64-v8a) can run."),
    );
    arm_group.add(&arm.row);
    let (arm_status_row, arm_status) = value_row(&tr("Status"), "");
    arm_group.add(&arm_status_row);
    let arm_restart = action_row(
        &tr("Restart Android to apply"),
        &tr(
            "Android started before this change. Stop and start Android for ARM apps to follow it.",
        ),
    );
    arm_restart.set_visible(false);
    arm_group.add(&arm_restart);
    let (arm_remove_row, arm_remove) = button_row(
        &tr("Remove the ARM translation package"),
        &tr("Deletes the package from your add-on disk. ARM apps you installed stay, but do not run after Android restarts."),
        &tr("Remove"),
    );
    arm_remove.add_css_class("destructive-action");
    arm_group.add(&arm_remove_row);
    // Where the image has a default translator, removing the package goes back to it, so this
    // row takes the remove row's place.
    let (arm_default_row, arm_default) =
        button_row(&tr("Use the default translator"), "", &tr("Use"));
    arm_default_row.set_visible(false);
    arm_group.add(&arm_default_row);
    let google = google_arm_rows(&arm_group);
    page.add(&arm_group);
    let show_arm: Rc<dyn Fn(Result<crate::model::ArmTranslation, crate::api::Failure>)> = {
        let (page, arm_restart, arm_remove_row, arm_default_row) = (
            page.clone(),
            arm_restart.clone(),
            arm_remove_row.clone(),
            arm_default_row.clone(),
        );
        let google = google.clone();
        Rc::new(
            move |r: Result<crate::model::ArmTranslation, crate::api::Failure>| {
                page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                let Ok(status) = r else { return };
                arm_status.set_label(&view::arm_status(&status));
                arm_status_row.set_subtitle(&view::arm_abis(&status));
                arm_restart.set_visible(status.restart_needed);
                let default = view::arm_default_text(&status);
                arm_remove_row.set_visible(default.is_none());
                arm_remove_row.set_sensitive(status.package.is_some());
                arm_default_row.set_visible(default.is_some());
                arm_default_row.set_subtitle(default.as_deref().unwrap_or_default());
                arm_default_row.set_sensitive(view::arm_can_use_default(&status));
                google.offer(status.google_fetch_available);
            },
        )
    };
    google.connect(ctx, show_arm.clone());
    {
        let (ctx, show_arm) = (ctx.clone(), show_arm.clone());
        arm_remove.connect_clicked(move |_| {
            let show_arm = show_arm.clone();
            request(&ctx, |c| c.remove_arm_translation(), move |r| show_arm(r));
        });
    }
    {
        let (ctx, show_arm) = (ctx.clone(), show_arm.clone());
        arm_default.connect_clicked(move |_| {
            let show_arm = show_arm.clone();
            request(
                &ctx,
                |c| c.use_default_arm_translator(),
                move |r| show_arm(r),
            );
        });
    }
    {
        let show_arm = show_arm.clone();
        request(ctx, |c| c.arm_translation(), move |r| show_arm(r));
    }

    // Google Play.
    google_play(ctx, &page);

    // Protected content.
    widevine(ctx, &page);

    // The clipboard and freezing.
    let developer_group = group("", None);
    let clipboard = user_switch(
        "clipboard",
        tr("Share the clipboard with Android"),
        tr("On by default: text you copy on this computer can be pasted in Android apps, and text copied in Android can be pasted on this computer. Images stay where they were copied."),
    );
    developer_group.add(&clipboard.row);
    let freeze = user_switch(
        "freeze",
        tr("Freeze Android when idle"),
        tr("Off by default: an idle Android dozes and still receives notifications. Frozen, it uses no CPU but receives nothing until you open an app."),
    );
    developer_group.add(&freeze.row);
    page.add(&developer_group);

    // Root access: it applies to every user of the computer, from the next start of Android.
    let root_group = group(
        &tr("Root access"),
        Some(&tr("This applies to every user of this computer, so changing it asks for an administrator's permission. Choose each app's level under Apps.")),
    );
    let root_elevation = machine_switch(
        "root-elevation",
        tr("Let Android apps ask for root"),
        tr("An app you allow under Apps can run commands as root once an administrator of this computer approves, as sudo does. Off by default. The Android image, its build properties and SELinux stay as they are."),
    );
    root_group.add(&root_elevation.row);
    let root_restart = action_row(
        &tr("Restart Android to apply"),
        &tr("Root access changes from the next start of Android. Stop Android on the General page, then open an app."),
    );
    root_restart.set_visible(false);
    root_group.add(&root_restart);
    let root_log = link_row(
        &tr("Where requests are recorded"),
        &view::root_log_text(None),
    );
    root_group.add(&root_log);
    {
        let (ctx, settings) = (ctx.clone(), settings.clone());
        root_log.connect_activated(move |_| {
            let reported = settings.borrow().as_ref().and_then(|s| s.log_dir.clone());
            open_folder(&ctx, &view::service_log_folder(reported.as_deref()));
        });
    }
    page.add(&root_group);

    // Diagnostics: they apply to every user of the computer.
    let diagnostics = group(
        &tr("Diagnostics"),
        Some(&tr("The traces go to the DSA logs below and make them grow, so turn them on only to report a problem. They apply to every user of this computer, so changing them asks for an administrator's permission.")),
    );
    let input_trace = machine_switch(
        "input-trace",
        tr("Input trace"),
        tr("Every DSA process writes when each touch, click and key reaches it, so the delay from your input to the picture can be measured. Off by default."),
    );
    diagnostics.add(&input_trace.row);
    let resize_trace = machine_switch(
        "resize-trace",
        tr("Resize trace"),
        tr("Every DSA process writes each stage of each window resize, to find where a resize is slow or flickers. Off by default."),
    );
    diagnostics.add(&resize_trace.row);
    page.add(&diagnostics);

    // Logs and the command line.
    let logs = group(&tr("Logs"), None);
    let user_logs = link_row(
        &tr("Open my log folder"),
        &tr("The logs of your window manager and app windows."),
    );
    logs.add(&user_logs);
    let service_logs = link_row(
        &tr("Open the service's log folder (administrators)"),
        &tr("The logs of the DSA service and of Android itself."),
    );
    logs.add(&service_logs);
    {
        let ctx = ctx.clone();
        user_logs.connect_activated(move |_| open_folder(&ctx, &view::user_log_folder()));
    }
    {
        let (ctx, settings) = (ctx.clone(), settings.clone());
        service_logs.connect_activated(move |_| {
            let reported = settings.borrow().as_ref().and_then(|s| s.log_dir.clone());
            open_folder(&ctx, &view::service_log_folder(reported.as_deref()));
        });
    }
    let command = group(&tr("Command line"), None);
    let cli_row = action_row(
        &tr("The dsa command"),
        &tr("Everything on these pages is also available from a terminal."),
    );
    command.add(&cli_row);
    // Each command is its own row with its own copy button: they are separate commands, and
    // one line with all of them would read as a shell pipeline.
    for cli_command in view::COMMAND_LINE_COMMANDS {
        let (row, _) = command_row(cli_command);
        command.add(&row);
    }
    page.add(&logs);
    page.add(&command);

    // The backup of the apps' data, which brings them back after a reset.
    super::backup::backup(ctx, &page);

    // Reset.
    let reset_group = group(&tr("Reset"), None);
    let (reset_row, reset) = button_row(
        &tr("Reset Android's data"),
        &tr("Erases every app you installed, your accounts and the data of every app, so Android starts as it was first installed. The Android image and these settings stay."),
        &tr("Reset"),
    );
    reset.add_css_class("destructive-action");
    reset_group.add(&reset_row);
    page.add(&reset_group);
    {
        let (ctx, page) = (ctx.clone(), page.clone());
        reset.connect_clicked(move |button| {
            let (ctx2, page, button) = (ctx.clone(), page.clone(), button.clone());
            confirm(
                &ctx,
                &tr("Reset Android's data?"),
                &tr("This erases every app you installed in Android, the accounts you signed in to and the data of every app. Android stops now, and its next start is as it was first installed. The Android image and DSA's settings stay. This cannot be undone."),
                &tr("Reset"),
                true,
                move || {
                    button.set_sensitive(false);
                    let (ctx3, page, button) = (ctx2.clone(), page.clone(), button.clone());
                    request(&ctx2, |c| c.reset_data(), move |r| {
                        button.set_sensitive(true);
                        page.show_failure(&tr("Something went wrong"), r.as_ref().err());
                        if r.is_ok() {
                            tell(
                                &ctx3,
                                &tr("Android's data was reset"),
                                &tr("The next app you open starts Android as it was first installed."),
                            );
                        }
                    });
                },
            );
        });
    }

    *switches.borrow_mut() = Some(Switches {
        file_sharing,
        folders,
        links,
        clipboard,
        arm,
        freeze,
        input_trace,
        resize_trace,
        root_elevation,
        root_restart,
        root_log,
        root_at_start: Cell::new(None),
    });
    request(ctx, |c| c.settings(), move |r| show(r));
    page.root.clone().upcast()
}

/// Developer mode: whether adb reaches Android through this computer's loopback port, the
/// port, attaching it to the adb server by itself, and the command that connects. It is on
/// the Network page, since it is how a program on this computer reaches Android.
fn developer(ctx: &Shared, page: &Rc<Page>) -> adw::PreferencesGroup {
    let settings: Rc<RefCell<Option<Settings>>> = Rc::default();
    let switches: Rc<RefCell<Option<(Switch, Switch)>>> = Rc::default();
    let developer_group = group(&tr("Developer"), None);
    let auto_connect_state = action_row(
        &tr("Automatic connection"),
        &view::adb_auto_connect_text(AdbAutoConnectState::Off),
    );
    let (adb_row, adb_command) = value_row(
        &tr("Connect adb"),
        &format!("adb connect 127.0.0.1:{}", crate::model::DEFAULT_ADB_PORT),
    );
    let port = adw::EntryRow::builder()
        .title(tr("Port"))
        .show_apply_button(true)
        .input_purpose(gtk::InputPurpose::Digits)
        .use_markup(false)
        .build();
    // Shows what the service now keeps; every row follows it.
    let show: Rc<dyn Fn(Result<Settings, crate::api::Failure>)> = {
        let (page, settings, switches) = (page.clone(), settings.clone(), switches.clone());
        let (state, command, port) = (
            auto_connect_state.clone(),
            adb_command.clone(),
            port.clone(),
        );
        Rc::new(move |r: Result<Settings, crate::api::Failure>| {
            page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
            let now = match r {
                Ok(now) => now,
                Err(_) => match settings.borrow().clone() {
                    Some(kept) => kept,
                    None => return,
                },
            };
            if let Some((developer, auto_connect)) = switches.borrow().as_ref() {
                developer.set(now.developer_mode);
                auto_connect.set(now.adb_auto_connect);
            }
            state.set_subtitle(&view::adb_auto_connect_text(now.adb_auto_connect_state));
            command.set_label(&now.adb_connect());
            port.set_text(&now.adb_port.to_string());
            *settings.borrow_mut() = Some(now);
        })
    };
    let user_switch = |name: &'static str, title: String, subtitle: String| {
        let (ctx, show) = (ctx.clone(), show.clone());
        Switch::new(&title, &subtitle, move |on| {
            let show = show.clone();
            request(&ctx, move |c| c.switch(name, on), move |r| show(r));
        })
    };
    let developer = user_switch(
        "developer-mode",
        tr("Developer mode"),
        tr("Off by default. When on, adb on this computer can connect to Android without asking in Android, through a port only this computer can reach. Turn it off when you are done."),
    );
    developer_group.add(&developer.row);
    let auto_connect = user_switch(
        "adb-auto-connect",
        tr("Connect adb automatically"),
        tr("On by default. While developer mode is on, Android shows up in adb devices and Android Studio without running adb connect. If you run adb disconnect, it stays off until the adb server restarts."),
    );
    developer_group.add(&auto_connect.row);
    developer_group.add(&auto_connect_state);
    adb_row.set_subtitle(&tr(
        "While developer mode is on and Android is running, run this command:",
    ));
    adb_command.add_css_class("monospace");
    adb_row.add_suffix(&copy_button(&adb_command));
    developer_group.add(&adb_row);
    {
        let (ctx, show) = (ctx.clone(), show.clone());
        port.connect_apply(move |entry| {
            let value = entry.text().trim().to_string();
            let show = show.clone();
            request(&ctx, move |c| c.set("adb-port", &value), move |r| show(r));
        });
    }
    developer_group.add(&port);
    *switches.borrow_mut() = Some((developer, auto_connect));
    {
        let show = show.clone();
        request(ctx, |c| c.settings(), move |r| show(r));
    }
    // Where auto-connect stands changes by itself, so it is read again every few seconds
    // while the page is shown and developer mode is on. Only the state row follows it, so a
    // port being typed is kept.
    {
        let (ctx, settings) = (ctx.clone(), settings.clone());
        let row = auto_connect_state.downgrade();
        let asking = Rc::new(Cell::new(false));
        gtk::glib::timeout_add_seconds_local(3, move || {
            let Some(row) = row.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            let developer_mode = settings.borrow().as_ref().is_some_and(|s| s.developer_mode);
            if !row.is_mapped() || !developer_mode || asking.get() {
                return gtk::glib::ControlFlow::Continue;
            }
            asking.set(true);
            let (settings, asking) = (settings.clone(), asking.clone());
            request(
                &ctx,
                |c| c.settings(),
                move |r| {
                    asking.set(false);
                    if let Ok(now) = r {
                        row.set_subtitle(&view::adb_auto_connect_text(now.adb_auto_connect_state));
                        if let Some(kept) = settings.borrow_mut().as_mut() {
                            kept.adb_auto_connect_state = now.adb_auto_connect_state;
                        }
                    }
                },
            );
            gtk::glib::ControlFlow::Continue
        });
    }
    developer_group
}

/// The rows of the ARM apps section for Google's ARM translator: getting it, the notice while
/// it downloads, and Google's terms. They show only where dsa can fetch it.
#[derive(Clone)]
struct GoogleArm {
    get_row: adw::ActionRow,
    get: gtk::Button,
    busy: adw::ActionRow,
    terms: adw::ActionRow,
}

fn google_arm_rows(arm_group: &adw::PreferencesGroup) -> GoogleArm {
    let (get_row, get) = button_row(
        &tr("Google's ARM translator"),
        &tr("Gets the ARM translator from the Android emulator image Google offers for this Android version and installs it as a translation package. DSA does not include or pass on Google's translator: your computer downloads the image from Google, under Google's Android SDK License, keeps only the translator and deletes the rest (about 2 GB). Nothing is downloaded until you agree. A package built from another translator, such as Digitalis, does the same job."),
        &tr("Get"),
    );
    get.add_css_class("suggested-action");
    let busy = action_row(&view::arm_google_busy_text(), "");
    let terms = link_row(
        &tr("Android SDK License"),
        &tr("Google's terms for the emulator image the translator comes from."),
    );
    for row in [&get_row, &busy, &terms] {
        row.set_visible(false);
        arm_group.add(row);
    }
    GoogleArm {
        get_row,
        get,
        busy,
        terms,
    }
}

impl GoogleArm {
    /// Shows the rows where dsa can fetch Google's translator, and hides them elsewhere.
    fn offer(&self, available: bool) {
        self.get_row.set_visible(available);
        self.terms.set_visible(available);
    }

    /// Asks first, saying where the download comes from and under which terms, then fetches
    /// the translator and shows the section as dsa reports it.
    fn connect(
        &self,
        ctx: &Shared,
        show: Rc<dyn Fn(Result<crate::model::ArmTranslation, crate::api::Failure>)>,
    ) {
        let this = self.clone();
        let asking = ctx.clone();
        self.get.connect_clicked(move |_| {
            let (this, ctx, show) = (this.clone(), asking.clone(), show.clone());
            confirm(
                &asking,
                &tr("Get Google's ARM translator?"),
                &view::arm_google_notice(),
                &tr("Accept and download"),
                false,
                move || {
                    this.busy.set_visible(true);
                    this.get_row.set_sensitive(false);
                    let (this, show) = (this.clone(), show.clone());
                    request(
                        &ctx,
                        |c| c.fetch_google_arm_translator(),
                        move |r| {
                            this.busy.set_visible(false);
                            this.get_row.set_sensitive(true);
                            show(r);
                        },
                    );
                },
            );
        });
        let ctx = ctx.clone();
        self.terms.connect_activated(move |_| {
            open_page(&ctx, crate::model::GOOGLE_ARM_TERMS_URL);
        });
    }
}

/// The Google Play item: whether Google's apps are installed, installing and removing them,
/// Google's terms and how to register this device with Google.
fn google_play(ctx: &Shared, page: &Rc<Page>) {
    let gapps_group = group(
        &tr("Google Play"),
        Some(&tr("Google Play services and the Play Store are not part of DSA. When you ask, DSA downloads LiteGapps Core, a public package of Google's apps, and installs it in your Android. Without it, the rest of DSA works as before.")),
    );
    let (status_row, status) = value_row(&tr("Status"), "");
    gapps_group.add(&status_row);
    let busy_row = action_row(&view::gapps_busy_text(), "");
    busy_row.set_visible(false);
    gapps_group.add(&busy_row);
    let restart = action_row(
        &tr("Restart Android to apply"),
        &tr("Android started before this change. Stop and start Android for Google Play to follow it."),
    );
    restart.set_visible(false);
    gapps_group.add(&restart);
    let (install_row, install) = button_row(
        &tr("Install Google Play"),
        &tr("Downloads LiteGapps Core for your Android version from the LiteGapps project, about 120 to 260 MB, checks it and installs it in your Android. Android starts if it is not running. Google Play is there from the next start of Android."),
        &tr("Install"),
    );
    install.add_css_class("suggested-action");
    gapps_group.add(&install_row);
    let (remove_row, remove) = button_row(
        &tr("Remove Google Play"),
        &tr("Deletes Google's apps from your add-on disk. They are gone from the next start of Android."),
        &tr("Remove"),
    );
    remove.add_css_class("destructive-action");
    gapps_group.add(&remove_row);
    let terms = link_row(
        &tr("Google's terms"),
        &tr("Google's apps belong to Google and follow Google's terms. DSA does not distribute them: it downloads the public LiteGapps package when you ask."),
    );
    gapps_group.add(&terms);
    let register = link_row(
        &tr("Register this device with Google"),
        &tr("Google has not certified this Android, so Google may refuse to sign you in until you register it. Open Google's registration page, sign in with your Google account and enter the Android ID below."),
    );
    gapps_group.add(&register);
    let (id_row, id) = value_row(&tr("Android ID"), "");
    gapps_group.add(&id_row);
    page.add(&gapps_group);

    let now: Rc<RefCell<Option<crate::model::Gapps>>> = Rc::default();
    let busy: Rc<Cell<bool>> = Rc::default();
    // The buttons follow what is installed, and neither works while a request is under way.
    let buttons: Rc<dyn Fn()> = {
        let (now, busy) = (now.clone(), busy.clone());
        Rc::new(move || {
            let (now, busy) = (now.borrow(), busy.get());
            install_row.set_sensitive(now.as_ref().is_some_and(|g| g.can_install(busy)));
            remove_row.set_sensitive(now.as_ref().is_some_and(|g| g.can_remove(busy)));
        })
    };
    let show: Rc<dyn Fn(String, Result<crate::model::Gapps, crate::api::Failure>)> = {
        let (page, now, buttons) = (page.clone(), now.clone(), buttons.clone());
        Rc::new(
            move |failed: String, r: Result<crate::model::Gapps, crate::api::Failure>| {
                page.show_failure(&failed, r.as_ref().err());
                if let Ok(gapps) = r {
                    status.set_label(&view::gapps_status(&gapps));
                    status_row.set_subtitle(&view::gapps_source(&gapps));
                    restart.set_visible(gapps.restart_needed);
                    id.set_label(gapps.device_id.as_deref().unwrap_or_default());
                    id_row.set_subtitle(&view::gapps_device_id(&gapps));
                    *now.borrow_mut() = Some(gapps);
                }
                buttons();
            },
        )
    };
    {
        let (ctx, show, busy, buttons) = (ctx.clone(), show.clone(), busy.clone(), buttons.clone());
        install.connect_clicked(move |_| {
            busy.set(true);
            busy_row.set_visible(true);
            buttons();
            let (show, busy, busy_row) = (show.clone(), busy.clone(), busy_row.clone());
            request(
                &ctx,
                |c| c.install_gapps(),
                move |r| {
                    busy.set(false);
                    busy_row.set_visible(false);
                    show(tr("Google Play was not installed"), r);
                },
            );
        });
    }
    {
        let (ctx, show, busy, buttons) = (ctx.clone(), show.clone(), busy.clone(), buttons.clone());
        remove.connect_clicked(move |_| {
            busy.set(true);
            buttons();
            let (show, busy) = (show.clone(), busy.clone());
            request(
                &ctx,
                |c| c.remove_gapps(),
                move |r| {
                    busy.set(false);
                    show(tr("Google Play was not removed"), r);
                },
            );
        });
    }
    {
        let (ctx, now) = (ctx.clone(), now.clone());
        terms.connect_activated(move |_| {
            let url = now.borrow().as_ref().map(|g| g.terms_url.clone());
            open_page(
                &ctx,
                &url.unwrap_or_else(|| crate::model::GAPPS_TERMS_URL.into()),
            );
        });
    }
    {
        let (ctx, now) = (ctx.clone(), now.clone());
        register.connect_activated(move |_| {
            let url = now.borrow().as_ref().map(|g| g.registration_url.clone());
            open_page(
                &ctx,
                &url.unwrap_or_else(|| crate::model::GAPPS_REGISTRATION_URL.into()),
            );
        });
    }
    request(
        ctx,
        |c| c.gapps(),
        move |r| show(tr("Where Google Play stands could not be read"), r),
    );
}

/// The protected content item: whether Widevine is installed, taking it from Google's image
/// with the user's agreement, removing it, and what L3 means for streaming.
fn widevine(ctx: &Shared, page: &Rc<Page>) {
    let group_ = group(
        &tr("Protected content"),
        Some(&tr("Widevine lets streaming apps play protected video. DSA does not include or pass on Google's Widevine: when you ask, your computer downloads the Android emulator image Google offers for this Android version, takes out only the Widevine module unchanged and deletes the rest.")),
    );
    let (status_row, status) = value_row(&tr("Status"), "");
    group_.add(&status_row);
    let busy_row = action_row(&view::widevine_busy_text(), "");
    busy_row.set_visible(false);
    group_.add(&busy_row);
    let restart = action_row(
        &tr("Restart Android to apply"),
        &tr(
            "Android started before this change. Stop and start Android for Widevine to follow it.",
        ),
    );
    restart.set_visible(false);
    group_.add(&restart);
    let quality = action_row(&tr("Standard definition only"), &view::widevine_quality());
    group_.add(&quality);
    let (install_row, install) = button_row(
        &tr("Get Widevine"),
        &tr("Downloads about 2 GB from Google, under Google's Android SDK License, and keeps only Widevine. Nothing is downloaded until you agree. Widevine is there from the next start of Android."),
        &tr("Get"),
    );
    install.add_css_class("suggested-action");
    group_.add(&install_row);
    let (remove_row, remove) = button_row(
        &tr("Remove Widevine"),
        &tr("Removes the module and the device certificates it kept, from the next start of Android."),
        &tr("Remove"),
    );
    remove.add_css_class("destructive-action");
    group_.add(&remove_row);
    let terms = link_row(
        &tr("Android SDK License"),
        &tr(
            "Widevine is Google's software and follows Google's terms. DSA does not distribute it.",
        ),
    );
    group_.add(&terms);
    page.add(&group_);

    let now: Rc<RefCell<Option<crate::model::Widevine>>> = Rc::default();
    let busy: Rc<Cell<bool>> = Rc::default();
    let buttons: Rc<dyn Fn()> = {
        let (now, busy) = (now.clone(), busy.clone());
        Rc::new(move || {
            let (now, busy) = (now.borrow(), busy.get());
            install_row.set_sensitive(now.as_ref().is_none_or(|w| w.can_install(busy)) && !busy);
            remove_row.set_sensitive(now.as_ref().is_some_and(|w| w.can_remove(busy)));
        })
    };
    let show: Rc<dyn Fn(String, Result<crate::model::Widevine, crate::api::Failure>)> = {
        let (page, now, buttons) = (page.clone(), now.clone(), buttons.clone());
        Rc::new(
            move |failed: String, r: Result<crate::model::Widevine, crate::api::Failure>| {
                page.show_failure(&failed, r.as_ref().err());
                if let Ok(widevine) = r {
                    status.set_label(&view::widevine_status(&widevine));
                    restart.set_visible(widevine.restart_needed);
                    *now.borrow_mut() = Some(widevine);
                }
                buttons();
            },
        )
    };
    {
        let (ctx, show, busy, buttons) = (ctx.clone(), show.clone(), busy.clone(), buttons.clone());
        install.connect_clicked(move |_| {
            let (asking, show, busy, buttons, busy_row) = (
                ctx.clone(),
                show.clone(),
                busy.clone(),
                buttons.clone(),
                busy_row.clone(),
            );
            confirm(
                &ctx,
                &tr("Get Widevine from Google?"),
                &view::widevine_notice(),
                &tr("Accept and download"),
                false,
                move || {
                    busy.set(true);
                    busy_row.set_visible(true);
                    buttons();
                    let (show, busy, busy_row) = (show.clone(), busy.clone(), busy_row.clone());
                    request(
                        &asking,
                        |c| c.install_widevine(),
                        move |r| {
                            busy.set(false);
                            busy_row.set_visible(false);
                            show(tr("Widevine was not installed"), r);
                        },
                    );
                },
            );
        });
    }
    {
        let (ctx, show, busy, buttons) = (ctx.clone(), show.clone(), busy.clone(), buttons.clone());
        remove.connect_clicked(move |_| {
            busy.set(true);
            buttons();
            let (show, busy) = (show.clone(), busy.clone());
            request(
                &ctx,
                |c| c.remove_widevine(),
                move |r| {
                    busy.set(false);
                    show(tr("Widevine was not removed"), r);
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        terms.connect_activated(move |_| open_page(&ctx, crate::model::WIDEVINE_TERMS_URL));
    }
    request(
        ctx,
        |c| c.widevine(),
        move |r| show(tr("Where Widevine stands could not be read"), r),
    );
}

/// Opens a web page in the user's browser, or says why it cannot.
fn open_page(ctx: &Shared, url: &str) {
    if let Err(error) =
        gtk::gio::AppInfo::launch_default_for_uri(url, None::<&gtk::gio::AppLaunchContext>)
    {
        tell(ctx, &tr("The page could not be opened"), error.message());
    }
}

fn apply(s: &Switches, now: &Settings) {
    s.file_sharing.set(now.file_sharing);
    for (folder, switch) in &s.folders {
        switch.set(now.shared_folders.iter().any(|f| f == folder));
        switch.row.set_sensitive(now.file_sharing);
    }
    s.links.set(now.links_in_android);
    s.clipboard.set(now.clipboard);
    s.arm.set(now.arm_translation);
    s.freeze.set(now.freeze);
    s.input_trace.set(now.input_trace);
    s.resize_trace.set(now.resize_trace);
    s.root_elevation.set(now.root_elevation);
    if s.root_at_start.get().is_none() {
        s.root_at_start.set(Some(now.root_elevation));
    }
    s.root_restart.set_visible(view::root_restart_needed(
        s.root_at_start.get(),
        now.root_elevation,
    ));
    s.root_log
        .set_subtitle(&view::root_log_text(now.log_dir.as_deref()));
}
