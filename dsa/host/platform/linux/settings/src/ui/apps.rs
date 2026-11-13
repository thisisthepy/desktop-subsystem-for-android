//! The pages about the apps in Android: Apps, Running in the background, and Widgets.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;

use super::common::*;
use crate::i18n::{tr, trf};
use crate::model::{App, AppWindow, DpiScaling};
use crate::view;

/// A callback set once its page is built, called to draw or reload that page again.
type Hook = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// The installed apps: open, App info, uninstall, and each app's window settings.
pub fn apps(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());

    let manage = group(
        "",
        Some(&tr("The Android apps on this computer. Open them, change them in App info, or uninstall the ones you installed.\nAndroid starts when you open this page.")),
    );
    let manage_row = link_row(
        &tr("Manage apps"),
        &tr("Open the list of apps in Android's own settings"),
    );
    manage.add(&manage_row);
    page.add(&manage);
    {
        let (ctx, page) = (ctx.clone(), page.clone());
        manage_row.connect_activated(move |_| {
            let page = page.clone();
            request(
                &ctx,
                |c| c.open_app_settings(None),
                move |r| {
                    page.show_failure(
                        &tr("The apps could not be read or changed"),
                        r.as_ref().err(),
                    );
                },
            );
        });
    }

    let installed = group(&tr("Installed apps"), None);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let search = gtk::SearchEntry::builder()
        .placeholder_text(tr("Search apps"))
        .valign(gtk::Align::Center)
        .build();
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some(&tr("Refresh")));
    refresh.add_css_class("flat");
    header.append(&search);
    header.append(&refresh);
    installed.set_header_suffix(Some(&header));
    let rows = Rows::new(installed);
    page.add(&rows.group);

    let apps: Rc<RefCell<Option<Vec<App>>>> = Rc::default();
    let windows: Rc<RefCell<HashMap<String, AppWindow>>> = Rc::default();
    // Whether Android scales an app the System (forced) way yet.
    let forced: Rc<Cell<bool>> = Rc::default();

    // Builds the list again from what is known, as the search says.
    let render: Hook = Rc::default();
    let reload: Hook = Rc::default();
    {
        let (ctx, page, rows, apps, windows) = (
            ctx.clone(),
            page.clone(),
            rows.clone(),
            apps.clone(),
            windows.clone(),
        );
        let search = search.clone();
        let reload = reload.clone();
        let forced = forced.clone();
        *render.borrow_mut() = Some(Rc::new(move || {
            rows.clear();
            let all = apps.borrow();
            let Some(all) = all.as_ref() else { return };
            let query = search.text().to_string();
            let shown = view::filter_apps(all, &query);
            rows.group.set_description(Some(&view::apps_count(
                all.len(),
                shown.len(),
                !query.trim().is_empty(),
            )));
            if all.is_empty() {
                rows.note(&tr(
                    "No app is installed in Android yet. Open an APK file to install one.",
                ));
            } else if shown.is_empty() {
                rows.note(&tr("No app matches your search."));
            }
            if view::root_unavailable(all) {
                rows.note(&tr("Root access is off for this computer, so no app can use su.\nAn administrator turns it on under Advanced. Then you choose each app's level here."));
            }
            for app in shown {
                let window = windows.borrow().get(&app.package).cloned();
                let group_note = view::root_group_note(app, all);
                let row = app_row(
                    &ctx,
                    &page,
                    app,
                    group_note,
                    window,
                    forced.get(),
                    reload.clone(),
                );
                // The app App info was opened for: its row opens and comes into view, once.
                let focused = ctx.focus_app.borrow().as_deref() == Some(app.package.as_str());
                if focused {
                    ctx.focus_app.replace(None);
                    row.set_expanded(true);
                    let shown_row = row.clone();
                    gtk::glib::idle_add_local_once(move || {
                        shown_row.grab_focus();
                    });
                }
                rows.add(&row);
            }
        }));
    }
    {
        let (ctx, page, apps, windows) = (ctx.clone(), page.clone(), apps.clone(), windows.clone());
        let render = render.clone();
        let forced = forced.clone();
        *reload.borrow_mut() = Some(Rc::new(move || {
            let (page, apps, render) = (page.clone(), apps.clone(), render.clone());
            let (windows, forced) = (windows.clone(), forced.clone());
            request(
                &ctx,
                |c| Ok((c.apps()?, c.app_window_list().unwrap_or_default())),
                move |r| {
                    page.show_failure(
                        &tr("The apps could not be read or changed"),
                        r.as_ref().err(),
                    );
                    if let Ok((list, settings)) = r {
                        *apps.borrow_mut() = Some(list);
                        forced.set(settings.system_forced_available);
                        *windows.borrow_mut() = settings
                            .apps
                            .into_iter()
                            .map(|w| (w.package.clone(), w))
                            .collect();
                    }
                    if let Some(render) = render.borrow().clone() {
                        render();
                    }
                },
            );
        }));
    }
    {
        let render = render.clone();
        search.connect_search_changed(move |_| {
            if let Some(render) = render.borrow().clone() {
                render();
            }
        });
    }
    let reload_now = reload.borrow().clone();
    if let Some(reload_now) = reload_now {
        let again = reload_now.clone();
        refresh.connect_clicked(move |_| again());
        reload_now();
    }
    page.root.clone().upcast()
}

