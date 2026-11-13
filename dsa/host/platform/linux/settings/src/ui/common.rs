//! What every page is built from: running a request off the main thread, the rows and
//! groups of a GNOME Settings page, the banner that says what went wrong, and the questions
//! the app asks.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::api::Failure;
use crate::i18n::tr;
use crate::model::Client;
use crate::view;

/// What the pages share: the client of the local API and the window they are in.
pub struct Ctx {
    pub client: Client,
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    /// The app whose row the Apps page opens and brings into view the next time it lists the
    /// apps, as `--app` asks.
    pub focus_app: RefCell<Option<String>>,
}

pub type Shared = Rc<Ctx>;

impl Ctx {
    /// Says `text` for a moment at the bottom of the window.
    pub fn toast(&self, text: &str) {
        let toast = adw::Toast::new(text);
        toast.set_use_markup(false);
        self.toasts.add_toast(toast);
    }
}

/// Runs `work` on a worker thread, so a request that waits for the service never holds the
/// window, then `done` with its result on the main thread.
pub fn spawn<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    glib::spawn_future_local(async move {
        if let Ok(value) = gio::spawn_blocking(work).await {
            done(value);
        }
    });
}

/// Runs a request of the client on a worker thread.
pub fn request<T: Send + 'static>(
    ctx: &Shared,
    work: impl FnOnce(Client) -> Result<T, Failure> + Send + 'static,
    done: impl FnOnce(Result<T, Failure>) + 'static,
) {
    let client = ctx.client.clone();
    spawn(move || work(client), done);
}

/// A page of groups with a banner above it that says what went wrong.
pub struct Page {
    pub root: gtk::Box,
    pub prefs: adw::PreferencesPage,
    pub banner: adw::Banner,
}

impl Default for Page {
    fn default() -> Page {
        Page::new()
    }
}

impl Page {
    pub fn new() -> Page {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let banner = adw::Banner::builder()
            .revealed(false)
            .use_markup(false)
            .build();
        let prefs = adw::PreferencesPage::new();
        prefs.set_vexpand(true);
        root.append(&banner);
        root.append(&prefs);
        Page {
            root,
            prefs,
            banner,
        }
    }

    pub fn add(&self, group: &adw::PreferencesGroup) {
        self.prefs.add(group);
    }

    /// Shows what went wrong, or hides the banner when `failure` is `None`.
    pub fn show_failure(&self, title: &str, failure: Option<&Failure>) {
        match failure {
            Some(failure) => {
                let detail = view::problem_text(failure).replace('\n', " ");
                self.banner.set_title(&format!("{title}: {detail}"));
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
    }
}

pub fn group(title: &str, description: Option<&str>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    if !title.is_empty() {
        group.set_title(title);
    }
    if let Some(description) = description {
        group.set_description(Some(description));
    }
    group
}

pub fn action_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .use_markup(false)
        .build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row
}

/// A row that shows a property: its name as the title and its value under it, which can be
/// selected and copied.
pub fn property_row(title: &str) -> adw::ActionRow {
    let row = action_row(title, "");
    row.set_subtitle_selectable(true);
    row.add_css_class("property");
    row
}

/// A row with a box to type a value in.
pub fn entry_row(title: &str) -> adw::EntryRow {
    adw::EntryRow::builder()
        .title(title)
        .use_markup(false)
        .build()
}

/// A row that opens to show more rows under it.
pub fn expander_row(title: &str, subtitle: &str) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title(title)
        .use_markup(false)
        .build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row
}

/// A block of text at the top of a group, such as a device's name over its maker, spaced from
/// the rows below it as GNOME's About shows the device.
pub fn heading_block() -> gtk::Box {
    let block = gtk::Box::new(gtk::Orientation::Vertical, 4);
    block.set_margin_bottom(6);
    block
}

/// A row that shows a value on its right, which can be selected and copied.
pub fn value_row(title: &str, value: &str) -> (adw::ActionRow, gtk::Label) {
    let row = action_row(title, "");
    let label = gtk::Label::builder()
        .label(value)
        .selectable(true)
        .wrap(true)
        .xalign(1.0)
        .build();
    label.add_css_class("dim-label");
    row.add_suffix(&label);
    (row, label)
}

/// A flat copy button that puts what `source` shows at the moment of the click on the
/// clipboard, and shows a check mark for a moment so the user sees that the copy happened.
pub fn copy_button(source: &gtk::Label) -> gtk::Button {
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text(tr("Copy"))
        .valign(gtk::Align::Center)
        .build();
    copy.add_css_class("flat");
    copy.update_property(&[gtk::accessible::Property::Label(&tr("Copy"))]);
    let source = source.clone();
    copy.connect_clicked(move |button| {
        button.clipboard().set_text(&source.label());
        button.set_icon_name("object-select-symbolic");
        let button = button.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            button.set_icon_name("edit-copy-symbolic");
        });
    });
    copy
}

/// A row that shows one command in monospaced, selectable text, with a copy button that
/// copies exactly that command.
pub fn command_row(command: &str) -> (adw::ActionRow, gtk::Label) {
    let row = action_row("", "");
    let label = gtk::Label::builder()
        .label(command)
        .selectable(true)
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .build();
    label.add_css_class("monospace");
    row.add_prefix(&label);
    row.add_suffix(&copy_button(&label));
    (row, label)
}

pub fn button(label: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .build()
}

/// A row with a button on its right that the whole row activates.
pub fn button_row(title: &str, subtitle: &str, label: &str) -> (adw::ActionRow, gtk::Button) {
    let row = action_row(title, subtitle);
    let button = button(label);
    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));
    (row, button)
}

