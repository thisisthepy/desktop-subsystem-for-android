//! The pages about Android itself: General, Memory, Display and GPU, Android version,
//! Updates and About.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use super::common::*;
use crate::i18n::{tr, trf};
use crate::model::{AndroidVersion, Settings, Status};
use crate::view;

/// Start and stop, the state and its last error, freezing now, and the log folders.
pub fn general(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());

    let state = group(
        &tr("State"),
        Some(&tr("Desktop Subsystem for Android™ runs Android in its own lightweight virtual machine and shows each app in its own window.")),
    );
    let (state_row, state_label) = value_row(&tr("Android"), "");
    let start = button(&tr("Start"));
    start.add_css_class("suggested-action");
    let stop = button(&tr("Stop"));
    state_row.add_suffix(&start);
    state_row.add_suffix(&stop);
    state.add(&state_row);
    let (problem_row, problem_label) = value_row(&tr("Android is not ready"), "");
    let (reason_row, reason_label) = value_row(&tr("Last stopped"), "");
    let (notice_row, notice_label) = value_row(&tr("Update"), "");
    for row in [&problem_row, &reason_row, &notice_row] {
        row.set_visible(false);
        state.add(row);
    }
    page.add(&state);

    let more = group(&tr("More"), None);
    let (freeze_row, freeze) = button_row(
        &tr("Freeze Android now"),
        &tr("Stops every process in Android until you open one of its windows again. Notifications, alarms and widgets stop with it."),
        &tr("Freeze"),
    );
    more.add(&freeze_row);
    let logs = link_row(
        &tr("Open my log folder"),
        &tr("The logs of the window manager and the app windows, for reporting a problem."),
    );
    more.add(&logs);
    let service_logs = link_row(
        &tr("Open the service's log folder (administrators)"),
        &tr("The logs of the DSA service and of Android itself."),
    );
    more.add(&service_logs);
    let (refresh_row, refresh) = button_row(
        &tr("Refresh"),
        &tr("Ask the DSA service for the state again."),
        &tr("Refresh"),
    );
    more.add(&refresh_row);
    page.add(&more);

    let log_dir: Rc<RefCell<Option<String>>> = Rc::default();
    let show: Rc<dyn Fn(Result<Status, crate::api::Failure>)> = {
        let page = page.clone();
        let (start, stop, freeze) = (start.clone(), stop.clone(), freeze.clone());
        Rc::new(move |result: Result<Status, crate::api::Failure>| {
            let status = result.as_ref().ok();
            page.show_failure(&tr("Something went wrong"), result.as_ref().err());
            let controls = view::controls(status);
            start.set_sensitive(controls.can_start);
            stop.set_sensitive(controls.can_stop);
            freeze.set_sensitive(controls.can_freeze);
            state_label.set_label(&view::state_text(
                status
                    .map(|s| s.state)
                    .unwrap_or(crate::model::State::Unknown),
            ));
            for (row, label, value) in [
                (
                    &problem_row,
                    &problem_label,
                    status.and_then(|s| s.problem.clone()),
                ),
                (
                    &reason_row,
                    &reason_label,
                    status.and_then(|s| s.last_reason.clone()),
                ),
                (
                    &notice_row,
                    &notice_label,
                    status.and_then(|s| s.update_notice.clone()),
                ),
            ] {
                row.set_visible(value.is_some());
                label.set_label(value.as_deref().unwrap_or_default());
            }
        })
    };
    let busy = {
        let (start, stop, freeze) = (start.clone(), stop.clone(), freeze.clone());
        move || {
            start.set_sensitive(false);
            stop.set_sensitive(false);
            freeze.set_sensitive(false);
        }
    };
    let refresh_now = {
        let (ctx, show, log_dir) = (ctx.clone(), show.clone(), log_dir.clone());
        Rc::new(move || {
            let show = show.clone();
            request(&ctx, |c| c.status(), move |r| show(r));
            let log_dir = log_dir.clone();
            request(
                &ctx,
                |c| c.settings(),
                move |r: Result<Settings, _>| {
                    *log_dir.borrow_mut() = r.ok().and_then(|s| s.log_dir);
                },
            );
        })
    };
    {
        let (ctx, show, busy) = (ctx.clone(), show.clone(), busy.clone());
        start.connect_clicked(move |_| {
            busy();
            let show = show.clone();
            request(&ctx, |c| c.start(), move |r| show(r));
        });
    }
    {
        let (ctx, show, busy) = (ctx.clone(), show.clone(), busy.clone());
        stop.connect_clicked(move |_| {
            busy();
            let show = show.clone();
            request(&ctx, |c| c.stop(), move |r| show(r));
        });
    }
    {
        let (ctx, show) = (ctx.clone(), show.clone());
        freeze.connect_clicked(move |_| {
            busy();
            let show = show.clone();
            request(&ctx, |c| c.freeze(), move |r| show(r));
        });
    }
    {
        let ctx = ctx.clone();
        logs.connect_activated(move |_| open_folder(&ctx, &view::user_log_folder()));
    }
    {
        let ctx = ctx.clone();
        service_logs.connect_activated(move |_| {
            open_folder(&ctx, &view::service_log_folder(log_dir.borrow().as_deref()))
        });
    }
    {
        let refresh_now = refresh_now.clone();
        refresh.connect_clicked(move |_| refresh_now());
    }
    refresh_now();
    page.root.clone().upcast()
}

