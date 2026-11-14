//! The macOS widget panels through fakes: the widgets' watch of the window manager, run
//! against the local API service with a fake Android, drives the widget desk as the window
//! manager's main thread does, on a Retina screen (backing scale 2), with its look, its
//! points and its native menu's items. What the test does with the pointer is what the panel's
//! view passes on; what reaches the fake Android is what the service would tell the guest.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids, RunningService};
use dsa_host_core::widget_windows::desk::{Button, Desk, Effect, Key};
use dsa_host_core::widget_windows::paint::Image;
use dsa_host_core::widget_windows::{
    android_density, edit_margin, WidgetWindows, WindowCommand, WindowReport,
};
use dsa_host_core::widgets::WidgetProvider;
use dsa_host_macos::wm::widget_panels::{
    desk_point, dpi_of_backing, look, menu_items, panel_frame, MenuItem,
};
use dsa_protocol::control::PointerAction;

const WAIT: Duration = Duration::from_secs(10);

/// The height of the main screen in points, for the panels' frames.
const MAIN_HEIGHT: f64 = 900.0;

/// The widget desk as the window manager's main thread holds it, and what it asked of the
/// desktop.
#[derive(Clone)]
struct DeskWindows {
    desk: Arc<Mutex<Desk>>,
    effects: Arc<Mutex<Vec<Effect>>>,
}

impl DeskWindows {
    fn new(scale: f64) -> DeskWindows {
        let decode = Box::new(|png: &[u8]| {
            dsa_host_core::png::decode(png)
                .map(|image| Image::from_rgba(image.width, image.height, &image.pixels))
        });
        DeskWindows {
            desk: Arc::new(Mutex::new(Desk::new(
                look(false, 0),
                dpi_of_backing(scale),
                false,
                decode,
            ))),
            effects: Arc::default(),
        }
    }

    /// Runs `f` on the desk, then keeps what it asked of the desktop.
    fn with<R>(&self, f: impl FnOnce(&mut Desk) -> R) -> R {
        let mut desk = self.desk.lock().unwrap();
        let out = f(&mut desk);
        let effects = desk.take_effects();
        self.effects.lock().unwrap().extend(effects);
        out
    }

    fn take_effects(&self) -> Vec<Effect> {
        std::mem::take(&mut *self.effects.lock().unwrap())
    }
}

impl WidgetWindows for DeskWindows {
    fn command(&mut self, command: WindowCommand) {
        self.with(|desk| desk.command(command, Instant::now()));
    }

    fn reports(&mut self) -> Vec<WindowReport> {
        self.desk.lock().unwrap().take_reports()
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "{what} never happened");
        thread::sleep(Duration::from_millis(10));
    }
}

fn weather() -> WidgetProvider {
    WidgetProvider {
        provider: "com.example.weather/.WeatherWidget".into(),
        package_name: "com.example.weather".into(),
        label: "Weather".into(),
        min_width_dp: 110,
        min_height_dp: 40,
        default_width_dp: 180,
        default_height_dp: 110,
        resize_mode: 3,
        reconfigurable: true,
        ..WidgetProvider::default()
    }
}

/// A service with the caller's Android running and offering the weather widget, placed.
fn placed() -> (
    RunningService,
    Arc<FakeAndroid>,
    i32,
    impl Fn() -> Box<dyn dsa_transport::Connector>,
) {
    let (listener, connector) = dsa_transport::local::pair();
    let androids = Arc::new(FakeAndroids::default());
    let service = RunningService::start(Box::new(listener), androids.clone());
    let mut client =
        HostApiClient::connect(&mut connector.clone(), "test", WAIT).expect("connecting");
    client.start().expect("starting Android");
    let android = androids
        .androids
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .expect("the caller's Android");
    android.widget_offer.lock().unwrap().push(weather());
    let widget = client
        .add_widget(&weather().provider, WAIT)
        .expect("placing the widget");
    let connect = move || Box::new(connector.clone()) as Box<dyn dsa_transport::Connector>;
    (service, android, widget.widget_id, connect)
}