fn app_row(
    ctx: &Shared,
    page: &Rc<Page>,
    app: &App,
    group_note: Option<String>,
    window: Option<AppWindow>,
    system_forced_available: bool,
    reload: Hook,
) -> adw::ExpanderRow {
    let row = expander_row(&app.label, &view::app_detail(app));
    row.add_prefix(&png_image(&app.icon_png, "application-x-executable", 32));

    let open = button(&tr("Open"));
    row.add_suffix(&open);
    {
        let (ctx, page, package) = (ctx.clone(), page.clone(), app.package.clone());
        open.connect_clicked(move |_| {
            let page = page.clone();
            let package = package.clone();
            request(
                &ctx,
                move |c| c.launch(&package),
                move |r| {
                    page.show_failure(
                        &tr("The apps could not be read or changed"),
                        r.as_ref().err(),
                    );
                },
            );
        });
    }

    let package_row = value_row(&tr("Package"), &app.package).0;
    row.add_row(&package_row);
    if app.first_install_time_ms > 0 {
        let date = gtk::glib::DateTime::from_unix_local((app.first_install_time_ms / 1000) as i64)
            .ok()
            .and_then(|d| d.format("%x").ok())
            .map(|d| d.to_string())
            .unwrap_or_default();
        row.add_row(&value_row(&tr("Installed"), &date).0);
    }

    let (info_row, info) = button_row(
        &tr("App info"),
        &tr("Permissions, storage and notifications, in Android's settings"),
        &tr("App info"),
    );
    row.add_row(&info_row);
    {
        let (ctx, page, package) = (ctx.clone(), page.clone(), app.package.clone());
        info.connect_clicked(move |_| {
            let page = page.clone();
            let package = package.clone();
            request(
                &ctx,
                move |c| c.open_app_settings(Some(&package)),
                move |r| {
                    page.show_failure(
                        &tr("The apps could not be read or changed"),
                        r.as_ref().err(),
                    );
                },
            );
        });
    }

    let (desktop_row, desktop) = button_row(
        &tr("Desktop shortcut"),
        &tr("A shortcut on the desktop that opens the app"),
        &tr("Create desktop shortcut"),
    );
    row.add_row(&desktop_row);
    {
        let (ctx, page, package) = (ctx.clone(), page.clone(), app.package.clone());
        let label = app.label.clone();
        desktop.connect_clicked(move |_| {
            let (page, package, ctx2) = (page.clone(), package.clone(), ctx.clone());
            let label = label.clone();
            request(
                &ctx,
                move |c| c.desktop_shortcut(&package),
                move |r| {
                    page.show_failure(
                        &tr("The desktop shortcut could not be made"),
                        r.as_ref().err(),
                    );
                    if r.is_ok() {
                        ctx2.toast(&trf("{0} is on the desktop.", &[&label]));
                    }
                },
            );
        });
    }

    // The window settings: whether the windows can be resized, their start size, and how they
    // follow a monitor of another scale.
    let fixed = window.as_ref().is_some_and(|w| w.fixed_size);
    let start = window.as_ref().and_then(|w| w.start_size);
    let scaling = window.as_ref().map(|w| w.dpi_scaling).unwrap_or_default();
    let state = Rc::new(RefCell::new((fixed, start, scaling)));
    let save = {
        let (ctx, page, package, state) = (
            ctx.clone(),
            page.clone(),
            app.package.clone(),
            state.clone(),
        );
        let label = app.label.clone();
        Rc::new(move || {
            let (fixed, start, scaling) = *state.borrow();
            let (page, package, ctx2) = (page.clone(), package.clone(), ctx.clone());
            let label = label.clone();
            request(
                &ctx,
                move |c| c.set_app_window(&package, fixed, start, scaling),
                move |r| {
                    page.show_failure(
                        &tr("The apps could not be read or changed"),
                        r.as_ref().err(),
                    );
                    if r.is_ok() {
                        ctx2.toast(&trf(
                            "{0}'s windows follow the new settings from the next one that opens.",
                            &[&label],
                        ));
                    }
                },
            );
        })
    };
    let resizable = {
        let (state, save) = (state.clone(), save.clone());
        Switch::new(
            &tr("Resizable"),
            &tr("Whether you can resize and maximize this app's windows. Off keeps each window at the size it opens at."),
            move |on| {
                state.borrow_mut().0 = !on;
                save();
            },
        )
    };
    resizable.set(!fixed);
    row.add_row(&resizable.row);
    let choices = view::start_size_choices();
    let size = {
        let (state, save) = (state.clone(), save.clone());
        Combo::new(
            &tr("Window size when it opens"),
            &tr("The size this app's windows open at"),
            move |index| {
                if let Some((_, size)) = view::start_size_choices().get(index as usize) {
                    state.borrow_mut().1 = *size;
                    save();
                }
            },
        )
    };
    let mut labels: Vec<String> = choices.iter().map(|(l, _)| l.clone()).collect();
    let index = match view::start_size_index(window.as_ref()) {
        Some(index) => index,
        None => {
            let (w, h) = start.unwrap_or_default();
            labels.push(format!("{w} × {h}"));
            labels.len() - 1
        }
    };
    size.set(&labels, index as u32);
    row.add_row(&size.row);
    // Auto and System never restart the app; System (forced) is marked while Android cannot do it.
    let dpi_choices = view::dpi_scaling_choices(system_forced_available);
    let dpi = {
        let state = state.clone();
        Combo::new(
            &tr("Scaling on another monitor"),
            &view::dpi_scaling_subtitle(window.as_ref(), system_forced_available),
            move |index| {
                if let Some(scaling) = DpiScaling::ALL.get(index as usize) {
                    if state.borrow().2 != *scaling {
                        state.borrow_mut().2 = *scaling;
                        save();
                    }
                }
            },
        )
    };
    let dpi_labels: Vec<String> = dpi_choices.into_iter().map(|(label, _)| label).collect();
    dpi.set(&dpi_labels, view::dpi_scaling_index(window.as_ref()) as u32);
    row.add_row(&dpi.row);

    // Whether the app runs its ARM code through the translator.
    let arm = view::run_as_arm_view(app);
    let run_as_arm = {
        let (ctx, page, package, label) = (
            ctx.clone(),
            page.clone(),
            app.package.clone(),
            app.label.clone(),
        );
        let reload = reload.clone();
        Switch::new(&tr("Run as ARM"), &arm.subtitle, move |on| {
            switch_abi(&ctx, &page, &package, &label, on, false, reload.clone());
        })
    };
    run_as_arm.set(arm.on);
    run_as_arm.row.set_sensitive(arm.sensitive);
    row.add_row(&run_as_arm.row);

    // How far the app may use root: not at all, asking an administrator each time, or without
    // asking.
    let root = view::root_view(app);
    // Apps that share a user ID are one group for root: the row says so, and with which apps.
    let root_subtitle = match &group_note {
        Some(note) => format!(
            "{}
{note}",
            root.subtitle
        ),
        None => root.subtitle.clone(),
    };
    let root_level = {
        let (ctx, page, package, label) = (
            ctx.clone(),
            page.clone(),
            app.package.clone(),
            app.label.clone(),
        );
        let reload = reload.clone();
        let current = app.root.clone();
        Combo::new(&tr("Root access"), &root_subtitle, move |index| {
            let Some(level) = view::ROOT_LEVELS.get(index as usize) else {
                return;
            };
            if *level == current {
                return;
            }
            let (ctx2, page, label) = (ctx.clone(), page.clone(), label.clone());
            let (package, level) = (package.clone(), level.to_string());
            let reload = reload.clone();
            request(
                &ctx,
                move |c| c.set_app_root(&package, &level),
                move |r| {
                    page.show_failure(
                        &tr("The apps could not be read or changed"),
                        r.as_ref().err(),
                    );
                    if let Ok(outcome) = &r {
                        match view::root_next(&label, outcome) {
                            Ok(toast) => ctx2.toast(&toast),
                            Err(why) => tell(&ctx2, &tr("Not changed"), &why),
                        }
                    }
                    // The list shows the level the service keeps, also after a refusal.
                    if let Some(reload) = reload.borrow().clone() {
                        reload();
                    }
                },
            );
        })
    };
    let mut root_labels = view::root_choices();
    let root_index = match root.index {
        Some(index) => index,
        None => {
            root_labels.push(tr("Not available"));
            root_labels.len() - 1
        }
    };
    root_level.set(&root_labels, root_index as u32);
    root_level.row.set_sensitive(root.sensitive);
    row.add_row(&root_level.row);

    let (uninstall_row, uninstall) = button_row(&tr("Uninstall"), "", &tr("Uninstall"));
    if app.system_app {
        uninstall_row.set_subtitle(&tr(
            "This app is part of Android and cannot be uninstalled.",
        ));
    }
    uninstall.add_css_class("destructive-action");
    uninstall.set_sensitive(!app.system_app);
    row.add_row(&uninstall_row);
    {
        let (ctx, page) = (ctx.clone(), page.clone());
        let (package, label) = (app.package.clone(), app.label.clone());
        uninstall.connect_clicked(move |_| {
            let (ctx2, page, package, label) =
                (ctx.clone(), page.clone(), package.clone(), label.clone());
            let reload = reload.clone();
            confirm(
                &ctx,
                &trf("Uninstall {0}?", &[&label]),
                &trf(
                    "{0} and its data will be removed from Android. This cannot be undone.",
                    &[&label],
                ),
                &tr("Uninstall"),
                true,
                move || {
                    let (ctx3, page, label) = (ctx2.clone(), page.clone(), label.clone());
                    let package = package.clone();
                    let reload = reload.clone();
                    request(
                        &ctx2,
                        move |c| c.uninstall(&package),
                        move |r| {
                            page.show_failure(
                                &tr("The apps could not be read or changed"),
                                r.as_ref().err(),
                            );
                            match r {
                                Ok(outcome) if outcome.done => {
                                    ctx3.toast(&trf("{0} is uninstalled.", &[&label]))
                                }
                                Ok(outcome) => tell(
                                    &ctx3,
                                    &tr("Not uninstalled"),
                                    &trf(
                                        "{0} was not uninstalled: {1} ({2})",
                                        &[
                                            &label,
                                            &view::uninstall_reason(&outcome.status),
                                            &outcome.detail,
                                        ],
                                    ),
                                ),
                                Err(_) => {}
                            }
                            if let Some(reload) = reload.borrow().clone() {
                                reload();
                            }
                        },
                    );
                },
            );
        });
    }
    row
}

