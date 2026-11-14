//! The Backup section of the Advanced page: whether backup is on (off until the user turns it
//! on, since it keeps a second copy of the apps' user data on this computer), where the
//! encrypted backup stands, backing up and restoring now, the recovery key that opens the
//! backup on another computer, and how Android's own keys are protected here.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use super::common::*;
use crate::api::Failure;
use crate::i18n::tr;
use crate::model::{backup_actions, backup_key_words, Backup, BackupJob, KeyImport};
use crate::view;

/// A time in milliseconds since 1970 in the user's local time and date format.
fn local_time(unix_ms: i64) -> String {
    gtk::glib::DateTime::from_unix_local(unix_ms.div_euclid(1000))
        .ok()
        .and_then(|d| d.format("%x %X").ok()) // not translated
        .map(|d| d.to_string())
        .unwrap_or_default()
}

/// What the backup switch does when it is flipped, set once the rest of the section exists.
type SwitchHandler = Rc<RefCell<Option<Rc<dyn Fn(bool)>>>>;

/// Adds the Backup section to `page`.
pub fn backup(ctx: &Shared, page: &Rc<Page>) {
    let backup_group = group(
        &tr("Backup"),
        Some(&tr("DSA keeps an encrypted backup of your Android apps' data in a folder of your home folder.\nThe apps and their data can come back after Android's data is reset, or on another computer.\nThe key that opens it is kept in your keyring.")),
    );
    // The switch's changes are handled once the rest of the section exists.
    let switched: SwitchHandler = Rc::default();
    let switch = {
        let switched = switched.clone();
        Switch::new(
            &tr("Back up apps' data to this computer"),
            &view::backup_switch_subtitle(None),
            move |on| {
                if let Some(switched) = switched.borrow().as_ref() {
                    switched(on);
                }
            },
        )
    };
    switch.row.set_sensitive(false);
    backup_group.add(&switch.row);
    let (folder_row, folder) = value_row(&tr("Folder"), "");
    backup_group.add(&folder_row);
    let problem_row = action_row(&tr("Backups are unavailable"), "");
    problem_row.set_visible(false);
    backup_group.add(&problem_row);
    let (last_row, last) = value_row(&tr("Last backup"), "");
    backup_group.add(&last_row);
    let (contents_row, contents) = value_row(&tr("In the backup"), "");
    backup_group.add(&contents_row);
    let (restored_row, restored) = value_row(&tr("Last restore"), "");
    restored_row.set_visible(false);
    backup_group.add(&restored_row);
    let error_row = action_row(&tr("Last problem"), "");
    error_row.set_visible(false);
    backup_group.add(&error_row);
    let busy_row = action_row("", "");
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    busy_row.add_suffix(&spinner);
    busy_row.set_visible(false);
    backup_group.add(&busy_row);
    let result_row = action_row("", "");
    result_row.set_visible(false);
    backup_group.add(&result_row);
    let (now_row, now_button) = button_row(
        &tr("Back up now"),
        &tr("Has Android back up the data of every app into the folder now. Android starts if it is not running."),
        &tr("Back up"),
    );
    now_button.add_css_class("suggested-action");
    backup_group.add(&now_row);
    let (restore_row, restore) = button_row(
        &tr("Restore"),
        &tr("Installs the apps of the backup that Android does not have, then replaces every app's data with the backup's."),
        &tr("Restore"),
    );
    backup_group.add(&restore_row);
    let (key_row, key) = button_row(
        &tr("Show recovery key"),
        &tr("The key that opens the backup anywhere else. Write it down and keep it safe."),
        &tr("Show"),
    );
    backup_group.add(&key_row);
    let (import_row, import) = button_row(
        &tr("Use a recovery key"),
        &tr("Opens a backup folder brought from another computer, or kept from before this computer's keyring was reset."),
        &tr("Enter"),
    );
    backup_group.add(&import_row);
    page.add(&backup_group);
    // Rows that mean something only while backup is on.
    let on_rows = [
        folder_row.clone(),
        last_row.clone(),
        contents_row.clone(),
        now_row.clone(),
        restore_row.clone(),
        key_row.clone(),
        import_row.clone(),
    ];

    let keys_group = group(&tr("Protection of Android's keys"), None);
    let keys_row = action_row(&tr("Android's keys"), "");
    keys_group.add(&keys_row);
    let keys_reason_row = action_row(&tr("Why not the TPM"), "");
    keys_reason_row.set_visible(false);
    keys_group.add(&keys_reason_row);
    let keys_problem_row = action_row(&tr("Android's keys could not be opened"), "");
    keys_problem_row.add_css_class("error");
    keys_problem_row.set_visible(false);
    keys_group.add(&keys_problem_row);
    keys_group.set_visible(false);
    page.add(&keys_group);

    let now: Rc<RefCell<Option<Backup>>> = Rc::default();
    let running: Rc<Cell<Option<BackupJob>>> = Rc::default();
    // The buttons follow the backup, and none works while a request is under way.
    let buttons: Rc<dyn Fn()> = {
        let (now, running) = (now.clone(), running.clone());
        let rows = (
            now_row.clone(),
            restore_row.clone(),
            key_row.clone(),
            import_row.clone(),
            switch.row.clone(),
        );
        Rc::new(move || {
            let actions = backup_actions(now.borrow().as_ref(), running.get());
            rows.0.set_sensitive(actions.back_up);
            rows.1.set_sensitive(actions.restore);
            rows.2.set_sensitive(actions.show_key);
            rows.3.set_sensitive(actions.import_key);
            rows.4.set_sensitive(actions.switch);
        })
    };
    buttons();
    // Shows the backup as the service reports it, or what went wrong.
    let show: Rc<dyn Fn(String, Result<Backup, Failure>)> = {
        let (page, now, buttons, switch) =
            (page.clone(), now.clone(), buttons.clone(), switch.clone());
        Rc::new(move |failed: String, r: Result<Backup, Failure>| {
            page.show_failure(&failed, r.as_ref().err());
            if let Ok(backup) = r {
                switch.set(backup.enabled);
                switch
                    .row
                    .set_subtitle(&view::backup_switch_subtitle(Some(&backup)));
                for row in &on_rows {
                    row.set_visible(backup.enabled);
                }
                match &backup.keymint_key {
                    Some(key) => {
                        keys_group.set_visible(true);
                        keys_row.set_subtitle(&view::key_protection_text(key));
                        let reason = view::key_protection_reason(key);
                        keys_reason_row.set_visible(reason.is_some());
                        keys_reason_row.set_subtitle(reason.as_deref().unwrap_or_default());
                        keys_problem_row.set_visible(key.problem.is_some());
                        keys_problem_row.set_subtitle(key.problem.as_deref().unwrap_or_default());
                    }
                    None => keys_group.set_visible(false),
                }
                folder.set_label(&backup.folder);
                let problem = backup.shown_problem();
                problem_row.set_visible(problem.is_some());
                problem_row.set_subtitle(problem.unwrap_or_default());
                last.set_label(&view::backup_when(backup.last_backup(), local_time));
                contents.set_label(&view::backup_contents(&backup));
                restored_row.set_visible(backup.last_restore().is_some());
                restored.set_label(&view::backup_when(backup.last_restore(), local_time));
                error_row.set_visible(backup.last_error.is_some());
                error_row.set_subtitle(backup.last_error.as_deref().unwrap_or_default());
                *now.borrow_mut() = Some(backup);
            }
            buttons();
        })
    };
    // Backs up or restores, showing that it runs until it ends, and how it ended.
    let run: Rc<dyn Fn(BackupJob)> = {
        let (ctx, show, running, buttons, busy_row, result_row) = (
            ctx.clone(),
            show.clone(),
            running.clone(),
            buttons.clone(),
            busy_row.clone(),
            result_row.clone(),
        );
        Rc::new(move |job: BackupJob| {
            running.set(Some(job));
            busy_row.set_title(&view::backup_busy_text(job));
            busy_row.set_visible(job.is_long());
            result_row.set_visible(false);
            buttons();
            let (ctx2, show, running) = (ctx.clone(), show.clone(), running.clone());
            let (busy_row, result_row) = (busy_row.clone(), result_row.clone());
            request(
                &ctx,
                move |c| match job {
                    BackupJob::Restore => c.restore_backup(),
                    _ => c.back_up_now(),
                },
                move |r| {
                    running.set(None);
                    busy_row.set_visible(false);
                    if let Ok(backup) = &r {
                        let result = view::backup_result(job, backup);
                        result_row.set_title(&result);
                        result_row.set_visible(true);
                        ctx2.toast(&result);
                    }
                    let failed = if job == BackupJob::Restore {
                        tr("The backup was not restored")
                    } else {
                        tr("The apps were not backed up")
                    };
                    show(failed, r);
                },
            );
        })
    };
    {
        let run = run.clone();
        now_button.connect_clicked(move |_| run(BackupJob::BackUp));
    }
    {
        let (ctx, run) = (ctx.clone(), run.clone());
        restore.connect_clicked(move |_| {
            let run = run.clone();
            confirm(
                &ctx,
                &tr("Restore from the backup?"),
                &view::backup_restore_warning(),
                &tr("Restore"),
                true,
                move || run(BackupJob::Restore),
            );
        });
    }
    {
        let (ctx, page, running, buttons) =
            (ctx.clone(), page.clone(), running.clone(), buttons.clone());
        key.connect_clicked(move |_| {
            running.set(Some(BackupJob::ShowKey));
            buttons();
            let (ctx2, page, running, buttons) =
                (ctx.clone(), page.clone(), running.clone(), buttons.clone());
            request(
                &ctx,
                |c| c.backup_recovery_key(),
                move |r| {
                    running.set(None);
                    buttons();
                    page.show_failure(&tr("The recovery key could not be shown"), r.as_ref().err());
                    if let Ok(key) = r {
                        show_recovery_key(&ctx2, &key);
                    }
                },
            );
        });
    }
    {
        let (ctx, show, running, buttons) =
            (ctx.clone(), show.clone(), running.clone(), buttons.clone());
        import.connect_clicked(move |_| {
            let (ctx2, show, running, buttons) =
                (ctx.clone(), show.clone(), running.clone(), buttons.clone());
            ask_recovery_key(&ctx, move |text: String| {
                running.set(Some(BackupJob::ImportKey));
                buttons();
                let (ctx3, show, running, buttons) =
                    (ctx2.clone(), show.clone(), running.clone(), buttons.clone());
                request(
                    &ctx2,
                    move |c| c.import_backup_key(&text),
                    move |r| {
                        running.set(None);
                        match r {
                            Ok(KeyImport::Kept(backup)) => {
                                ctx3.toast(&tr(
                                    "The recovery key is kept. The backup folder is read with it.",
                                ));
                                show(String::new(), Ok(backup));
                            }
                            Ok(KeyImport::Refused(reason)) => {
                                buttons();
                                tell(
                                    &ctx3,
                                    &tr("The recovery key was not used"),
                                    &view::backup_key_refusal(&reason),
                                );
                            }
                            Err(failure) => show(tr("The recovery key was not used"), Err(failure)),
                        }
                    },
                );
            });
        });
    }
    // Turning backup on, or off: off while backups are kept asks whether they go too.
    {
        let (ctx, show, running, buttons, now, result_row, switch) = (
            ctx.clone(),
            show.clone(),
            running.clone(),
            buttons.clone(),
            now.clone(),
            result_row.clone(),
            switch.clone(),
        );
        let dialog_ctx = ctx.clone();
        let set: Rc<dyn Fn(bool, bool)> = Rc::new(move |on: bool, delete: bool| {
            running.set(Some(BackupJob::Switch));
            buttons();
            let (ctx2, show, running, result_row) = (
                ctx.clone(),
                show.clone(),
                running.clone(),
                result_row.clone(),
            );
            request(
                &ctx,
                move |c| c.set_backup(on, delete),
                move |r| {
                    running.set(None);
                    if r.is_ok() {
                        let result = view::backup_switched(on, delete);
                        result_row.set_title(&result);
                        result_row.set_visible(true);
                        ctx2.toast(&result);
                    }
                    show(
                        if on {
                            tr("Backup was not turned on")
                        } else {
                            tr("Backup was not turned off")
                        },
                        r,
                    );
                },
            );
        });
        *switched.borrow_mut() = Some(Rc::new(move |on: bool| {
            let kept = now
                .borrow()
                .as_ref()
                .filter(|b| !on && b.has_backups_kept())
                .cloned();
            match kept {
                None => set(on, false),
                Some(backup) => {
                    let (set, switch) = (set.clone(), switch.clone());
                    ask_delete(&dialog_ctx, &backup, move |answer| match answer {
                        Some(delete) => set(false, delete),
                        // Closed without an answer: backup stays on.
                        None => switch.set(true),
                    });
                }
            }
        }));
    }
    {
        let running = running.clone();
        running.set(Some(BackupJob::Read));
        buttons();
        request(
            ctx,
            |c| c.backup(),
            move |r| {
                running.set(None);
                show(tr("Where the backup stands could not be read"), r);
            },
        );
    }
}

