//! A user's window settings for each app: whether the app's windows may be resized, the size
//! they open at, how the app follows its window to a monitor of another scale, and where its
//! full screen shows.
//!
//! The settings app's Apps page and `dsa app-window` change them through the local API; the
//! service keeps them beside the user's other settings, and the user's window manager reads
//! them when an app's window opens. An app with the defaults (resizable, opening at the size
//! Android gives its task, scaling its own way, under the unified title bar) has no entry.

use dsa_protocol::host_api::types;
use serde::{Deserialize, Serialize};

use crate::compositor::window_sizing::MIN_CLIENT_DP;
use crate::tasks::FullScreenRequest;

/// The apps whose windows DSA scales the system way unless the user chose otherwise: apps that
/// break when their density changes while they run, so that only restarting their process
/// would repair them. [`DpiScaling::Auto`] is [`DpiScaling::System`] for these, and for every
/// other app it follows what the app says of a change of density. This is the only copy of the
/// list: the window
/// manager, `dsa app-window` and, through it, the settings apps all read it from here.
pub const SYSTEM_SCALED_BY_DEFAULT: &[&str] = &[
    // YouTube keeps values that depend on the density in its process. Moved to a monitor of
    // another scale, its bottom tabs are squashed and covered by the content, and its
    // miniplayer turns green, whether it handles the new density itself or its activity is
    // recreated; only a restart of its process draws it right again. Scaled the system way it
    // stays whole, and its text is sharp enough on a 125% monitor (measured on 2026-11-14).
    "com.google.android.youtube",
];

/// How an app follows its window to a monitor of another scale, as a user chooses it, the way
/// Windows lets a user choose who performs the high DPI scaling of a desktop program. The
/// default and [`DpiScaling::System`] never restart the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DpiScaling {
    /// [`DpiScaling::System`] for an app in [`SYSTEM_SCALED_BY_DEFAULT`]
    /// ([`DpiScaling::default_for`]); for any other, [`DpiScaling::App`] for an app whose
    /// activity says it handles a change of density itself, [`DpiScaling::System`] for one
    /// that does not.
    #[default]
    Auto,
    /// The task takes the new monitor's density, and the app redraws at it: its text stays
    /// sharp. An app that does not say it handles a change of density is given it too: Android
    /// recreates its activity, which restores its saved state (scroll position, typed text),
    /// so it may redraw briefly when it moves, as a Windows program does with "application".
    App,
    /// The task keeps its density, and the window manager scales the window's picture to the
    /// new monitor, so the app keeps its size on the screen. Scaled up, text can look a little
    /// soft.
    System,
    /// The app is given the new density without a restart, even though it does not say it
    /// handles one: Android's framework, in DSA's image, is told to treat a change of density
    /// as handled by the task's activity instead of relaunching it, and the app's process
    /// converts the pixel sizes its views took at the old density (layout parameters, padding,
    /// text, drawables) to the new one. Its window then follows its monitor as
    /// [`DpiScaling::App`] does. A size the app keeps in its own fields stays at the old density
    /// until the app reads it again, which is why it is a choice per app and never a default. On
    /// an image whose framework cannot do it ([`DpiScaling::mode_on`]), it works as
    /// [`DpiScaling::System`] does.
    SystemForced,
}

/// What a task's window does on a monitor of another scale, once the user's choice and the
/// app's own say are taken together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingMode {
    /// The task takes each monitor's density.
    App,
    /// The task keeps its density and the window is scaled.
    System,
}

impl DpiScaling {
    /// Whether [`DpiScaling::SystemForced`] does what it says. It needs DSA's framework
    /// change, which the image carries; an image without it falls back to
    /// [`DpiScaling::System`] ([`DpiScaling::mode_on`]).
    pub const FORCED_AVAILABLE: bool = true;

    /// Every choice, in the order the settings apps list them.
    pub const ALL: [DpiScaling; 4] = [
        DpiScaling::Auto,
        DpiScaling::App,
        DpiScaling::System,
        DpiScaling::SystemForced,
    ];