/// Installs `package` again with its ARM code (`on`) or its x86_64 code. When Android cannot
/// keep the app's data, the user is asked first, and only then is it erased.
fn switch_abi(
    ctx: &Shared,
    page: &Rc<Page>,
    package: &str,
    label: &str,
    on: bool,
    erase_data: bool,
    reload: Hook,
) {
    let (ctx2, page, package, label) = (
        ctx.clone(),
        page.clone(),
        package.to_string(),
        label.to_string(),
    );
    let asked = package.clone();
    request(
        ctx,
        move |c| c.run_as_arm(&asked, on, erase_data),
        move |r| {
            page.show_failure(
                &tr("The apps could not be read or changed"),
                r.as_ref().err(),
            );
            if let Ok(outcome) = &r {
                match view::run_as_arm_next(&label, on, outcome) {
                    view::RunAsArmNext::Done(toast) => ctx2.toast(&toast),
                    view::RunAsArmNext::ConfirmErase => {
                        // The switch shows how the app runs until the user decides.
                        if let Some(reload) = reload.borrow().clone() {
                            reload();
                        }
                        let (ctx3, page, package, label) =
                            (ctx2.clone(), page.clone(), package.clone(), label.clone());
                        let reload = reload.clone();
                        confirm(
                            &ctx2,
                            &trf("Erase {0}'s data?", &[&label]),
                            &trf(
                                "Android cannot change how {0} runs and keep its data.\n\nInstalling it again erases its data: sign-ins, settings and files it keeps for itself.",
                                &[&label],
                            ),
                            &tr("Erase and install again"),
                            true,
                            move || {
                                switch_abi(&ctx3, &page, &package, &label, on, true, reload.clone())
                            },
                        );
                        return;
                    }
                    view::RunAsArmNext::Refused(why) => {
                        tell(&ctx2, &tr("Not installed again"), &why)
                    }
                }
            }
            if let Some(reload) = reload.borrow().clone() {
                reload();
            }
        },
    );
}

