//! The Games page's list: the installed apps with whether each keeps its focus while its
//! window is not the active one ("keep game focus"), games first, and what its "Select all"
//! does. `dsa games` and, through it, every settings app read them from here.
//!
//! The setting itself is an app window setting ([`AppWindowSettings::keep_focus`]), stored and
//! sent with the others; what it does to the guest's focus is
//! [`crate::compositor::keep_focus`].

use crate::app_windows::AppWindowSettings;
use crate::tasks::AppInfo;

/// One app of the Games page's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameApp {
    pub package_name: String,
    /// The app's own name, as a launcher shows it.
    pub label: String,
    /// Whether the app says it is a game (Android's `CATEGORY_GAME`).
    pub game: bool,
    /// Whether the app keeps its focus while its window is not the active one.
    pub keep_focus: bool,
    /// The app's launcher icon as a PNG image; empty when the guest gave none.
    pub icon_png: Vec<u8>,
}

/// The installed apps (`apps`) with the user's window settings (`windows`): games first, then
/// every other app, each part by name as a person reads it, then by package. An app the user
/// set nothing for keeps no focus.
pub fn game_apps(apps: &[AppInfo], windows: &[AppWindowSettings]) -> Vec<GameApp> {
    let mut list: Vec<GameApp> = apps
        .iter()
        .map(|app| GameApp {
            package_name: app.package_name.clone(),
            label: app.label.clone(),
            game: app.game,
            keep_focus: windows
                .iter()
                .any(|window| window.package_name == app.package_name && window.keep_focus),
            icon_png: app.icon_png.clone(),
        })
        .collect();
    list.sort_by(|a, b| {
        b.game
            .cmp(&a.game)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
            .then_with(|| a.package_name.cmp(&b.package_name))
    });
    list.dedup_by(|a, b| a.package_name == b.package_name);
    list
}

/// What "Select all" turns every app of `list` to: on, unless every app is on already, in
/// which case off. So the first press turns all on and the next turns all off. An empty list
/// has nothing to turn, and reads as on.
pub fn select_all_turns_on(list: &[GameApp]) -> bool {
    list.is_empty() || !list.iter().all(|app| app.keep_focus)
}

/// The window settings of `package` with keep focus `on`, from its current ones in `windows`
/// (or the defaults when it has none), every other setting as it was.
pub fn with_keep_focus(
    windows: &[AppWindowSettings],
    package: &str,
    on: bool,
) -> AppWindowSettings {
    let current = windows
        .iter()
        .find(|window| window.package_name == package)
        .cloned()
        .unwrap_or_else(|| AppWindowSettings {
            package_name: package.to_string(),
            ..AppWindowSettings::default()
        });
    AppWindowSettings {
        keep_focus: on,
        ..current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(package: &str, label: &str, game: bool) -> AppInfo {
        AppInfo {
            package_name: package.into(),
            label: label.into(),
            game,
            ..AppInfo::default()
        }
    }

    fn installed() -> Vec<AppInfo> {
        vec![
            app("com.example.notes", "Notes", false),
            app("com.ustwo.monumentvalley", "Monument Valley", true),
            app("com.android.settings", "Settings", false),
            app("com.example.chess", "chess", true),
        ]
    }

    #[test]
    fn fr44_games_come_first_in_the_list() {
        let list = game_apps(&installed(), &[]);
        let packages: Vec<&str> = list.iter().map(|app| app.package_name.as_str()).collect();
        assert_eq!(
            packages,
            vec![
                "com.example.chess",
                "com.ustwo.monumentvalley",
                "com.example.notes",
                "com.android.settings",
            ]
        );
    }

    #[test]
    fn fr44_keep_game_focus_is_off_for_every_installed_app_by_default() {
        let list = game_apps(&installed(), &[]);
        assert_eq!(list.len(), 4);
        assert!(list.iter().all(|app| !app.keep_focus));
        // Other window settings do not turn it on.
        let resized = AppWindowSettings {
            package_name: "com.example.chess".into(),
            fixed_size: true,
            ..AppWindowSettings::default()
        };
        assert!(game_apps(&installed(), &[resized])
            .iter()
            .all(|app| !app.keep_focus));
    }

    #[test]
    fn fr44_select_all_turns_every_app_on_and_pressed_again_off() {
        let mut windows: Vec<AppWindowSettings> = Vec::new();
        let press = |windows: &mut Vec<AppWindowSettings>| {
            let list = game_apps(&installed(), windows);
            let on = select_all_turns_on(&list);
            for app in &list {
                let wanted = with_keep_focus(windows, &app.package_name, on);
                *windows = crate::app_windows::replace(windows, wanted).unwrap();
            }
            game_apps(&installed(), windows)
        };
        // Nothing on, then a few on: the press turns every app on.
        assert!(press(&mut windows).iter().all(|app| app.keep_focus));
        // Every app on: the next press turns every app off, and leaves no entry behind.
        assert!(press(&mut windows).iter().all(|app| !app.keep_focus));
        assert!(windows.is_empty());
        windows = vec![with_keep_focus(&[], "com.example.notes", true)];
        assert!(press(&mut windows).iter().all(|app| app.keep_focus));
    }

    #[test]
    fn fr44_turning_keep_focus_keeps_the_other_window_settings() {
        let resized = AppWindowSettings {
            package_name: "com.example.chess".into(),
            fixed_size: true,
            start_size_dp: Some((800, 600)),
            ..AppWindowSettings::default()
        };
        let on = with_keep_focus(std::slice::from_ref(&resized), "com.example.chess", true);
        assert_eq!(
            on,
            AppWindowSettings {
                keep_focus: true,
                ..resized.clone()
            }
        );
        let off = with_keep_focus(&[on], "com.example.chess", false);
        assert_eq!(off, resized);
        let fresh = with_keep_focus(&[], "com.example.notes", true);
        assert!(fresh.keep_focus && !fresh.fixed_size);
    }
}