    /// The name `dsa app-window --dpi-scaling` takes and prints.
    pub fn name(self) -> &'static str {
        match self {
            DpiScaling::Auto => "auto",
            DpiScaling::App => "app",
            DpiScaling::System => "system",
            DpiScaling::SystemForced => "system-forced",
        }
    }

    /// The choice `name` names, as [`DpiScaling::name`] gives it.
    pub fn from_name(name: &str) -> Option<DpiScaling> {
        DpiScaling::ALL
            .into_iter()
            .find(|scaling| scaling.name() == name)
    }

    /// What [`DpiScaling::Auto`] is for `package`: [`DpiScaling::System`] for an app in
    /// [`SYSTEM_SCALED_BY_DEFAULT`], else [`DpiScaling::Auto`] itself, which follows what the
    /// app's activity says of a change of density ([`DpiScaling::mode`]).
    pub fn default_for(package: &str) -> DpiScaling {
        if SYSTEM_SCALED_BY_DEFAULT.contains(&package) {
            DpiScaling::System
        } else {
            DpiScaling::Auto
        }
    }

    /// This choice for `package`, with [`DpiScaling::Auto`] as DSA's choice for that app.
    pub fn resolved_for(self, package: &str) -> DpiScaling {
        match self {
            DpiScaling::Auto => DpiScaling::default_for(package),
            chosen => chosen,
        }
    }

    /// What the window of a task of `package` does with this choice, for a task whose top
    /// activity does (`handles_density_change`) or does not say it handles a change of
    /// density itself: what [`DpiScaling::mode`] says of [`DpiScaling::resolved_for`].
    pub fn mode_for(self, package: &str, handles_density_change: bool) -> ScalingMode {
        self.resolved_for(package).mode(handles_density_change)
    }

    /// [`DpiScaling::mode_for`] on a guest that can (`forced_density_available`) or cannot
    /// give a task a new density without relaunching its activity. Where it cannot,
    /// [`DpiScaling::SystemForced`] works as [`DpiScaling::System`] does: following the
    /// monitor there would restart the app, which is what the user chose it to avoid. Every
    /// other choice is as [`DpiScaling::mode_for`] says.
    pub fn mode_on(
        self,
        package: &str,
        handles_density_change: bool,
        forced_density_available: bool,
    ) -> ScalingMode {
        match self.resolved_for(package) {
            DpiScaling::SystemForced if !forced_density_available => ScalingMode::System,
            resolved => resolved.mode(handles_density_change),
        }
    }

    /// Whether the density requests of a task of `package` with this choice go out forced
    /// (the guest gives the density without relaunching the activity): for
    /// [`DpiScaling::SystemForced`] on a guest that can, and for nothing else.
    pub fn forced_on(self, package: &str, forced_density_available: bool) -> bool {
        DpiScaling::FORCED_AVAILABLE
            && forced_density_available
            && self.resolved_for(package) == DpiScaling::SystemForced
    }

    /// What a task's window does with this choice, for a task whose top activity does
    /// (`handles_density_change`) or does not say it handles a change of density itself, and
    /// whose app is outside [`SYSTEM_SCALED_BY_DEFAULT`] ([`DpiScaling::mode_for`] takes the
    /// list into account).
    ///
    /// Android relaunches an activity that does not say it handles a change of density when
    /// its density changes, and the default must never restart an app. So
    /// [`DpiScaling::Auto`] gives another density only to an activity that says it handles
    /// one. [`DpiScaling::App`] is the user's explicit choice to stay sharp at the cost of
    /// that relaunch, so it gives the new density to any app. [`DpiScaling::SystemForced`]
    /// gives it too, without the relaunch, on a guest whose framework can
    /// ([`DpiScaling::mode_on`] falls back to [`DpiScaling::System`] where it cannot).
    pub fn mode(self, handles_density_change: bool) -> ScalingMode {
        match self {
            DpiScaling::App => ScalingMode::App,
            DpiScaling::Auto if handles_density_change => ScalingMode::App,
            DpiScaling::SystemForced if DpiScaling::FORCED_AVAILABLE => ScalingMode::App,
            DpiScaling::Auto | DpiScaling::System | DpiScaling::SystemForced => ScalingMode::System,
        }
    }

    fn from_wire(value: i32) -> DpiScaling {
        match types::DpiScaling::try_from(value) {
            Ok(types::DpiScaling::App) => DpiScaling::App,
            Ok(types::DpiScaling::System) => DpiScaling::System,
            Ok(types::DpiScaling::SystemForced) => DpiScaling::SystemForced,
            Ok(types::DpiScaling::Auto) | Err(_) => DpiScaling::Auto,
        }
    }

    fn to_wire(self) -> i32 {
        match self {
            DpiScaling::Auto => types::DpiScaling::Auto,
            DpiScaling::App => types::DpiScaling::App,
            DpiScaling::System => types::DpiScaling::System,
            DpiScaling::SystemForced => types::DpiScaling::SystemForced,
        }
        .into()
    }
}