/// The apps that keep running with no window, with their media, and stopping them.
pub fn background(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());
    let intro = group(
        &tr("Apps"),
        Some(&tr("These Android apps keep running after their windows close, to play music, download files, keep a VPN on or do other work.\nEach also has an indicator in the top bar.\nStopping an app ends what it is doing and frees the memory it used.")),
    );
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some(&tr("Refresh")));
    refresh.add_css_class("flat");
    intro.set_header_suffix(Some(&refresh));
    let rows = Rows::new(intro);
    page.add(&rows.group);

    let stopping = group(&tr("Stopping apps"), None);
    let ask = {
        let (ctx, page) = (ctx.clone(), page.clone());
        Switch::new(
            &tr("Ask before stopping an app from its indicator"),
            &tr("Turn this back on if you chose not to be asked again."),
            move |on| {
                let page = page.clone();
                request(
                    &ctx,
                    move |c| c.switch("stop-without-asking", !on),
                    move |r| {
                        page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                    },
                );
            },
        )
    };
    stopping.add(&ask.row);
    page.add(&stopping);

    let reload: Hook = Rc::default();
    {
        let (ctx, page, rows, slot) = (ctx.clone(), page.clone(), rows.clone(), reload.clone());
        *reload.borrow_mut() = Some(Rc::new(move || {
            let (ctx2, page, rows, slot) = (ctx.clone(), page.clone(), rows.clone(), slot.clone());
            request(
                &ctx,
                |c| c.background(),
                move |r| {
                    page.show_failure(
                        &tr("The apps that run in the background could not be read or changed"),
                        r.as_ref().err(),
                    );
                    rows.clear();
                    let Ok(list) = r else { return };
                    if list.apps.is_empty() {
                        rows.note(&tr("No app runs in the background."));
                    }
                    for app in &list.apps {
                        let media = list.media.iter().find(|m| m.package == app.package);
                        let detail = match media {
                            Some(m) if m.playback == "playing" => {
                                trf("Playing {0}", &[&media_title(&m.title, &m.artist)])
                            }
                            Some(m) => trf("Paused: {0}", &[&media_title(&m.title, &m.artist)]),
                            None => [app.title.as_str(), app.text.as_str()]
                                .into_iter()
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                                .join(" · "),
                        };
                        let row = action_row(&app.label, &detail);
                        row.add_prefix(&png_image(&app.icon_png, "application-x-executable", 32));
                        let open = button(&tr("Open"));
                        let stop = button(&tr("Stop"));
                        stop.add_css_class("destructive-action");
                        row.add_suffix(&open);
                        row.add_suffix(&stop);
                        let run = |action: &'static str| {
                            let (ctx, page, package) =
                                (ctx2.clone(), page.clone(), app.package.clone());
                            let (label, slot) = (app.label.clone(), slot.clone());
                            move |_: &gtk::Button| {
                                let (ctx3, page, label) =
                                    (ctx.clone(), page.clone(), label.clone());
                                let package = package.clone();
                                let slot = slot.clone();
                                request(
                                    &ctx,
                                    move |c| match action {
                                        "open" => c.open_background(&package),
                                        _ => c.stop_background(&package),
                                    },
                                    move |r| {
                                        page.show_failure(
                                        &tr("The apps that run in the background could not be read or changed"),
                                        r.as_ref().err(),
                                    );
                                        if r.is_ok() && action == "stop" {
                                            ctx3.toast(&trf("{0} was stopped.", &[&label]));
                                        }
                                        if let Some(reload) = slot.borrow().clone() {
                                            reload();
                                        }
                                    },
                                );
                            }
                        };
                        open.connect_clicked(run("open"));
                        stop.connect_clicked(run("stop"));
                        rows.add(&row);
                    }
                },
            );
        }));
    }
    {
        let ask = ask.clone();
        let page = page.clone();
        request(
            ctx,
            |c| c.settings(),
            move |r| {
                page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                if let Ok(settings) = r {
                    ask.set(!settings.stop_without_asking);
                }
            },
        );
    }
    let reload_now = reload.borrow().clone();
    if let Some(reload_now) = reload_now {
        let again = reload_now.clone();
        refresh.connect_clicked(move |_| again());
        reload_now();
    }
    page.root.clone().upcast()
}

