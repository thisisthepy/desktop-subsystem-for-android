//! The Android log page: Android's log as it comes, filtered by priority and by text, which
//! can be paused, and saving the diagnostics. Android sends its log only while this page is
//! shown, and nothing is written to disk but the diagnostics archive the user saves.

use std::cell::RefCell;
use std::process::Child;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use super::common::*;
use crate::api::{self, Failure};
use crate::i18n::{tr, trf};
use crate::model::LogLine;
use crate::view::{self, LogView};

struct Reading {
    child: Child,
    lines: mpsc::Receiver<Result<String, Failure>>,
}

pub fn logs(ctx: &Shared) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let banner = adw::Banner::builder()
        .revealed(false)
        .use_markup(false)
        .build();
    root.append(&banner);

    let clamp = adw::Clamp::builder().maximum_size(1200).build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    clamp.set_child(Some(&body));
    clamp.set_vexpand(true);
    root.append(&clamp);

    let note = gtk::Label::builder()
        .label(tr("Android sends its log only while this page is open, and nothing is written to disk. Save diagnostics gathers what a problem report needs into one zip file."))
        .wrap(true)
        .xalign(0.0)
        .build();
    note.add_css_class("dim-label");
    body.append(&note);

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let labels: Vec<String> = view::LOG_PRIORITIES
        .iter()
        .map(|p| view::priority_label(p))
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let priority = gtk::DropDown::from_strings(&refs);
    priority.set_selected(2);
    let filter = gtk::SearchEntry::builder()
        .placeholder_text(tr("Filter by tag or text"))
        .hexpand(true)
        .build();
    let pause = gtk::ToggleButton::with_label(&tr("Pause"));
    let save = gtk::Button::with_label(&tr("Save diagnostics"));
    save.add_css_class("suggested-action");
    bar.append(&priority);
    bar.append(&filter);
    bar.append(&pause);
    bar.append(&save);
    body.append(&bar);

    let status = gtk::Label::builder().xalign(0.0).wrap(true).build();
    status.add_css_class("dim-label");
    body.append(&status);

    let list = gtk::ListBox::new();
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .build();
    scroll.add_css_class("card");
    body.append(&scroll);

    let model = Rc::new(RefCell::new(LogView::default()));
    let reading: Rc<RefCell<Option<Reading>>> = Rc::default();

    // The lines the list shows, in its order: row n is line n.
    let shown: Rc<RefCell<Vec<LogLine>>> = Rc::default();
    copy_lines(&list, &shown);

    let redraw = {
        let (model, list, shown) = (model.clone(), list.clone(), shown.clone());
        Rc::new(move || {
            list.remove_all();
            let lines: Vec<LogLine> = model.borrow().shown().into_iter().cloned().collect();
            for line in &lines {
                list.append(&log_row(line));
            }
            *shown.borrow_mut() = lines;
        })
    };
    let stop = {
        let reading = reading.clone();
        Rc::new(move || {
            if let Some(mut r) = reading.borrow_mut().take() {
                let _ = r.child.kill();
                let _ = r.child.wait();
            }
        })
    };
    let start = {
        let (reading, model, stop, banner) =
            (reading.clone(), model.clone(), stop.clone(), banner.clone());
        let (priority, redraw) = (priority.clone(), redraw.clone());
        Rc::new(move || {
            stop();
            model.borrow_mut().clear();
            redraw();
            let level = view::LOG_PRIORITIES
                .get(priority.selected() as usize)
                .copied()
                .unwrap_or("I");
            let (sender, lines) = mpsc::channel();
            match api::spawn_lines(&view::logcat_args(level), sender) {
                Ok(child) => {
                    banner.set_revealed(false);
                    *reading.borrow_mut() = Some(Reading { child, lines });
                }
                Err(failure) => {
                    banner.set_title(&format!(
                        "{}: {}",
                        tr("The log cannot be read"),
                        view::problem_text(&failure).replace('\n', " ")
                    ));
                    banner.set_revealed(true);
                }
            }
        })
    };

    // Takes what has come since the last look, a batch at a time, so a busy log does not
    // redraw for every line.
    {
        let (reading, model, list, shown, banner, status) = (
            reading.clone(),
            model.clone(),
            list.clone(),
            shown.clone(),
            banner.clone(),
            status.clone(),
        );
        let (scroll, root_weak) = (scroll.clone(), root.downgrade());
        glib::timeout_add_local(Duration::from_millis(200), move || {
            if root_weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            let mut appended = Vec::new();
            if let Some(r) = reading.borrow().as_ref() {
                let mut model = model.borrow_mut();
                while let Ok(line) = r.lines.try_recv() {
                    match line {
                        Ok(line) => {
                            if model.add(&line) {
                                if let Some(last) = model.last() {
                                    if model.matches(last) {
                                        appended.push(last.clone());
                                    }
                                }
                            }
                        }
                        Err(failure) => {
                            banner.set_title(&format!(
                                "{}: {}",
                                tr("The log cannot be read"),
                                view::problem_text(&failure).replace('\n', " ")
                            ));
                            banner.set_revealed(true);
                        }
                    }
                }
                status.set_label(&model.paused_text());
            }
            if !appended.is_empty() {
                let mut shown = shown.borrow_mut();
                for line in appended {
                    list.append(&log_row(&line));
                    shown.push(line);
                }
                // The page keeps as many lines as the model does; the oldest rows go first.
                while shown.len() > LogView::LIMIT {
                    if let Some(first) = list.row_at_index(0) {
                        list.remove(&first);
                    }
                    shown.remove(0);
                }
                let adjustment = scroll.vadjustment();
                adjustment.set_value(adjustment.upper());
            }
            glib::ControlFlow::Continue
        });
    }
    {
        let (model, redraw) = (model.clone(), redraw.clone());
        filter.connect_search_changed(move |entry| {
            model.borrow_mut().filter = entry.text().to_string();
            redraw();
        });
    }
    {
        let (model, redraw, status) = (model.clone(), redraw.clone(), status.clone());
        pause.connect_toggled(move |button| {
            model.borrow_mut().set_paused(button.is_active());
            status.set_label(&model.borrow().paused_text());
            redraw();
        });
    }
    {
        let start = start.clone();
        priority.connect_selected_notify(move |_| start());
    }
    {
        let start = start.clone();
        root.connect_map(move |_| start());
    }
    {
        let stop = stop.clone();
        root.connect_unmap(move |_| stop());
    }
    {
        let (ctx, status) = (ctx.clone(), status.clone());
        save.connect_clicked(move |button| {
            let dialog = gtk::FileDialog::builder()
                .title(tr("Save diagnostics"))
                .initial_name("dsa-diagnostics.zip")
                .build();
            let (ctx, status, button) = (ctx.clone(), status.clone(), button.clone());
            dialog.save(
                Some(&ctx.window.clone()),
                gio::Cancellable::NONE,
                move |file| {
                    let Some(path) = file.ok().and_then(|f| f.path()) else {
                        return;
                    };
                    let path = path.display().to_string();
                    button.set_sensitive(false);
                    status.set_label(&tr(
                        "Gathering the diagnostics. This takes a minute or more.",
                    ));
                    let shown = path.clone();
                    request(
                        &ctx,
                        move |c| c.collect_diagnostics(&path),
                        move |r| {
                            button.set_sensitive(true);
                            match r {
                                Ok(_) => status
                                    .set_label(&trf("Saved the diagnostics to {0}.", &[&shown])),
                                Err(failure) => status.set_label(&view::problem_text(&failure)),
                            }
                        },
                    );
                },
            );
        });
    }
    root.upcast()
}