/// Asks, when backup is turned off while `backup` holds apps, whether its backups and key are
/// deleted too (`Some(true)`), kept (`Some(false)`), or backup stays on (`None`).
fn ask_delete(ctx: &Shared, backup: &Backup, then: impl Fn(Option<bool>) + 'static) {
    let dialog = adw::MessageDialog::new(
        Some(&ctx.window),
        Some(&tr("Turn off backup?")),
        Some(&view::backup_delete_question(backup)),
    );
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("keep", &tr("Keep them"));
    dialog.add_response("delete", &tr("Delete backups"));
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("keep"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        then(match response {
            "delete" => Some(true),
            "keep" => Some(false),
            _ => None,
        })
    });
    dialog.present();
}

/// Shows the recovery key in a dialog, in selectable text with a copy button, with the advice
/// to write it down and keep it safe.
fn show_recovery_key(ctx: &Shared, key: &str) {
    let dialog = adw::MessageDialog::new(
        Some(&ctx.window),
        Some(&tr("Recovery key")),
        Some(&view::backup_key_advice()),
    );
    let label = gtk::Label::builder()
        .label(key)
        .selectable(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .justify(gtk::Justification::Center)
        .hexpand(true)
        .build();
    label.add_css_class("monospace");
    label.add_css_class("title-4");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&label);
    content.append(&copy_button(&label));
    dialog.set_extra_child(Some(&content));
    dialog.add_response("ok", &tr("OK"));
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog.present();
}