fn media_title(title: &str, artist: &str) -> String {
    if artist.is_empty() {
        title.to_string()
    } else {
        format!("{title} · {artist}")
    }
}

/// The widgets on the desktop and the ones the apps offer.
pub fn widgets(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());
    let placed = group(
        &tr("On your desktop"),
        Some(&tr("Place the widgets your Android apps offer on your desktop. Each widget gets a window of its own.\nDrag the band along its top to move it, and its edges to resize it.\nRight-click a widget to resize or remove it. Android starts when you open this page.")),
    );
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some(&tr("Refresh")));
    refresh.add_css_class("flat");
    placed.set_header_suffix(Some(&refresh));
    let placed = Rows::new(placed);
    page.add(&placed.group);
    let offers = Rows::new(group(&tr("Widgets your apps offer"), None));
    page.add(&offers.group);

    let reload: Hook = Rc::default();
    {
        let (ctx, page, slot) = (ctx.clone(), page.clone(), reload.clone());
        let (placed, offers) = (placed.clone(), offers.clone());
        *reload.borrow_mut() = Some(Rc::new(move || {
            let (ctx2, page, slot) = (ctx.clone(), page.clone(), slot.clone());
            let (placed, offers) = (placed.clone(), offers.clone());
            request(
                &ctx,
                |c| c.widgets(),
                move |r| {
                    let failed = &tr("Widgets could not be read or changed");
                    page.show_failure(failed, r.as_ref().err());
                    placed.clear();
                    offers.clear();
                    let Ok(list) = r else { return };
                    if list.placed.is_empty() {
                        placed.note(&tr("No widget is on your desktop yet."));
                    }
                    if list.offers.is_empty() {
                        offers.note(&tr("No app in Android offers a widget."));
                    }
                    let after = {
                        let (ctx, page, slot) = (ctx2.clone(), page.clone(), slot.clone());
                        move |done: String| {
                            let (ctx, page, slot) = (ctx.clone(), page.clone(), slot.clone());
                            move |r: Result<(), crate::api::Failure>| {
                                page.show_failure(
                                    &tr("Widgets could not be read or changed"),
                                    r.as_ref().err(),
                                );
                                if r.is_ok() {
                                    ctx.toast(&done);
                                }
                                if let Some(reload) = slot.borrow().clone() {
                                    reload();
                                }
                            }
                        }
                    };
                    for widget in &list.placed {
                        let detail = trf(
                            "At {0}, {1}, {2} × {3}",
                            &[
                                &widget.x.to_string(),
                                &widget.y.to_string(),
                                &widget.width_dp.to_string(),
                                &widget.height_dp.to_string(),
                            ],
                        );
                        let row = action_row(&widget.label, &format!("{} · {detail}", widget.app));
                        let remove = button(&tr("Remove"));
                        row.add_suffix(&remove);
                        let (ctx, id) = (ctx2.clone(), widget.id);
                        let done = trf("{0} was removed from your desktop.", &[&widget.label]);
                        let after = after.clone();
                        remove.connect_clicked(move |_| {
                            request(&ctx, move |c| c.remove_widget(id), after(done.clone()));
                        });
                        placed.add(&row);
                    }
                    for offer in &list.offers {
                        let mut detail = vec![
                            offer.app.clone(),
                            trf(
                                "{0} × {1}",
                                &[&offer.width_dp.to_string(), &offer.height_dp.to_string()],
                            ),
                        ];
                        if offer.needs_configuration {
                            detail.push(tr("asks you to set it up"));
                        }
                        let row = action_row(&offer.label, &detail.join(" · "));
                        row.add_prefix(&png_image(&offer.preview_png, "view-grid-symbolic", 48));
                        let add = button(&tr("Add to desktop"));
                        row.add_suffix(&add);
                        let (ctx, provider) = (ctx2.clone(), offer.provider.clone());
                        let done = trf("{0} is on your desktop.", &[&offer.label]);
                        let after = after.clone();
                        add.connect_clicked(move |_| {
                            let provider = provider.clone();
                            request(
                                &ctx,
                                move |c| c.add_widget(&provider).map(|_| ()),
                                after(done.clone()),
                            );
                        });
                        offers.add(&row);
                    }
                },
            );
        }));
    }
    let reload_now = reload.borrow().clone();
    if let Some(reload_now) = reload_now {
        let again = reload_now.clone();
        refresh.connect_clicked(move |_| again());
        reload_now();
    }
    page.root.clone().upcast()
}