/// What the VM takes on this computer and Android's own breakdown, and the memory limit.
pub fn memory(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());

    let host = group(&tr("On this computer"), None);
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some(&tr("Refresh")));
    refresh.add_css_class("flat");
    host.set_header_suffix(Some(&refresh));
    let (total_row, total_label) = value_row(&tr("In use on this computer"), "");
    total_row.set_subtitle(&tr(
        "The RAM the virtual machine, the window manager and the app windows hold now.",
    ));
    host.add(&total_row);
    let (vm_row, vm_label) = value_row(&tr("Android's VM now"), "");
    vm_row.set_subtitle(&tr(
        "The memory the VM holds on this computer, and how much of it Android uses",
    ));
    host.add(&vm_row);
    let host_rows = Rows::new(host);
    page.add(&host_rows.group);

    let guest = group(&tr("Inside Android"), None);
    let (available_row, available_label) = value_row(&tr("Available to Android"), "");
    let level = gtk::LevelBar::for_interval(0.0, 1.0);
    level.set_hexpand(true);
    level.set_valign(gtk::Align::Center);
    level.set_width_request(160);
    available_row.add_suffix(&level);
    guest.add(&available_row);
    let guest_rows = Rows::new(guest);
    page.add(&guest_rows.group);
    let kernel_rows = Rows::new(group(&tr("Android's kernel"), None));
    page.add(&kernel_rows.group);
    let process_rows = Rows::new(group(&tr("Largest processes"), None));
    page.add(&process_rows.group);

    let limit_group = group("", None);
    let current_limit = Rc::new(std::cell::Cell::new(0u32));
    let limit = {
        let (ctx, page, current_limit) = (ctx.clone(), page.clone(), current_limit.clone());
        Combo::new(
            &tr("Memory limit"),
            &tr("The most memory Android may grow to. Android takes only what it needs and returns the rest at once.\nAutomatic sets no fixed limit: up to this computer's memory less what Linux keeps.\nTakes effect the next time Android starts."),
            move |index| {
                let choices = view::memory_limit_choices(current_limit.get());
                let Some((_, mb)) = choices.get(index as usize).cloned() else {
                    return;
                };
                let page = page.clone();
                let current_limit = current_limit.clone();
                request(&ctx, move |c| c.set_memory_limit(mb), move |r| {
                    page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                    if let Ok(settings) = r {
                        current_limit.set(settings.memory_limit_mb);
                    }
                });
            },
        )
    };
    limit_group.add(&limit.row);
    page.add(&limit_group);

    let refresh_now = {
        let ctx = ctx.clone();
        let page = page.clone();
        Rc::new(move || {
            let page = page.clone();
            let (host_rows, guest_rows) = (host_rows.clone(), guest_rows.clone());
            let (kernel_rows, process_rows) = (kernel_rows.clone(), process_rows.clone());
            let (total_label, vm_label) = (total_label.clone(), vm_label.clone());
            let (available_label, level) = (available_label.clone(), level.clone());
            let available_row = available_row.clone();
            request(
                &ctx,
                |c| c.memory(),
                move |r| {
                    page.show_failure(&tr("The memory could not be measured"), r.as_ref().err());
                    host_rows.clear();
                    guest_rows.clear();
                    kernel_rows.clear();
                    process_rows.clear();
                    let Ok(memory) = r else { return };
                    total_label.set_label(&view::size_kb(memory.host_total_kb));
                    vm_label.set_label(
                        &match (view::vm_size_kb(&memory), memory.vm_beyond_guest_kb) {
                            (Some(vm), Some(beyond)) => trf(
                                "{0}: used by Android {1}, free, not yet returned {2}",
                                &[
                                    &view::size_kb(vm),
                                    &view::size_kb(vm.saturating_sub(beyond)),
                                    &view::size_kb(beyond),
                                ],
                            ),
                            (Some(vm), None) => view::size_kb(vm),
                            (None, _) => String::new(),
                        },
                    );
                    if memory.host.is_empty() {
                        host_rows.note(&tr(
                            "Android is not running, so it takes no memory on this computer.",
                        ));
                    }
                    for row in view::host_rows(&memory) {
                        host_rows.add(&memory_row(&row));
                    }
                    match &memory.guest {
                        None => {
                            available_row.set_visible(false);
                            guest_rows.note(&tr("No report yet. Android reports how it uses its memory a few minutes after it starts."));
                        }
                        Some(guest) => {
                            available_row.set_visible(true);
                            available_row.set_subtitle(&trf(
                                "Reported {0} seconds ago",
                                &[&guest.age_seconds.to_string()],
                            ));
                            available_label.set_label(&trf(
                                "{0} available of {1}",
                                &[
                                    &view::size_kb(guest.available_kb),
                                    &view::size_kb(guest.total_kb),
                                ],
                            ));
                            level.set_value(view::guest_used_fraction(guest));
                            for row in view::guest_rows(guest) {
                                guest_rows.add(&memory_row(&row));
                            }
                            for row in view::kernel_rows(guest) {
                                kernel_rows.add(&memory_row(&row));
                            }
                            for row in view::process_rows(guest) {
                                process_rows.add(&memory_row(&row));
                            }
                        }
                    }
                    kernel_rows
                        .group
                        .set_visible(memory.guest.as_ref().is_some_and(|g| g.kernel.is_some()));
                    process_rows.group.set_visible(memory.guest.is_some());
                },
            );
            let limit = limit.clone();
            let current_limit = current_limit.clone();
            request(
                &ctx,
                |c| c.settings(),
                move |r| {
                    if let Ok(settings) = r {
                        current_limit.set(settings.memory_limit_mb);
                        let choices = view::memory_limit_choices(settings.memory_limit_mb);
                        let index = choices
                            .iter()
                            .position(|(_, mb)| *mb == settings.memory_limit_mb)
                            .unwrap_or(0);
                        let labels: Vec<String> = choices.into_iter().map(|(l, _)| l).collect();
                        limit.set(&labels, index as u32);
                    }
                },
            );
        })
    };
    {
        let refresh_now = refresh_now.clone();
        refresh.connect_clicked(move |_| refresh_now());
    }
    refresh_now();
    page.root.clone().upcast()
}