#[test]
fn fr12_long_pressing_a_widget_edits_it_like_a_home_screen() {
    let (_service, android, widget_id, connect) = placed();
    let windows = DeskWindows::new(2.0);
    let stop = Arc::new(AtomicBool::new(false));
    let syncing = {
        let mut windows = windows.clone();
        let stop = Arc::clone(&stop);
        let connector = connect();
        thread::spawn(move || {
            dsa_host_core::widget_windows::sync(
                connector,
                &mut windows,
                stop,
                "widget-test",
                Arc::new(|_: &str| {}),
            )
        })
    };

    // The widget's panel opens on the desktop at its place, 180 x 110 dp on a Retina screen,
    // where Android draws at 262 dpi: 295 x 180 pixels, 147.5 x 90 points.
    until("the widget's window opening", || {
        windows.with(|desk| desk.window(widget_id).is_some())
    });
    let opened = windows.take_effects();
    let Some(Effect::Open { origin, size, .. }) = opened
        .iter()
        .find(|e| matches!(e, Effect::Open { widget_id: id, .. } if *id == widget_id))
        .cloned()
    else {
        panic!("no window opened: {opened:?}");
    };
    assert_eq!(size, (295, 180));
    assert_eq!(origin, (64, 64), "where the service placed it");
    let (x, y, width, height) = panel_frame(origin, size, MAIN_HEIGHT, 2.0);
    assert_eq!((x, width, height), (32.0, 147.5, 90.0));
    assert_eq!(
        y,
        MAIN_HEIGHT - 32.0 - 90.0,
        "from the top of the main screen"
    );
    // The pointer on the screen, at the panel's top left corner, is there in the desk's pixels.
    assert_eq!(
        desk_point((32.0, MAIN_HEIGHT - 32.0), MAIN_HEIGHT, 2.0),
        origin
    );

    // A press held still for half a second lifts it: Android's press is cancelled.
    let now = Instant::now();
    windows.with(|desk| desk.down(widget_id, (60, 60), Button::Primary, false, now));
    windows.with(|desk| desk.tick(now + Duration::from_millis(500)));
    assert!(windows.with(|desk| desk.window(widget_id).unwrap().editing()));
    until("Android's press being cancelled", || {
        let pointers = android.widget_pointers.lock().unwrap();
        pointers.len() >= 2
    });
    let actions: Vec<i32> = android
        .widget_pointers
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.action)
        .collect();
    assert_eq!(
        actions,
        vec![PointerAction::Down as i32, PointerAction::Cancel as i32]
    );
    let lifted = windows.take_effects();
    assert!(
        lifted.contains(&Effect::Focus { widget_id }),
        "it takes the keyboard"
    );

    // Letting go opens the menu under it, as an NSMenu shows it: Widget settings, then Resize
    // with its sizes as a submenu, then Remove.
    let margin = edit_margin(dpi_of_backing(2.0));
    windows.with(|desk| desk.up(widget_id, (60 + margin, 60 + margin), Button::Primary));
    let menu = windows.take_effects();
    let Some(Effect::Menu { entries, .. }) = menu
        .iter()
        .find(|e| matches!(e, Effect::Menu { .. }))
        .cloned()
    else {
        panic!("no menu opened: {menu:?}");
    };
    let shown = menu_items(&entries);
    assert!(matches!(&shown[0], MenuItem::Item { title, .. } if title == "Widget settings"));
    assert!(matches!(&shown[1], MenuItem::Submenu { title, .. } if title == "Resize"));
    assert!(matches!(shown.last(), Some(MenuItem::Item { title, .. }) if title == "Remove"));
    let MenuItem::Item { tag, .. } = &shown[0] else {
        unreachable!()
    };

    // Widget settings opens its setup in Android again.
    let command = *tag as u32;
    windows.with(|desk| desk.menu_closed(widget_id, Some(command), false));
    until("the widget's settings reaching Android", || {
        !android.widget_reconfigures.lock().unwrap().is_empty()
    });
    assert_eq!(
        *android.widget_reconfigures.lock().unwrap(),
        vec![widget_id]
    );
    assert!(!windows.with(|desk| desk.window(widget_id).unwrap().editing()));

    // Lifted again and dragged, the widget moves; letting go tells the service where, in dp
    // at Android's density on the screen, 262 on a Retina screen.
    let sizes_before = android.widget_sizes.lock().unwrap().len();
    windows.with(|desk| desk.down(widget_id, (60, 60), Button::Primary, false, now));
    windows.with(|desk| desk.tick(now + Duration::from_millis(600)));
    windows.with(|desk| desk.moved(widget_id, (60 + margin + 40, 60 + margin + 20)));
    windows.with(|desk| {
        desk.up(
            widget_id,
            (60 + margin + 40, 60 + margin + 20),
            Button::Primary,
        )
    });
    assert_eq!(
        windows.with(|desk| desk.window(widget_id).unwrap().origin),
        (104, 84)
    );
    until("the new place reaching Android", || {
        android.widget_sizes.lock().unwrap().len() > sizes_before
    });
    let (id, told) = *android.widget_sizes.lock().unwrap().last().unwrap();
    assert_eq!(id, widget_id);
    assert_eq!(
        (told.width_dp, told.height_dp, told.density_dpi),
        (180, 110, android_density(dpi_of_backing(2.0)))
    );

    // Esc ends an edit without a choice.
    windows.with(|desk| desk.down(widget_id, (60, 60), Button::Primary, false, now));
    windows.with(|desk| desk.tick(now + Duration::from_millis(600)));
    windows.with(|desk| desk.up(widget_id, (60 + margin, 60 + margin), Button::Primary));
    windows.with(|desk| desk.menu_closed(widget_id, None, false));
    windows.with(|desk| desk.key(widget_id, Key::Escape));
    assert!(!windows.with(|desk| desk.window(widget_id).unwrap().editing()));

    stop.store(true, Ordering::Relaxed);
    syncing.join().unwrap();
}

