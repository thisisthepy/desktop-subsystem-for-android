//! The window: a GNOME Settings window with the pages in a sidebar on the left and the page
//! on the right, which folds into one column on a narrow window. libadwaita gives it the
//! system's light or dark style and its accent colour.

mod apps;
mod common;
mod logs;
mod status;
mod system;

use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::api::{is_administrator, Elevated, LocalApi};
use crate::i18n::{tr, trf};
use crate::model::Client;
use crate::pages::Page;
use crate::view;
use common::{request, Ctx, Shared};

/// What the command line asked for: the page to open on, and APKs to install.
#[derive(Debug, Default, Clone)]
struct Start {
    page: Option<Page>,
    apks: Vec<String>,
}

fn parse_args(args: &[String]) -> Start {
    let mut start = Start::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--page" => start.page = args.next().and_then(|id| Page::from_id(id)),
            "--install-apk" => start.apks.extend(args.next().cloned()),
            other if is_install_file(other) => start.apks.push(other.to_string()),
            _ => {}
        }
    }
    start
}

/// Whether `path` names what the install takes: an APK, a split-APK bundle (`.xapk`, `.apks`,
/// `.apkm`), or an App Bundle, which it refuses with its reason.
fn is_install_file(path: &str) -> bool {
    dsa_host_core::apk_bundle::is_install_file(std::path::Path::new(path))
}

/// Runs the app until its window closes.
pub fn run() -> glib::ExitCode {
    crate::i18n::init();
    let start = parse_args(&std::env::args().skip(1).collect::<Vec<_>>());
    let app = adw::Application::builder()
        .application_id(crate::APP_ID)
        .build();
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        build(app, &start);
    });
    // GTK does not see the arguments; they were read above.
    app.run_with_args::<&str>(&[])
}

fn build(app: &adw::Application, start: &Start) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(tr("DSA Settings"))
        .icon_name(crate::APP_ID)
        .default_width(1040)
        .default_height(760)
        .width_request(360)
        .height_request(360)
        .build();
    let toasts = adw::ToastOverlay::new();
    let split = adw::NavigationSplitView::new();
    // As wide as the longest label needs and no wider, as the Windows app sizes its pane:
    // the row's 6 px margins and 10 px padding on each side, the 16 px icon, 12 px spacing,
    // and "Running in the background" at Cantarell 11 pt (about 183 px), rounded up to 4.
    split.set_min_sidebar_width(244.0);
    split.set_max_sidebar_width(244.0);
    toasts.set_child(Some(&split));
    window.set_content(Some(&toasts));

    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        640.0,
        adw::LengthUnit::Sp,
    ));
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(breakpoint);

    let client = Client::new(Arc::new(LocalApi), Arc::new(Elevated), !is_administrator());
    let ctx: Shared = Rc::new(Ctx {
        client,
        window: window.clone(),
        toasts,
    });

    // The sidebar.
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    for page in Page::ALL {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.append(&gtk::Image::from_icon_name(page.icon()));
        let label = gtk::Label::builder()
            .label(tr(page.title()))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        row.append(&label);
        list.append(&row);
    }
    let sidebar_view = adw::ToolbarView::new();
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_title_widget(Some(&adw::WindowTitle::new(&tr("DSA Settings"), "")));
    sidebar_view.add_top_bar(&sidebar_header);
    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    sidebar_view.set_content(Some(&sidebar_scroll));
    split.set_sidebar(Some(&adw::NavigationPage::new(
        &sidebar_view,
        &tr("DSA Settings"),
    )));

    {
        let (ctx, split) = (ctx.clone(), split.clone());
        list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let Some(page) = Page::ALL.get(row.index() as usize).copied() else {
                return;
            };
            show_page(&ctx, &split, page);
        });
    }
    let first = start.page.unwrap_or(Page::General);
    let index = Page::ALL.iter().position(|p| *p == first).unwrap_or(0);
    if let Some(row) = list.row_at_index(index as i32) {
        list.select_row(Some(&row));
    }
    if start.page.is_some() {
        split.set_show_content(true);
    }

    // APK files dropped anywhere on the window are installed in Android.
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    {
        let ctx = ctx.clone();
        drop.connect_drop(move |_, value, _, _| {
            let Ok(files) = value.get::<gdk::FileList>() else {
                return false;
            };
            let paths: Vec<String> = files
                .files()
                .iter()
                .filter_map(|f| f.path())
                .map(|p| p.display().to_string())
                .collect();
            install(&ctx, paths);
            true
        });
    }
    window.add_controller(drop);
    window.present();
    if !start.apks.is_empty() {
        install(&ctx, start.apks.clone());
    }
}

fn show_page(ctx: &Shared, split: &adw::NavigationSplitView, page: Page) {
    let content = match page {
        Page::General => status::general(ctx),
        Page::Memory => status::memory(ctx),
        Page::Apps => apps::apps(ctx),
        Page::Background => apps::background(ctx),
        Page::Widgets => apps::widgets(ctx),
        Page::Display => status::display(ctx),
        Page::Android => status::android(ctx),
        Page::Updates => status::updates(ctx),
        Page::Identity => system::identity(ctx),
        Page::Network => system::network(ctx),
        Page::Advanced => system::advanced(ctx),
        Page::Logs => logs::logs(ctx),
        Page::About => status::about(ctx),
    };
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&content));
    let title = tr(page.title());
    split.set_content(Some(&adw::NavigationPage::new(&view, &title)));
    split.set_show_content(true);
}

/// Installs the APKs and split-APK bundles at `paths` one after the other, starting Android
/// first when it is not running, and says how each went.
fn install(ctx: &Shared, paths: Vec<String>) {
    let (apks, others): (Vec<String>, Vec<String>) =
        paths.into_iter().partition(|p| is_install_file(p));
    if !others.is_empty() {
        common::tell(
            ctx,
            &tr("Not installed"),
            &trf(
                "Only APK files and split APK bundles can be installed: {0}",
                &[&others.join(", ")],
            ),
        );
    }
    for apk in apks {
        let name = std::path::Path::new(&apk)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| apk.clone());
        ctx.toast(&trf("Installing {0} in Android…", &[&name]));
        let ctx2 = ctx.clone();
        request(
            ctx,
            move |c| c.install(&apk),
            move |r| match r {
                Ok(outcome) if outcome.done => ctx2.toast(&trf(
                    "{0} is installed ({1}). Open it from the app grid.",
                    &[&name, outcome.package.as_deref().unwrap_or_default()],
                )),
                Ok(outcome) => common::tell(
                    &ctx2,
                    &tr("Not installed"),
                    &trf(
                        "{0} was not installed: {1} ({2})",
                        &[
                            &name,
                            &view::install_reason(&outcome.status),
                            &outcome.detail,
                        ],
                    ),
                ),
                Err(failure) => common::tell(
                    &ctx2,
                    &tr("Not installed"),
                    &trf(
                        "{0} did not reach Android. {1}",
                        &[&name, &view::problem_text(&failure)],
                    ),
                ),
            },
        );
    }
}