fn memory_row(row: &view::MemoryRow) -> adw::ActionRow {
    let (action, _) = value_row(&row.label, &row.size);
    if !row.detail.is_empty() {
        action.set_subtitle(&row.detail);
    }
    action
}

/// The render GPU and restarting the window manager.
pub fn display(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());
    let gpu = group(&tr("Render GPU"), None);
    let setting = Rc::new(RefCell::new(String::new()));
    let combo = {
        let (ctx, page, setting) = (ctx.clone(), page.clone(), setting.clone());
        Combo::new(
            &tr("Render GPU"),
            &tr("The GPU that draws Android apps.\nThe default, the primary monitor's GPU, keeps frames on one GPU and lets a laptop's discrete GPU sleep."),
            move |index| {
                let choices = view::render_gpu_choices();
                let value = match choices.get(index as usize) {
                    Some((value, _)) => value.to_string(),
                    None => setting.borrow().clone(),
                };
                let page = page.clone();
                let ctx2 = ctx.clone();
                request(&ctx, move |c| c.set("render-gpu", &value), move |r| {
                    page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                    if r.is_ok() {
                        ctx2.toast(&tr("A new render GPU takes effect when the window manager starts again: at the next sign-in, or when you restart it below."));
                    }
                });
            },
        )
    };
    gpu.add(&combo.row);
    let (display_row, display_label) = value_row(&tr("GPU of the primary monitor"), "");
    display_row.set_subtitle(&tr("What the default choice renders on now."));
    gpu.add(&display_row);
    page.add(&gpu);

    let wm = group("", None);
    let (restart_row, restart) = button_row(
        &tr("Restart the window manager"),
        &tr("Open Android app windows close and come back."),
        &tr("Restart"),
    );
    wm.add(&restart_row);
    page.add(&wm);
    {
        let ctx = ctx.clone();
        restart.connect_clicked(move |_| {
            let ctx2 = ctx.clone();
            confirm(
                &ctx,
                &tr("Restart the window manager?"),
                &tr("The windows of Android apps close for a moment. Android and its apps keep running."),
                &tr("Restart"),
                false,
                move || {
                    if let Err(why) = start_program(&view::restart_window_manager_command()) {
                        tell(&ctx2, &tr("The window manager could not be restarted"), &why);
                    }
                },
            );
        });
    }

    {
        let page = page.clone();
        request(
            ctx,
            |c| c.settings(),
            move |r| {
                page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                if let Ok(settings) = r {
                    let mut labels: Vec<String> = view::render_gpu_choices()
                        .into_iter()
                        .map(|(_, l)| l)
                        .collect();
                    let index = view::render_gpu_index(&settings.render_gpu);
                    if index == labels.len() {
                        labels.push(settings.render_gpu.clone());
                    }
                    *setting.borrow_mut() = settings.render_gpu;
                    combo.set(&labels, index as u32);
                }
            },
        );
    }
    request(
        ctx,
        |c| c.gpus(),
        move |r| {
            let name = r
                .ok()
                .and_then(|gpus| gpus.into_iter().find(|(_, primary)| *primary))
                .map(|(name, _)| name)
                .unwrap_or_else(|| tr("Chosen by the window manager when it starts"));
            display_label.set_label(&name);
        },
    );
    page.root.clone().upcast()
}