/// Where a window shows an app that asks to fill the screen, as a user chooses it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FullScreenPlace {
    /// As the default says: for one app, the user's choice for every app
    /// ([`FullScreenDefaults`]); for every app, what DSA does without a choice.
    #[default]
    Default,
    /// On the whole screen: the window fills its monitor, and its title bar shows while the
    /// pointer is at the top edge, so minimize and close stay in reach.
    Screen,
    /// Inside the window: it keeps its size and its title bar, and the app fills it with no
    /// title band of its own.
    Window,
}

impl FullScreenPlace {
    /// Every choice, in the order the settings apps list them.
    pub const ALL: [FullScreenPlace; 3] = [
        FullScreenPlace::Default,
        FullScreenPlace::Screen,
        FullScreenPlace::Window,
    ];

    /// The name `dsa app-window --fullscreen` and `dsa settings set app-fullscreen` take and
    /// print.
    pub fn name(self) -> &'static str {
        match self {
            FullScreenPlace::Default => "default",
            FullScreenPlace::Screen => "screen",
            FullScreenPlace::Window => "window",
        }
    }

    /// The choice `name` names, as [`FullScreenPlace::name`] gives it.
    pub fn from_name(name: &str) -> Option<FullScreenPlace> {
        FullScreenPlace::ALL
            .into_iter()
            .find(|place| place.name() == name)
    }

    /// This choice, or `default` when it leaves the choice to the default.
    pub fn or(self, default: FullScreenPlace) -> FullScreenPlace {
        match self {
            FullScreenPlace::Default => default,
            chosen => chosen,
        }
    }

    pub fn from_wire(value: i32) -> FullScreenPlace {
        match types::FullScreenPlace::try_from(value) {
            Ok(types::FullScreenPlace::Screen) => FullScreenPlace::Screen,
            Ok(types::FullScreenPlace::Window) => FullScreenPlace::Window,
            Ok(types::FullScreenPlace::Default) | Err(_) => FullScreenPlace::Default,
        }
    }

    pub fn to_wire(self) -> i32 {
        match self {
            FullScreenPlace::Default => types::FullScreenPlace::Default,
            FullScreenPlace::Screen => types::FullScreenPlace::Screen,
            FullScreenPlace::Window => types::FullScreenPlace::Window,
        }
        .into()
    }
}

/// The user's full screen choices for every app, one for each kind of request, which an
/// app's own choice overrides.
///
/// A clear request (a video player's full screen button, a game that starts full screen)
/// fills the whole screen unless the user chose otherwise. An app that only hides its bars
/// (a feed of short videos) stays inside its window: the user opened it as a window, and the
/// app did not ask to leave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullScreenDefaults {
    /// Where a clear full screen request is shown.
    pub fullscreen: FullScreenPlace,
    /// Where an app that hides both its bars alone is shown.
    pub immersive: FullScreenPlace,
}

impl Default for FullScreenDefaults {
    fn default() -> FullScreenDefaults {
        FullScreenDefaults {
            fullscreen: FullScreenPlace::Screen,
            immersive: FullScreenPlace::Window,
        }
    }
}

impl FullScreenDefaults {
    /// These choices with each part left to the default ([`FullScreenPlace::Default`]) as
    /// DSA's own default for it, so neither part is ever `Default`.
    pub fn resolved(self) -> FullScreenDefaults {
        let builtin = FullScreenDefaults::default();
        FullScreenDefaults {
            fullscreen: self.fullscreen.or(builtin.fullscreen),
            immersive: self.immersive.or(builtin.immersive),
        }
    }

    /// The choices a local API message carries.
    pub fn from_wire(defaults: Option<&types::FullScreenDefaults>) -> FullScreenDefaults {
        let Some(defaults) = defaults else {
            return FullScreenDefaults::default();
        };
        FullScreenDefaults {
            fullscreen: FullScreenPlace::from_wire(defaults.fullscreen),
            immersive: FullScreenPlace::from_wire(defaults.immersive),
        }
        .resolved()
    }