/// Asks for a recovery key and runs `then` with what the user entered. Use works only once
/// something that can be a key is entered; Enter in the field does the same as Use.
fn ask_recovery_key(ctx: &Shared, then: impl Fn(String) + 'static) {
    let dialog = adw::MessageDialog::new(
        Some(&ctx.window),
        Some(&tr("Use a recovery key")),
        Some(&tr("Enter the recovery key of the backup folder, as it was written down.\n\nSpaces, dashes and capitals do not matter.\n\nA key that does not open the folder is refused, and the key kept now stays.")),
    );
    let entry = gtk::Entry::builder().hexpand(true).build();
    entry.add_css_class("monospace");
    dialog.set_extra_child(Some(&entry));
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("ok", &tr("Use"));
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled("ok", false);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    {
        let dialog = dialog.clone();
        entry.connect_changed(move |entry| {
            dialog.set_response_enabled("ok", !backup_key_words(&entry.text()).is_empty());
        });
    }
    let then: Rc<dyn Fn(String)> = Rc::new(then);
    {
        let (dialog, then) = (dialog.clone(), then.clone());
        entry.connect_activate(move |entry| {
            if !backup_key_words(&entry.text()).is_empty() {
                then(entry.text().to_string());
                dialog.close();
            }
        });
    }
    {
        let entry = entry.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "ok" {
                then(entry.text().to_string());
            }
        });
    }
    dialog.present();
}