/// The Android part of the Updates page: the version that runs, the version it follows, where
/// its image update stands (with the download's progress and a check now), and the supported
/// versions to choose from. `image` is the automatic image updates group, shown between them.
fn android_groups(ctx: &Shared, page: &Rc<Page>, image: &adw::PreferencesGroup) {
    let page = page.clone();
    let running = group(&tr("Android"), None);
    let (version_row, version_label) = value_row(&tr("Android"), "");
    let (build_row, build_label) = value_row(&tr("Build"), "");
    let (patch_row, patch_label) = value_row(&tr("Security patch level"), "");
    let (follows_row, follows_label) = value_row(&tr("Version it follows"), "");
    for row in [&version_row, &build_row, &patch_row, &follows_row] {
        running.add(row);
    }
    let update_row = action_row(&tr("Android update"), "");
    update_row.set_subtitle_lines(0);
    let progress = gtk::ProgressBar::builder()
        .valign(gtk::Align::Center)
        .width_request(120)
        .visible(false)
        .build();
    update_row.add_suffix(&progress);
    // Shown while a downloaded image waits for this user's Android to restart.
    let restart = button(&tr("Restart now"));
    restart.add_css_class("suggested-action");
    restart.set_visible(false);
    update_row.add_suffix(&restart);
    let restart_button = restart.clone();
    let check = button(&tr("Check now"));
    update_row.add_suffix(&check);
    running.add(&update_row);
    // The user's own switch: restarting into a downloaded image by itself.
    let auto_restart = {
        let (ctx, page) = (ctx.clone(), page.clone());
        Switch::new(
            &tr("Restart Android into updates by itself"),
            &tr("On by default. With no app window open, Android restarts into a downloaded update at once.\nWith windows open, DSA asks first or waits until you close them.\nOff, an update is used the next time you start Android."),
            move |on| {
                let page = page.clone();
                request(
                    &ctx,
                    move |c| c.switch("image-auto-restart", on),
                    move |r| {
                        page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                    },
                );
            },
        )
    };
    running.add(&auto_restart.row);
    {
        let auto_restart = auto_restart.clone();
        request(
            ctx,
            |c| c.settings(),
            move |r| {
                if let Ok(settings) = r {
                    auto_restart.set(settings.image_auto_restart);
                }
            },
        );
    }
    page.add(&running);
    page.add(image);

    let choose = group(
        &tr("Android version"),
        Some(&tr("Every user of this computer runs the same Android version, so changing it needs an administrator.\nThe chosen version keeps getting security updates.\nWhen its support ends, DSA moves to the next version and keeps your apps.")),
    );
    let info: Rc<RefCell<Option<AndroidVersion>>> = Rc::default();
    let picked = Rc::new(std::cell::Cell::new(0u32));
    let combo = {
        let picked = picked.clone();
        Combo::new(
            &tr("Version to use"),
            &tr("DSA supports the latest two Android versions. Some apps run on only one of them."),
            move |index| picked.set(index),
        )
    };
    let apply = button(&tr("Change"));
    apply.add_css_class("suggested-action");
    combo.row.add_suffix(&apply);
    choose.add(&combo.row);
    page.add(&choose);

    // The row the refresh below follows; the closure that shows a version takes the row itself.
    let watched_row = update_row.downgrade();
    let show: Rc<dyn Fn(Result<AndroidVersion, crate::api::Failure>)> = {
        let (page, info, combo, picked) =
            (page.clone(), info.clone(), combo.clone(), picked.clone());
        Rc::new(move |r: Result<AndroidVersion, crate::api::Failure>| {
            page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
            let Ok(version) = r else { return };
            version_label.set_label(&trf("Android {0}", &[&version.major.to_string()]));
            version_row.set_subtitle(&version.version);
            build_label.set_label(&version.build_id.clone().unwrap_or_else(|| tr("Unknown")));
            patch_label.set_label(
                &version
                    .security_patch
                    .clone()
                    .unwrap_or_else(|| tr("Unknown")),
            );
            follows_label.set_label(&view::android_channel_text(&version));
            update_row.set_subtitle(&view::android_update_text(&version));
            restart.set_visible(version.restart_pending.is_some());
            match view::android_download_percent(version.notice.as_deref()) {
                Some(percent) => {
                    progress.set_fraction(f64::from(percent) / 100.0);
                    progress.set_visible(true);
                }
                None => progress.set_visible(false),
            }
            let choices = view::android_choices(&version);
            let labels: Vec<String> = choices.into_iter().map(|c| c.label).collect();
            let index = view::android_choice_index(&version) as u32;
            combo.set(&labels, index);
            combo.row.set_subtitle(&trf(
                "Supported versions: Android {0}",
                &[&version
                    .supported
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")],
            ));
            picked.set(index);
            *info.borrow_mut() = Some(version);
        })
    };
    {
        // Choosing the version already followed checks the signed image list at once and
        // changes nothing else. The image is every user's, so an administrator asks.
        let (ctx, show, info) = (ctx.clone(), show.clone(), info.clone());
        check.connect_clicked(move |_| {
            let Some(version) = info.borrow().clone() else {
                return;
            };
            let channel = view::android_check_channel(&version);
            let show = show.clone();
            request(
                &ctx,
                move |c| c.use_android(&channel, false),
                move |r| show(r),
            );
        });
    }
    {
        // The restart is the user's own Android's, so no administrator is asked; the user
        // agrees first that its app windows close.
        let (ctx, show, info) = (ctx.clone(), show.clone(), info.clone());
        restart_button.connect_clicked(move |_| {
            let Some(version) = info
                .borrow()
                .as_ref()
                .and_then(|version| version.restart_pending.clone())
            else {
                return;
            };
            let (asked, show) = (ctx.clone(), show.clone());
            confirm(
                &ctx,
                &tr("Restart Android now?"),
                &trf(
                    "Android restarts into Android {0}.\n\nIts app windows close. Open the apps again once it has started.",
                    &[version.as_str()],
                ),
                &tr("Restart now"),
                false,
                move || {
                    let show = show.clone();
                    request(&asked, |c| c.restart_android(), move |r| show(r));
                },
            );
        });
    }
    {
        let (ctx, show) = (ctx.clone(), show.clone());
        apply.connect_clicked(move |_| {
            let Some(version) = info.borrow().clone() else {
                return;
            };
            let choices = view::android_choices(&version);
            let Some(choice) = choices.get(picked.get() as usize).cloned() else {
                return;
            };
            if choice.channel == version.channel {
                return;
            }
            let run = {
                let (ctx, show) = (ctx.clone(), show.clone());
                let channel = choice.channel.clone();
                move |erase: bool| {
                    let show = show.clone();
                    let channel = channel.clone();
                    request(&ctx, move |c| c.use_android(&channel, erase), move |r| show(r));
                }
            };
            match view::android_move(&version, &choice) {
                view::AndroidMove::Same => run(false),
                view::AndroidMove::Up => confirm(
                    &ctx,
                    &tr("Move to a newer Android?"),
                    &trf(
                        "Android {0} is downloaded and used the next time Android starts.\n\nYour apps and their data move along. Moving back later erases the data.",
                        &[&choice.major.to_string()],
                    ),
                    &tr("Change"),
                    false,
                    move || run(false),
                ),
                view::AndroidMove::Down => confirm(
                    &ctx,
                    &tr("Erase Android data to move to an older version?"),
                    &trf(
                        "Android {0} is older than the Android {1} that runs now. Android cannot read data a newer version wrote.\n\nMoving erases the Android data of every user of this computer:\n• every app they installed\n• the accounts they signed in to\n• the data of every app\n\nEach user's Android then starts as it was first installed. DSA's settings stay.\n\nThis cannot be undone.",
                        &[&choice.major.to_string(), &version.major.to_string()],
                    ),
                    &tr("Erase data and change"),
                    true,
                    move || run(true),
                ),
            }
        });
    }
    {
        let show = show.clone();
        request(ctx, |c| c.android_version(), move |r| show(r));
    }
    // A download moves on by itself, so the version is read again every few seconds while
    // the page is shown.
    {
        let ctx = ctx.clone();
        let row = watched_row;
        let asking = Rc::new(std::cell::Cell::new(false));
        gtk::glib::timeout_add_seconds_local(3, move || {
            let Some(row) = row.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            if !row.is_mapped() || asking.get() {
                return gtk::glib::ControlFlow::Continue;
            }
            asking.set(true);
            let (show, asking) = (show.clone(), asking.clone());
            request(
                &ctx,
                |c| c.android_version(),
                move |r| {
                    asking.set(false);
                    if r.is_ok() {
                        show(r);
                    }
                },
            );
            gtk::glib::ControlFlow::Continue
        });
    }
}

/// The update settings: the Android image (what runs, the version it follows, its update and the
/// version choice) and DSA itself, which comes through the system's package manager on Linux.
pub fn updates(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());
    let program = group(
        &tr("DSA"),
        Some(&tr("New versions of DSA come through the system's package manager and software center, with the other updates.\nInstalling DSA added its signed package repository.")),
    );
    let software = link_row(
        &tr("Open software updates"),
        &tr("Shows the updates the system has for DSA and the other installed programs."),
    );
    program.add(&software);
    // The release channel: which of DSA's package repositories the system reads.
    let channel_switch: Rc<RefCell<Option<Switch>>> = Rc::default();
    let switch = {
        let (ctx, page, cell) = (ctx.clone(), page.clone(), channel_switch.clone());
        Switch::new(
            &tr("Get pre-release versions"),
            &tr("Also install versions that are still being finished. Every release before 1.0 is a pre-release.\nThe package manager then reads DSA's pre-release repository.\nTurning this off never moves DSA back: it waits for a stable release newer than the one installed."),
            move |on| {
                let page = page.clone();
                let cell = cell.clone();
                request(&ctx, move |c| c.set_machine("prerelease-updates", on), move |r| {
                    page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                    if let Some(switch) = cell.borrow().as_ref() {
                        match r {
                            Ok(settings) => switch.set(settings.prerelease_updates),
                            Err(_) => switch.set(!on),
                        }
                    }
                });
            },
        )
    };
    *channel_switch.borrow_mut() = Some(switch);
    if let Some(switch) = channel_switch.borrow().as_ref() {
        program.add(&switch.row);
    }
    {
        let ctx = ctx.clone();
        software.connect_activated(move |_| {
            let started = view::software_updates_commands()
                .iter()
                .any(|command| start_program(command).is_ok());
            if !started {
                tell(
                    &ctx,
                    &tr("No software center was found"),
                    &tr("Update DSA with the system's package manager, such as apt or dnf."),
                );
            }
        });
    }

    let image = group(
        &tr("Android image"),
        Some(&tr("This setting applies to every user of this computer, so changing it asks for an administrator's permission.")),
    );
    let image_switch: Rc<RefCell<Option<Switch>>> = Rc::default();
    let switch = {
        let (ctx, page, cell) = (ctx.clone(), page.clone(), image_switch.clone());
        Switch::new(
            &tr("Update the Android image automatically"),
            &tr("Download signed security updates and new versions of Android.\nThey apply when Android restarts, as the Android update settings above say."),
            move |on| {
                let page = page.clone();
                let cell = cell.clone();
                request(&ctx, move |c| c.set_machine("image-updates", on), move |r| {
                    page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                    if let Some(switch) = cell.borrow().as_ref() {
                        match r {
                            Ok(settings) => switch.set(settings.image_updates),
                            Err(_) => switch.set(!on),
                        }
                    }
                });
            },
        )
    };
    *image_switch.borrow_mut() = Some(switch);
    if let Some(switch) = image_switch.borrow().as_ref() {
        image.add(&switch.row);
    }
    // Android first (what runs, its update, the version choice), then DSA itself.
    android_groups(ctx, &page, &image);
    page.add(&program);
    program_progress(ctx, &program);

    {
        let page = page.clone();
        request(
            ctx,
            |c| c.settings(),
            move |r| {
                page.show_failure(&tr("The setting was not changed"), r.as_ref().err());
                if let (Ok(settings), Some(switch)) = (&r, channel_switch.borrow().as_ref()) {
                    switch.set(settings.prerelease_updates);
                    switch.row.set_sensitive(settings.update_channel_available);
                    if !settings.update_channel_available {
                        switch.row.set_subtitle(&tr(
                            "This installation does not follow a release channel.",
                        ));
                    }
                }
                if let (Ok(settings), Some(switch)) = (r, image_switch.borrow().as_ref()) {
                    switch.set(settings.image_updates);
                    switch.row.set_sensitive(settings.image_updates_available);
                    if !settings.image_updates_available {
                        switch
                            .row
                            .set_subtitle(&tr("This installation does not offer image updates."));
                    }
                }
            },
        );
    }
    page.root.clone().upcast()
}