/// A row that opens something elsewhere, with the arrow GNOME Settings shows for it.
pub fn link_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = action_row(title, subtitle);
    row.set_activatable(true);
    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    row
}

/// A switch whose changes by the user run `changed`, and which [`Switch::set`] moves without
/// running it.
#[derive(Clone)]
pub struct Switch {
    pub row: adw::SwitchRow,
    quiet: Rc<Cell<bool>>,
}

impl Switch {
    pub fn new(title: &str, subtitle: &str, changed: impl Fn(bool) + 'static) -> Switch {
        let row = adw::SwitchRow::builder()
            .title(title)
            .use_markup(false)
            .build();
        if !subtitle.is_empty() {
            row.set_subtitle(subtitle);
        }
        let quiet = Rc::new(Cell::new(false));
        let q = quiet.clone();
        row.connect_active_notify(move |row| {
            if !q.get() {
                changed(row.is_active());
            }
        });
        Switch { row, quiet }
    }

    pub fn set(&self, on: bool) {
        self.quiet.set(true);
        self.row.set_active(on);
        self.quiet.set(false);
    }
}

/// A drop-down list whose choices by the user run `changed`, and which [`Combo::set`] moves
/// without running it.
#[derive(Clone)]
pub struct Combo {
    pub row: adw::ComboRow,
    quiet: Rc<Cell<bool>>,
}

impl Combo {
    pub fn new(title: &str, subtitle: &str, changed: impl Fn(u32) + 'static) -> Combo {
        let row = adw::ComboRow::builder()
            .title(title)
            .use_markup(false)
            .build();
        if !subtitle.is_empty() {
            row.set_subtitle(subtitle);
        }
        let quiet = Rc::new(Cell::new(false));
        let q = quiet.clone();
        row.connect_selected_notify(move |row| {
            if !q.get() && row.selected() != gtk::INVALID_LIST_POSITION {
                changed(row.selected());
            }
        });
        Combo { row, quiet }
    }

    /// Replaces the choices with `labels` and selects `selected`.
    pub fn set(&self, labels: &[String], selected: u32) {
        self.quiet.set(true);
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.row.set_model(Some(&gtk::StringList::new(&refs)));
        self.row.set_selected(selected);
        self.quiet.set(false);
    }
}

/// Rows added to a group that are replaced together.
pub struct Rows {
    pub group: adw::PreferencesGroup,
    rows: RefCell<Vec<gtk::Widget>>,
}

impl Rows {
    pub fn new(group: adw::PreferencesGroup) -> Rc<Rows> {
        Rc::new(Rows {
            group,
            rows: RefCell::new(Vec::new()),
        })
    }

    pub fn clear(&self) {
        for row in self.rows.borrow_mut().drain(..) {
            self.group.remove(&row);
        }
    }

    pub fn add(&self, row: &impl IsA<gtk::Widget>) {
        self.group.add(row);
        self.rows.borrow_mut().push(row.clone().upcast());
    }

    /// Adds a row that only says `text`, for an empty list.
    pub fn note(&self, text: &str) {
        let row = action_row(text, "");
        row.add_css_class("dim-label");
        self.add(&row);
    }
}

/// An icon from PNG bytes, or `fallback` from the icon theme when there are none.
pub fn png_image(png: &[u8], fallback: &str, size: i32) -> gtk::Image {
    let texture = (!png.is_empty())
        .then(|| gtk::gdk::Texture::from_bytes(&glib::Bytes::from(png)).ok())
        .flatten();
    let image = match texture {
        Some(texture) => gtk::Image::from_paintable(Some(&texture)),
        None => gtk::Image::from_icon_name(fallback),
    };
    image.set_pixel_size(size);
    image
}

/// Asks before doing something that cannot be undone, and runs `then` when the user agrees.
pub fn confirm(
    ctx: &Shared,
    heading: &str,
    body: &str,
    action: &str,
    destructive: bool,
    then: impl Fn() + 'static,
) {
    let dialog = adw::MessageDialog::new(Some(&ctx.window), Some(heading), Some(body));
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("ok", action);
    if destructive {
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
    } else {
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    }
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "ok" {
            then();
        }
    });
    dialog.present();
}

/// Tells the user something that needs their attention.
pub fn tell(ctx: &Shared, heading: &str, body: &str) {
    let dialog = adw::MessageDialog::new(Some(&ctx.window), Some(heading), Some(body));
    dialog.add_response("ok", &tr("OK"));
    dialog.present();
}

/// Opens a folder in the file manager, or says why it cannot.
pub fn open_folder(ctx: &Shared, path: &std::path::Path) {
    if !path.is_dir() {
        tell(
            ctx,
            &tr("The folder could not be opened"),
            &crate::i18n::trf(
                "{0} does not exist yet, or you may not open it.",
                &[&path.display().to_string()],
            ),
        );
        return;
    }
    let uri = gio::File::for_path(path).uri();
    open_uri(ctx, &uri);
}

/// Opens `uri` in the program the desktop opens it with.
pub fn open_uri(ctx: &Shared, uri: &str) {
    if let Err(error) = gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>) {
        tell(ctx, &tr("The folder could not be opened"), error.message());
    }
}

/// Starts a program of the desktop and does not wait for it. Returns why it could not.
pub fn start_program(command: &[&str]) -> Result<(), String> {
    let (program, args) = command.split_first().ok_or("no program")?; // not translated
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{program}: {e}"))
}