    /// The local API message that carries these choices.
    pub fn to_wire(self) -> types::FullScreenDefaults {
        let resolved = self.resolved();
        types::FullScreenDefaults {
            fullscreen: resolved.fullscreen.to_wire(),
            immersive: resolved.immersive.to_wire(),
        }
    }
}

/// Where a window shows a task whose app asks for `request`: the app's own choice for that
/// kind of request (`app`), else the user's choice for every app (`defaults`). `None` while
/// the app asks for no full screen. Never [`FullScreenPlace::Default`].
pub fn full_screen_place(
    request: FullScreenRequest,
    app: Option<&AppWindowSettings>,
    defaults: FullScreenDefaults,
) -> Option<FullScreenPlace> {
    let defaults = defaults.resolved();
    let (own, default) = match request {
        FullScreenRequest::None => return None,
        FullScreenRequest::Explicit => (app.map(|app| app.fullscreen), defaults.fullscreen),
        FullScreenRequest::BarsHidden => (app.map(|app| app.immersive), defaults.immersive),
    };
    Some(own.unwrap_or_default().or(default))
}

/// The user's window settings for every app that has any, with the full screen choices every
/// app follows unless it has its own: what a window manager reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppWindows {
    pub apps: Vec<AppWindowSettings>,
    pub defaults: FullScreenDefaults,
}

/// The largest start size accepted, in density-independent pixels in each direction: more
/// than any monitor shows at 100%.
pub const MAX_START_DP: u32 = 8192;

/// The most apps with settings kept for one user.
pub const MAX_APPS: usize = 4096;

/// One app's window settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppWindowSettings {
    pub package_name: String,
    /// Whether the app's windows keep the size they open at.
    #[serde(default)]
    pub fixed_size: bool,
    /// The size the app's windows open at, in density-independent pixels; `None` opens them
    /// at the size Android gives the task.
    #[serde(default)]
    pub start_size_dp: Option<(u32, u32)>,
    /// How the app follows its window to a monitor of another scale.
    #[serde(default)]
    pub dpi_scaling: DpiScaling,
    /// Where the app's windows show a clear full screen request.
    #[serde(default)]
    pub fullscreen: FullScreenPlace,
    /// Where the app's windows show the app hiding both its bars alone.
    #[serde(default)]
    pub immersive: FullScreenPlace,
    /// Whether the app starts below the title bar: its content begins under the band, which
    /// the window paints itself, and the app has no top inset to handle. For an app whose top
    /// bar is squeezed or hidden under the unified title bar. `None` leaves it to DSA's
    /// choice for the app ([`starts_below_title_bar`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below_title_bar: Option<bool>,
}

/// The apps that start below the title bar unless the user chose otherwise. None for now:
/// the unified title bar is the default for every app. An app found to need the switch (as
/// [`SYSTEM_SCALED_BY_DEFAULT`] lists the apps that need the system's scaling) is added here;
/// `dsa app-window` reports the list as `below_title_bar_by_default`.
pub const BELOW_TITLE_BAR_BY_DEFAULT: &[&str] = &[];

/// Whether a task of `package` starts below the title bar: the user's choice for the app
/// (`app`), else whether [`BELOW_TITLE_BAR_BY_DEFAULT`] lists it.
pub fn starts_below_title_bar(package: &str, app: Option<&AppWindowSettings>) -> bool {
    app.and_then(|app| app.below_title_bar)
        .unwrap_or_else(|| BELOW_TITLE_BAR_BY_DEFAULT.contains(&package))
}

/// The packages whose tasks start below the title bar, sorted, each once: what the guest is
/// told, from the user's settings for every app (`apps`) and [`BELOW_TITLE_BAR_BY_DEFAULT`].
pub fn below_title_bar_packages(apps: &[AppWindowSettings]) -> Vec<String> {
    let chosen = |package: &str| apps.iter().find(|app| app.package_name == package);
    let mut packages: Vec<String> = BELOW_TITLE_BAR_BY_DEFAULT
        .iter()
        .map(|package| package.to_string())
        .chain(apps.iter().map(|app| app.package_name.clone()))
        .filter(|package| starts_below_title_bar(package, chosen(package)))
        .collect();
    packages.sort();
    packages.dedup();
    packages
}