/// An update of DSA while it runs, in `program`: the phase, the version and a line asking not
/// to close DSA or start Android apps, with a moving progress bar, across the service's
/// restart; then how it ended. The running service is asked every 1.5 seconds while the page
/// is shown, never starting the service, so a service being replaced is not started again.
fn program_progress(ctx: &Shared, program: &adw::PreferencesGroup) {
    let progress_row = action_row("", "");
    progress_row.set_subtitle_lines(0);
    let bar = gtk::ProgressBar::builder()
        .valign(gtk::Align::Center)
        .width_request(120)
        .tooltip_text(tr("Updating DSA"))
        .build();
    progress_row.add_suffix(&bar);
    progress_row.set_visible(false);
    let (ended_row, ended_label) = value_row(&tr("Update of DSA"), "");
    ended_row.set_visible(false);
    program.add(&progress_row);
    program.add(&ended_row);

    // The bar moves on its own while it is shown: the phase says what is happening, not how
    // far it is.
    {
        let bar = bar.downgrade();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
            let Some(bar) = bar.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            if bar.is_mapped() {
                bar.pulse();
            }
            gtk::glib::ControlFlow::Continue
        });
    }
    let follower: Rc<RefCell<view::ProgramUpdateFollower>> = Rc::default();
    let show = {
        let (follower, progress_row) = (follower.clone(), progress_row.clone());
        let ended_row = ended_row.clone();
        Rc::new(move || {
            let follower = follower.borrow();
            progress_row.set_visible(follower.in_progress());
            progress_row.set_title(&follower.phase_text());
            let mut lines: Vec<String> = follower.version_text().into_iter().collect();
            lines.push(view::ProgramUpdateFollower::keep_open_text());
            progress_row.set_subtitle(&lines.join("\n"));
            match follower.ended_text() {
                Some((succeeded, text)) => {
                    ended_label.set_label(&text);
                    if succeeded {
                        ended_row.remove_css_class("error");
                        ended_row.add_css_class("success");
                    } else {
                        ended_row.remove_css_class("success");
                        ended_row.add_css_class("error");
                    }
                    ended_row.set_visible(true);
                }
                None => ended_row.set_visible(false),
            }
        })
    };
    let ctx = ctx.clone();
    let watched = program.downgrade();
    let asking = Rc::new(std::cell::Cell::new(false));
    let poll = move || {
        let Some(group) = watched.upgrade() else {
            return gtk::glib::ControlFlow::Break;
        };
        if !group.is_mapped() || asking.get() {
            return gtk::glib::ControlFlow::Continue;
        }
        asking.set(true);
        let (follower, show, asking) = (follower.clone(), show.clone(), asking.clone());
        request(
            &ctx,
            |c| c.running_status(),
            move |r| {
                asking.set(false);
                follower
                    .borrow_mut()
                    .polled(r.as_ref(), std::time::Instant::now());
                show();
            },
        );
        gtk::glib::ControlFlow::Continue
    };
    gtk::glib::timeout_add_local(std::time::Duration::from_millis(1500), poll);
}