#[test]
fn fr12_right_clicking_a_widget_resizes_or_removes_it() {
    let (_service, android, widget_id, connect) = placed();
    let windows = DeskWindows::new(1.0);
    let stop = Arc::new(AtomicBool::new(false));
    let syncing = {
        let mut windows = windows.clone();
        let stop = Arc::clone(&stop);
        let connector = connect();
        thread::spawn(move || {
            dsa_host_core::widget_windows::sync(
                connector,
                &mut windows,
                stop,
                "widget-test",
                Arc::new(|_: &str| {}),
            )
        })
    };
    until("the widget's window opening", || {
        windows.with(|desk| desk.window(widget_id).is_some())
    });
    windows.take_effects();
    // A right click opens the menu at the pointer; its Large size makes the widget one and a
    // half times its default, which Android is told.
    windows.with(|desk| {
        desk.down(
            widget_id,
            (20, 10),
            Button::Secondary,
            false,
            Instant::now(),
        )
    });
    let effects = windows.take_effects();
    let Some(Effect::Menu { entries, .. }) = effects
        .iter()
        .find(|e| matches!(e, Effect::Menu { .. }))
        .cloned()
    else {
        panic!("no menu: {effects:?}");
    };
    let shown = menu_items(&entries);
    let Some(MenuItem::Submenu { items: sizes, .. }) = shown
        .iter()
        .find(|item| matches!(item, MenuItem::Submenu { title, .. } if title == "Resize"))
    else {
        panic!("no Resize in {shown:?}");
    };
    assert!(matches!(shown.last(), Some(MenuItem::Item { title, .. }) if title == "Remove"));
    let large = sizes
        .iter()
        .find_map(|item| match item {
            MenuItem::Item { tag, title, .. } if title.starts_with("Large") => Some(*tag as u32),
            _ => None,
        })
        .expect("a Large size");
    let before = android.widget_sizes.lock().unwrap().len();
    windows.with(|desk| desk.menu_closed(widget_id, Some(large), false));
    until("the new size reaching Android", || {
        android.widget_sizes.lock().unwrap().len() > before
    });
    let (_, told) = *android.widget_sizes.lock().unwrap().last().unwrap();
    assert_eq!(
        (told.width_dp, told.height_dp, told.density_dpi),
        (270, 165, android_density(dpi_of_backing(1.0)))
    );

    // Remove takes it off the desktop: its window closes once the service says it is gone.
    windows.with(|desk| {
        desk.down(
            widget_id,
            (20, 10),
            Button::Secondary,
            false,
            Instant::now(),
        )
    });
    windows.take_effects();
    windows.with(|desk| {
        desk.menu_closed(
            widget_id,
            Some(dsa_host_core::widget_windows::menu::REMOVE),
            false,
        )
    });
    until("the window closing", || {
        windows.with(|desk| desk.window(widget_id).is_none())
    });
    assert!(windows
        .take_effects()
        .contains(&Effect::Close { widget_id }));

    stop.store(true, Ordering::Relaxed);
    syncing.join().unwrap();
}