impl AppWindowSettings {
    /// Whether these are the defaults, which need no entry.
    pub fn is_default(&self) -> bool {
        !self.fixed_size
            && self.start_size_dp.is_none()
            && self.dpi_scaling == DpiScaling::Auto
            && self.fullscreen == FullScreenPlace::Default
            && self.immersive == FullScreenPlace::Default
            && self.below_title_bar.is_none()
    }

    /// Checks the settings, as a client sent them: the package must be an Android package
    /// name, and a start size at least the smallest window and at most [`MAX_START_DP`].
    pub fn checked(self) -> Result<AppWindowSettings, String> {
        if !crate::compositor::window_hosts::is_valid_package(&self.package_name) {
            return Err(format!(
                "{:?} is not an Android package name",
                self.package_name
            ));
        }
        if let Some((width, height)) = self.start_size_dp {
            let fits = |length: u32| (MIN_CLIENT_DP..=MAX_START_DP).contains(&length);
            if !fits(width) || !fits(height) {
                return Err(format!(
                    "a start size of {width} x {height} is not between {MIN_CLIENT_DP} and \
                     {MAX_START_DP} in each direction"
                ));
            }
        }
        Ok(self)
    }

    /// The settings a local API message carries; a start size of 0 by 0 is none.
    pub fn from_wire(app: &types::AppWindowSettings) -> AppWindowSettings {
        AppWindowSettings {
            package_name: app.package_name.clone(),
            fixed_size: app.fixed_size,
            start_size_dp: (app.start_width_dp > 0 || app.start_height_dp > 0)
                .then_some((app.start_width_dp, app.start_height_dp)),
            dpi_scaling: DpiScaling::from_wire(app.dpi_scaling),
            fullscreen: FullScreenPlace::from_wire(app.fullscreen),
            immersive: FullScreenPlace::from_wire(app.immersive),
            // The wire carries the switch as it works. On is a choice; off is one only for an
            // app DSA would start below the title bar on its own.
            below_title_bar: (app.below_title_bar
                != BELOW_TITLE_BAR_BY_DEFAULT.contains(&app.package_name.as_str()))
            .then_some(app.below_title_bar),
        }
    }

    /// The local API message that carries these settings.
    pub fn to_wire(&self) -> types::AppWindowSettings {
        let (start_width_dp, start_height_dp) = self.start_size_dp.unwrap_or((0, 0));
        types::AppWindowSettings {
            package_name: self.package_name.clone(),
            fixed_size: self.fixed_size,
            start_width_dp,
            start_height_dp,
            dpi_scaling: self.dpi_scaling.to_wire(),
            fullscreen: self.fullscreen.to_wire(),
            immersive: self.immersive.to_wire(),
            below_title_bar: starts_below_title_bar(&self.package_name, Some(self)),
        }
    }
}