/// The versions, the licences, the source code and the notices.
pub fn about(ctx: &Shared) -> gtk::Widget {
    let page = Rc::new(Page::new());
    let product = group("", None);
    let header = action_row("Desktop Subsystem for Android™", "Thisisthepy");
    header.add_prefix(&{
        let image = gtk::Image::from_icon_name(crate::APP_ID);
        image.set_pixel_size(48);
        image
    });
    product.add(&header);
    let (app_row, _) = value_row(&tr("DSA Settings version"), env!("CARGO_PKG_VERSION"));
    product.add(&app_row);
    let (service_row, service_label) = value_row(&tr("DSA service version"), "");
    product.add(&service_row);
    let (image_row, image_label) = value_row(&tr("Android image version"), "");
    image_row.set_subtitle(&tr(
        "The Android image the next start boots. Image updates change it.",
    ));
    product.add(&image_row);
    page.add(&product);

    let links = group("", None);
    let licenses = link_row(
        &tr("Licences"),
        &tr("DSA is licensed under the Apache License 2.0. Open the licences of every component it ships."),
    );
    links.add(&licenses);
    let source = link_row(&tr("Source code"), view::SOURCE_URL);
    links.add(&source);
    page.add(&links);
    {
        let ctx = ctx.clone();
        licenses.connect_activated(move |_| {
            open_folder(&ctx, std::path::Path::new(view::LICENSE_FOLDER))
        });
    }
    {
        let ctx = ctx.clone();
        source.connect_activated(move |_| open_uri(&ctx, view::SOURCE_URL));
    }

    let notices = group(
        "",
        Some(&format!(
            "{}\n{}",
            tr("Desktop Subsystem for Android is an independent open source project.\nIt is not affiliated with, endorsed by or sponsored by Microsoft or Google."),
            "Android is a trademark of Google LLC."
        )),
    );
    page.add(&notices);

    request(
        ctx,
        |c| c.settings(),
        move |r| {
            if let Ok(settings) = r {
                service_label.set_label(&settings.version);
                image_label.set_label(&settings.android_version.unwrap_or_else(|| tr("Unknown")));
            }
        },
    );
    page.root.clone().upcast()
}