/// Lets the user select lines (click, Shift and Ctrl clicks, Ctrl+A) and copy them with
/// Ctrl+C or the context menu's Copy, as the whole lines of the log.
fn copy_lines(list: &gtk::ListBox, shown: &Rc<RefCell<Vec<LogLine>>>) {
    list.set_selection_mode(gtk::SelectionMode::Multiple);

    let copy = gio::SimpleAction::new("copy", None);
    {
        let (list_weak, shown) = (list.downgrade(), shown.clone());
        copy.connect_activate(move |_, _| {
            let Some(list) = list_weak.upgrade() else {
                return;
            };
            let selected: Vec<usize> = list
                .selected_rows()
                .iter()
                .filter_map(|row| usize::try_from(row.index()).ok())
                .collect();
            let text = view::log_copy_text(&shown.borrow(), &selected);
            if !text.is_empty() {
                list.clipboard().set_text(&text);
            }
        });
    }
    let actions = gio::SimpleActionGroup::new();
    actions.add_action(&copy);
    list.insert_action_group("log", Some(&actions));

    let keys = gtk::ShortcutController::new();
    keys.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>c"),
        Some(gtk::NamedAction::new("log.copy")),
    ));
    list.add_controller(keys);

    let menu = gio::Menu::new();
    menu.append(Some(tr("Copy").as_str()), Some("log.copy"));
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(list);
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);

    // A right click on a line that is not selected selects it first, as file managers do.
    let click = gtk::GestureClick::builder()
        .button(gtk::gdk::BUTTON_SECONDARY)
        .build();
    {
        let list_weak = list.downgrade();
        click.connect_pressed(move |gesture, _, x, y| {
            let Some(list) = list_weak.upgrade() else {
                return;
            };
            if let Some(row) = list.row_at_y(y as i32) {
                if !row.is_selected() {
                    list.unselect_all();
                    list.select_row(Some(&row));
                }
            }
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
    }
    list.add_controller(click);
}

/// One log line as a row of columns: time, PID, TID, priority and the body. The app's font is
/// proportional, so the fields have fixed widths to line up; the body has none and runs into
/// the list's horizontal scroll bar.
fn log_row(line: &LogLine) -> gtk::Widget {
    let columns = line.columns();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_margin_start(6);
    row.set_margin_end(6);
    let cell = |text: &str, width: i32, xalign: f32| {
        let label = gtk::Label::builder()
            .label(text)
            .xalign(xalign)
            .use_markup(false)
            .single_line_mode(true)
            .build();
        if width > 0 {
            label.set_size_request(width, -1);
        }
        row.append(&label);
        label
    };
    cell(&columns.time, 164, 0.0);
    cell(&columns.pid, 56, 1.0);
    cell(&columns.tid, 56, 1.0);
    cell(&columns.priority, 32, 0.5);
    cell(&columns.body, 0, 0.0).set_margin_start(6);
    row.upcast()
}