/// `apps` with `app` in place of its package's entry: removed when `app` has the defaults,
/// otherwise added or replaced. The list stays sorted by package name.
pub fn replace(
    apps: &[AppWindowSettings],
    app: AppWindowSettings,
) -> Result<Vec<AppWindowSettings>, String> {
    let mut list: Vec<AppWindowSettings> = apps
        .iter()
        .filter(|known| known.package_name != app.package_name)
        .cloned()
        .collect();
    if !app.is_default() {
        if list.len() >= MAX_APPS {
            return Err(format!(
                "window settings are kept for {MAX_APPS} apps at most; reset another app's first"
            ));
        }
        list.push(app);
    }
    list.sort_by(|a, b| a.package_name.cmp(&b.package_name));
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes(fixed_size: bool, start: Option<(u32, u32)>) -> AppWindowSettings {
        AppWindowSettings {
            package_name: "com.example.notes".into(),
            fixed_size,
            start_size_dp: start,
            ..AppWindowSettings::default()
        }
    }

    #[test]
    fn fr4_an_explicit_app_choice_always_takes_the_density() {
        assert_eq!(DpiScaling::Auto.mode(true), ScalingMode::App);
        assert_eq!(DpiScaling::Auto.mode(false), ScalingMode::System);
        assert_eq!(DpiScaling::App.mode(true), ScalingMode::App);
        // The user chose to stay sharp: Android recreates the activity and restores its state.
        assert_eq!(DpiScaling::App.mode(false), ScalingMode::App);
        assert_eq!(DpiScaling::System.mode(false), ScalingMode::System);
        assert_eq!(DpiScaling::System.mode(true), ScalingMode::System);
    }

    #[test]
    fn fr4_forced_follows_the_monitor_without_a_relaunch_where_the_guest_can() {
        const { assert!(DpiScaling::FORCED_AVAILABLE) };
        // Its window follows the monitor as app does, whatever the activity says.
        assert_eq!(DpiScaling::SystemForced.mode(false), ScalingMode::App);
        assert_eq!(DpiScaling::SystemForced.mode(true), ScalingMode::App);
        for handles in [false, true] {
            assert_eq!(
                DpiScaling::SystemForced.mode_on("com.example.notes", handles, true),
                ScalingMode::App
            );
        }
        // Its densities go out forced, and nothing else's ever does.
        assert!(DpiScaling::SystemForced.forced_on("com.example.notes", true));
        for scaling in [DpiScaling::Auto, DpiScaling::App, DpiScaling::System] {
            assert!(!scaling.forced_on("com.example.notes", true), "{scaling:?}");
            assert!(
                !scaling.forced_on("com.google.android.youtube", true),
                "{scaling:?}"
            );
        }
    }

    #[test]
    fn fr4_forced_works_as_system_on_an_image_that_would_relaunch_the_app() {
        for handles in [false, true] {
            assert_eq!(
                DpiScaling::SystemForced.mode_on("com.example.notes", handles, false),
                ScalingMode::System
            );
        }
        assert!(!DpiScaling::SystemForced.forced_on("com.example.notes", false));
    }

    #[test]
    fn fr4_the_guest_changes_nothing_for_auto_app_and_system() {
        for package in ["com.example.notes", "com.google.android.youtube"] {
            for handles in [false, true] {
                for scaling in [DpiScaling::Auto, DpiScaling::App, DpiScaling::System] {
                    for available in [false, true] {
                        assert_eq!(
                            scaling.mode_on(package, handles, available),
                            scaling.mode_for(package, handles),
                            "{scaling:?} {package} {handles} {available}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn fr4_youtube_scales_the_system_way_by_default() {
        let youtube = "com.google.android.youtube";
        assert!(SYSTEM_SCALED_BY_DEFAULT.contains(&youtube));
        assert_eq!(DpiScaling::default_for(youtube), DpiScaling::System);
        assert_eq!(DpiScaling::Auto.resolved_for(youtube), DpiScaling::System);
        // Even though YouTube says it handles a change of density.
        assert_eq!(
            DpiScaling::Auto.mode_for(youtube, true),
            ScalingMode::System
        );
        // An app not on the list scales its own way: it takes each monitor's density if it
        // says it handles a change of density, and is scaled if not.
        assert_eq!(
            DpiScaling::default_for("com.example.notes"),
            DpiScaling::Auto
        );
        assert_eq!(
            DpiScaling::Auto.mode_for("com.example.notes", true),
            ScalingMode::App
        );
        assert_eq!(
            DpiScaling::Auto.mode_for("com.example.notes", false),
            ScalingMode::System
        );
        // Every entry is a package name, and none is there twice.
        for (i, package) in SYSTEM_SCALED_BY_DEFAULT.iter().enumerate() {
            assert!(crate::compositor::window_hosts::is_valid_package(package));
            assert!(!SYSTEM_SCALED_BY_DEFAULT[..i].contains(package));
        }
    }

    #[test]
    fn fr4_the_users_choice_wins_over_the_built_in_list() {
        let youtube = "com.google.android.youtube";
        assert_eq!(DpiScaling::App.resolved_for(youtube), DpiScaling::App);
        assert_eq!(DpiScaling::App.mode_for(youtube, true), ScalingMode::App);
        assert_eq!(
            DpiScaling::System.mode_for(youtube, true),
            ScalingMode::System
        );
        assert_eq!(
            DpiScaling::SystemForced.mode_for(youtube, true),
            DpiScaling::SystemForced.mode(true)
        );
        // The list changes only what auto means: a choice for another app stays as it is.
        for scaling in [
            DpiScaling::App,
            DpiScaling::System,
            DpiScaling::SystemForced,
        ] {
            assert_eq!(scaling.resolved_for("com.example.notes"), scaling);
        }
    }

    #[test]
    fn fr4_the_dpi_scaling_choice_is_stored_with_the_other_window_settings() {
        for scaling in DpiScaling::ALL {
            assert_eq!(DpiScaling::from_name(scaling.name()), Some(scaling));
            let app = AppWindowSettings {
                dpi_scaling: scaling,
                ..notes(false, None)
            };
            assert_eq!(AppWindowSettings::from_wire(&app.to_wire()), app);
            let stored = serde_json::to_string(&app).unwrap();
            assert_eq!(
                serde_json::from_str::<AppWindowSettings>(&stored).unwrap(),
                app
            );
            // Any choice but the default is an entry of its own.
            assert_eq!(app.is_default(), scaling == DpiScaling::Auto);
        }
        assert_eq!(DpiScaling::from_name("enhanced"), None);
        // Settings stored before the choice existed read as auto.
        let old: AppWindowSettings =
            serde_json::from_str(r#"{"package_name":"com.example.notes","fixed_size":true}"#)
                .unwrap();
        assert_eq!(old.dpi_scaling, DpiScaling::Auto);
        assert_eq!(
            serde_json::to_value(DpiScaling::SystemForced).unwrap(),
            "system-forced"
        );
    }

    #[test]
    fn fr30_each_kind_of_full_screen_request_follows_its_setting() {
        use FullScreenPlace::{Screen, Window};
        let builtin = FullScreenDefaults::default();
        // A clear request fills the screen, and bars hidden alone stays in the window.
        assert_eq!(
            full_screen_place(FullScreenRequest::Explicit, None, builtin),
            Some(Screen)
        );
        assert_eq!(
            full_screen_place(FullScreenRequest::BarsHidden, None, builtin),
            Some(Window)
        );
        assert_eq!(
            full_screen_place(FullScreenRequest::None, None, builtin),
            None
        );
        // Each part of the setting moves its own kind only.
        let swapped = FullScreenDefaults {
            fullscreen: Window,
            immersive: Screen,
        };
        assert_eq!(
            full_screen_place(FullScreenRequest::Explicit, None, swapped),
            Some(Window)
        );
        assert_eq!(
            full_screen_place(FullScreenRequest::BarsHidden, None, swapped),
            Some(Screen)
        );
        // A part left to the default is DSA's own default, never "default".
        let unset = FullScreenDefaults {
            fullscreen: FullScreenPlace::Default,
            immersive: FullScreenPlace::Default,
        };
        assert_eq!(unset.resolved(), builtin);
        assert_eq!(
            FullScreenDefaults::from_wire(Some(&unset.to_wire())),
            builtin
        );
        assert_eq!(FullScreenDefaults::from_wire(None), builtin);
        assert_eq!(
            FullScreenDefaults::from_wire(Some(&swapped.to_wire())),
            swapped
        );
    }

    #[test]
    fn fr30_an_apps_full_screen_choice_wins_over_the_default() {
        use FullScreenPlace::{Screen, Window};
        let video = AppWindowSettings {
            fullscreen: Window,
            immersive: Screen,
            ..notes(false, None)
        };
        let builtin = FullScreenDefaults::default();
        assert_eq!(
            full_screen_place(FullScreenRequest::Explicit, Some(&video), builtin),
            Some(Window)
        );
        assert_eq!(
            full_screen_place(FullScreenRequest::BarsHidden, Some(&video), builtin),
            Some(Screen)
        );
        // An app that follows the default for one part follows the default there.
        let half = AppWindowSettings {
            immersive: FullScreenPlace::Default,
            ..video.clone()
        };
        assert_eq!(
            full_screen_place(FullScreenRequest::BarsHidden, Some(&half), builtin),
            Some(Window)
        );
        // The choices are kept like the other window settings, and only the defaults need no
        // entry.
        for place in FullScreenPlace::ALL {
            assert_eq!(FullScreenPlace::from_name(place.name()), Some(place));
            let app = AppWindowSettings {
                fullscreen: place,
                immersive: place,
                ..notes(false, None)
            };
            assert_eq!(AppWindowSettings::from_wire(&app.to_wire()), app);
            let stored = serde_json::to_string(&app).unwrap();
            assert_eq!(
                serde_json::from_str::<AppWindowSettings>(&stored).unwrap(),
                app
            );
            assert_eq!(app.is_default(), place == FullScreenPlace::Default);
        }
        assert_eq!(FullScreenPlace::from_name("monitor"), None);
        // Settings stored before the choice existed follow the default.
        let old: AppWindowSettings =
            serde_json::from_str(r#"{"package_name":"com.example.notes","fixed_size":true}"#)
                .unwrap();
        assert_eq!(old.fullscreen, FullScreenPlace::Default);
        assert_eq!(old.immersive, FullScreenPlace::Default);
    }

    #[test]
    fn fr30_the_below_title_bar_switch_is_stored_with_the_other_window_settings() {
        let on = AppWindowSettings {
            below_title_bar: Some(true),
            ..notes(false, None)
        };
        // On is an entry of its own, and survives the wire and the stored file.
        assert!(!on.is_default());
        assert!(on.to_wire().below_title_bar);
        assert_eq!(AppWindowSettings::from_wire(&on.to_wire()), on);
        let stored = serde_json::to_string(&on).unwrap();
        assert_eq!(
            serde_json::from_str::<AppWindowSettings>(&stored).unwrap(),
            on
        );
        assert!(starts_below_title_bar("com.example.notes", Some(&on)));
        // Off is the default: no entry, nothing set on the wire, nothing in the file.
        let off = notes(false, None);
        assert!(off.is_default());
        assert!(!off.to_wire().below_title_bar);
        assert_eq!(AppWindowSettings::from_wire(&off.to_wire()), off);
        assert!(!serde_json::to_string(&off)
            .unwrap()
            .contains("below_title_bar"));
        assert!(!starts_below_title_bar("com.example.notes", Some(&off)));
        assert!(!starts_below_title_bar("com.example.notes", None));
        // Turned off again, the app's entry goes with it.
        let list = replace(&[], on.clone()).unwrap();
        assert_eq!(list, vec![on]);
        assert!(replace(&list, off).unwrap().is_empty());
        // Settings stored before the switch existed read as off.
        let old: AppWindowSettings =
            serde_json::from_str(r#"{"package_name":"com.example.notes","fixed_size":true}"#)
                .unwrap();
        assert_eq!(old.below_title_bar, None);
        assert!(!starts_below_title_bar("com.example.notes", Some(&old)));
    }

    #[test]
    fn fr22_the_apps_that_start_below_the_title_bar_are_the_ones_switched_on() {
        let app = |package: &str, below: Option<bool>| AppWindowSettings {
            package_name: package.into(),
            fixed_size: true,
            below_title_bar: below,
            ..AppWindowSettings::default()
        };
        assert_eq!(
            below_title_bar_packages(&[
                app("org.example.video", Some(true)),
                app("com.example.notes", None),
                app("com.example.mail", Some(false)),
                app("com.example.player", Some(true)),
            ]),
            vec![
                "com.example.player".to_string(),
                "org.example.video".to_string()
            ]
        );
        assert!(below_title_bar_packages(&[]).is_empty());
        // No app starts below the title bar on DSA's own choice for now; every entry such a
        // list would hold is a package name, there once.
        for (i, package) in BELOW_TITLE_BAR_BY_DEFAULT.iter().enumerate() {
            assert!(crate::compositor::window_hosts::is_valid_package(package));
            assert!(!BELOW_TITLE_BAR_BY_DEFAULT[..i].contains(package));
        }
    }

    #[test]
    fn fr30_an_apps_settings_replace_its_entry_and_the_defaults_remove_it() {
        let list = replace(&[], notes(true, None)).unwrap();
        assert_eq!(list, vec![notes(true, None)]);
        let list = replace(&list, notes(false, Some((400, 700)))).unwrap();
        assert_eq!(list, vec![notes(false, Some((400, 700)))]);
        assert!(replace(&list, notes(false, None)).unwrap().is_empty());
    }

    #[test]
    fn fr30_a_start_size_must_be_one_a_window_can_have() {
        assert!(notes(false, Some((400, 700))).checked().is_ok());
        assert!(notes(false, Some((100, 700))).checked().is_err());
        assert!(notes(false, Some((400, 9000))).checked().is_err());
        let bad = AppWindowSettings {
            package_name: "..\\evil".into(),
            ..notes(true, None)
        };
        assert!(bad.checked().is_err());
    }

    #[test]
    fn fr30_a_start_size_of_zero_on_the_wire_is_none() {
        let wire = notes(true, None).to_wire();
        assert_eq!((wire.start_width_dp, wire.start_height_dp), (0, 0));
        assert_eq!(AppWindowSettings::from_wire(&wire), notes(true, None));
        let wire = notes(false, Some((500, 800))).to_wire();
        assert_eq!(
            AppWindowSettings::from_wire(&wire),
            notes(false, Some((500, 800)))
        );
    }
}
