//! The host's rule for letting an Android app elevate to root, like sudo on the desktop.
//!
//! The whole feature is off unless the user turns it on, and turning it on is an
//! administrator's to do (it applies to every user of the computer and takes effect from the
//! next start of Android). With it off, this module decides nothing: the guest ships no su to
//! any app.
//!
//! With it on, each app has a level:
//! - `Off` (the default): su does not exist for the app.
//! - `Ask`: each su call waits for the host's administrator approval.
//! - `Remember`: su calls are approved without asking. Moving an app to `Remember` is itself
//!   an administrator's to do.
//!
//! An approval lasts a grace, matching Linux sudo's `timestamp_timeout` default of 15
//! minutes, kept per app as sudo keeps it per terminal. Within an app's grace a second su
//! call from that app is approved without asking, whatever its level. The grace is cleared
//! when the feature is turned off, when the app's level changes, and when Android restarts (a
//! fresh [`Policy`] is built for each boot).
//!
//! This module is the rule alone: who may see su (the kernel's allowlist), what a su request
//! decides to, and when to ask the host. It names no host OS API and no guest transport, so
//! it runs and is tested on every platform. The platform's approval dialog (UAC, the macOS
//! authorization dialog, polkit) and the guest relay live elsewhere and call in here.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::host_api::UserAndroid;
use crate::settings::SettingsStore;

/// How long an approval lasts, per app. Linux sudo's `timestamp_timeout` default.
pub const GRACE: Duration = Duration::from_secs(15 * 60);

/// How long su waits for the host to answer before it fails. The guest relay fails the call
/// with [`DecisionDenied::timed_out`] if no answer comes in this long.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

/// The package the host names `adb shell` by: Android's shell package, which owns uid 2000,
/// the uid `adb shell` runs as. While developer mode is on, `adb shell` asks for root through
/// the same approval as an app, under this package.
pub const ADB_SHELL_PACKAGE: &str = "com.android.shell";

/// The name the approval prompt, the log and the settings apps show for `adb shell`.
pub const ADB_SHELL_NAME: &str = "adb shell";

/// The other names `dsa apps root` takes for `adb shell`.
pub const ADB_SHELL_ALIASES: [&str; 3] = ["adb", "adb-shell", "shell"];

/// The package `name` stands for: [`ADB_SHELL_PACKAGE`] for one of [`ADB_SHELL_ALIASES`],
/// otherwise `name` itself.
pub fn package_for_name(name: &str) -> &str {
    if ADB_SHELL_ALIASES.contains(&name) {
        ADB_SHELL_PACKAGE
    } else {
        name
    }
}

/// Whether `package` is the adb shell item rather than an app.
pub fn is_adb_shell(package: &str) -> bool {
    package == ADB_SHELL_PACKAGE
}

/// A package's level when the user has not set one: ask for `adb shell`, which only a
/// developer who turned developer mode on can reach, and off for every app.
pub fn default_level(package: &str) -> Level {
    if is_adb_shell(package) {
        Level::Ask
    } else {
        Level::Off
    }
}

/// How far an app may elevate to root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    /// su does not exist for the app.
    #[default]
    Off,
    /// Each su call waits for the host's administrator approval.
    Ask,
    /// su calls are approved without asking, within the grace.
    Remember,
}

impl Level {
    /// The stable name used on the wire, in the CLI and in settings files.
    pub fn code(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Ask => "ask",
            Level::Remember => "remember",
        }
    }

    /// Reads a level from its stable name. Any other text is `None`.
    pub fn from_code(code: &str) -> Option<Level> {
        match code {
            "off" => Some(Level::Off),
            "ask" => Some(Level::Ask),
            "remember" => Some(Level::Remember),
            _ => None,
        }
    }

    /// Whether moving an app to this level needs the host's administrator approval. Only
    /// `Remember` does: it hands an app root without a prompt each time, so the grant itself
    /// is the administrator's to make.
    pub fn change_needs_admin(self) -> bool {
        matches!(self, Level::Remember)
    }

    /// Whether the kernel lets the app see su at this level. An `Off` app sees nothing; `Ask`
    /// and `Remember` apps do (so su can run and reach the relay), and the relay then holds
    /// each call until this module decides it.
    pub fn su_visible(self) -> bool {
        matches!(self, Level::Ask | Level::Remember)
    }
}

/// What a su request decides to, before any host prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run as uid 0 at once, without a prompt: a `Remember` app, or any app within its grace.
    ApproveWithoutPrompt,
    /// Ask the host's administrator. On approval the caller records the grace with
    /// [`Policy::approved`].
    PromptHost,
    /// Fail the su call: the app's level is `Off`, or the feature is off. su should not reach
    /// here (the kernel hides it from `Off` apps), so this is a backstop.
    Deny,
}

/// Why a su call failed, for su to print and the log to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionDenied {
    /// The host's administrator refused.
    Refused,
    /// No answer came within [`ANSWER_TIMEOUT`].
    TimedOut,
    /// The app may not ask (level `Off`), or the feature is off.
    NotAllowed,
}

impl DecisionDenied {
    /// A sentence su prints and the log keeps.
    pub fn describe(self) -> &'static str {
        match self {
            DecisionDenied::Refused => "the computer's administrator refused the root request",
            DecisionDenied::TimedOut => {
                "the root request timed out waiting for the computer's administrator"
            }
            DecisionDenied::NotAllowed => "this app may not use root",
        }
    }
}

/// The host's root elevation state for one user's running Android. Built fresh for each boot,
/// so a restart clears every grace.
#[derive(Debug)]
pub struct Policy {
    enabled: bool,
    levels: HashMap<String, Level>,
    /// When each app's grace started. A missing or elapsed entry means no grace.
    grace: HashMap<String, Instant>,
}

/// One app's level, for building the kernel allowlist and the settings listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLevel {
    pub package: String,
    pub level: Level,
}

impl Policy {
    /// A policy with the feature off and no app levels.
    pub fn new() -> Policy {
        Policy {
            enabled: false,
            levels: HashMap::new(),
            grace: HashMap::new(),
        }
    }

    /// A policy with the feature in a known state and the given per-app levels, for a boot.
    pub fn with_levels(enabled: bool, levels: impl IntoIterator<Item = AppLevel>) -> Policy {
        let mut policy = Policy::new();
        policy.enabled = enabled;
        for app in levels {
            if app.level != Level::Off {
                policy.levels.insert(app.package, app.level);
            }
        }
        policy
    }

    /// Whether the feature is on.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turns the feature on or off. Turning it off clears every grace at once, so no app
    /// keeps a standing approval; the kernel allowlist empties from the feature being off.
    /// The change takes full effect from the next start of Android (the kernel hook and the
    /// relay are set up at boot); within a running Android it only stops new approvals.
    pub fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.grace.clear();
        }
        self.enabled = enabled;
    }

    /// The app's level, `Off` unless set.
    pub fn level(&self, package: &str) -> Level {
        self.levels.get(package).copied().unwrap_or(Level::Off)
    }

    /// Sets an app's level and clears that app's grace. `Off` forgets the app.
    pub fn set_level(&mut self, package: &str, level: Level) {
        self.grace.remove(package);
        if level == Level::Off {
            self.levels.remove(package);
        } else {
            self.levels.insert(package.to_string(), level);
        }
    }

    /// Every app with a level other than `Off`, in package order, for the settings listing.
    pub fn app_levels(&self) -> Vec<AppLevel> {
        let mut apps: Vec<AppLevel> = self
            .levels
            .iter()
            .map(|(package, level)| AppLevel {
                package: package.clone(),
                level: *level,
            })
            .collect();
        apps.sort_by(|a, b| a.package.cmp(&b.package));
        apps
    }

    /// The packages the kernel lets see su: the `Ask` and `Remember` apps, but only while the
    /// feature is on. Empty with the feature off, so su exists for no app.
    pub fn su_visible_packages(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        let mut packages: Vec<String> = self
            .levels
            .iter()
            .filter(|(_, level)| level.su_visible())
            .map(|(package, _)| package.clone())
            .collect();
        packages.sort();
        packages
    }

    /// Whether the app is within its grace at `now`.
    pub fn in_grace(&self, package: &str, now: Instant) -> bool {
        match self.grace.get(package) {
            Some(started) => now.duration_since(*started) < GRACE,
            None => false,
        }
    }

    /// What a su call from the app decides to at `now`, before any prompt.
    pub fn decide(&self, package: &str, now: Instant) -> Decision {
        if !self.enabled {
            return Decision::Deny;
        }
        match self.level(package) {
            Level::Off => Decision::Deny,
            Level::Remember => Decision::ApproveWithoutPrompt,
            Level::Ask => {
                if self.in_grace(package, now) {
                    Decision::ApproveWithoutPrompt
                } else {
                    Decision::PromptHost
                }
            }
        }
    }

    /// Records that the host approved the app at `now`, starting (or renewing) its grace. The
    /// caller calls this after a prompt the administrator approved, and also after a
    /// `Remember` approval so a later level change or feature-off still clears the standing
    /// state.
    pub fn approved(&mut self, package: &str, now: Instant) {
        if self.enabled && self.level(package) != Level::Off {
            self.grace.insert(package.to_string(), now);
        }
    }
}

impl Default for Policy {
    fn default() -> Policy {
        Policy::new()
    }
}

/// The computer's switch and `user`'s app levels as `store` keeps them, an app set to off
/// included: off is what keeps a whole shared user ID group from root, which only the guest
/// can group by uid. A level the store cannot read is left out (off), and a store that cannot
/// list the apps gives none. While
/// `user`'s developer mode is on, the adb shell item is among them at its stored level (ask
/// unless set); with developer mode off adb is closed, so the item is left out.
pub fn stored(store: &dyn SettingsStore, user: &str) -> (bool, Vec<AppLevel>) {
    let on = store.machine().root_elevation;
    let saved = match store.root_apps(user) {
        Ok(apps) => Some(apps),
        Err(why) => {
            eprintln!("root elevation: cannot read the app levels of {user}: {why}");
            None
        }
    };
    let developer_mode = store
        .get(user)
        .map(|settings| settings.developer_mode)
        .unwrap_or(false);
    let mut apps: Vec<AppLevel> = saved
        .iter()
        .flatten()
        .filter(|app| !is_adb_shell(&app.package))
        .filter_map(|app| {
            Some(AppLevel {
                level: Level::from_code(&app.level)?,
                package: app.package.clone(),
            })
        })
        .collect();
    if let Some(saved) = saved.as_ref().filter(|_| developer_mode) {
        let level = adb_shell_level(saved);
        if level != Level::Off {
            apps.push(AppLevel {
                package: ADB_SHELL_PACKAGE.to_string(),
                level,
            });
        }
    }
    (on, apps)
}

/// The adb shell item's level among `saved`: its stored level, or ask when none is stored.
pub fn adb_shell_level(saved: &[crate::settings::RootApp]) -> Level {
    saved
        .iter()
        .find(|app| is_adb_shell(&app.package))
        .and_then(|app| Level::from_code(&app.level))
        .unwrap_or(default_level(ADB_SHELL_PACKAGE))
}

/// The packages that share `package`'s Android user ID among `installed` (the guest's list
/// of installed apps), `package` first and the others in package order. Just `package` for an
/// app with a user ID of its own, or one the list does not name.
pub fn shared_user_group(installed: &[crate::tasks::AppInfo], package: &str) -> Vec<String> {
    let shared_user_id = installed
        .iter()
        .find(|app| app.package_name == package)
        .map(|app| app.shared_user_id.as_str())
        .unwrap_or_default();
    let mut group = vec![package.to_string()];
    if shared_user_id.is_empty() {
        return group;
    }
    let mut others: Vec<String> = installed
        .iter()
        .filter(|app| app.shared_user_id == shared_user_id && app.package_name != package)
        .map(|app| app.package_name.clone())
        .collect();
    others.sort();
    others.dedup();
    group.extend(others);
    group
}

/// The level of a group of apps that share an Android user ID, from the levels in `saved`:
/// off when any app of the group was set to off (the kernel tells the apps apart only by
/// their uid, so one app kept from root keeps the whole group from it), otherwise the
/// highest level set for any of them, and the first app's default when none was set. For a
/// group of one, the app's own level.
pub fn group_level(saved: &[crate::settings::RootApp], group: &[String]) -> Level {
    let levels: Vec<Level> = group
        .iter()
        .filter_map(|member| {
            saved
                .iter()
                .find(|app| app.package == *member)
                .and_then(|app| Level::from_code(&app.level))
        })
        .collect();
    if levels.contains(&Level::Off) {
        Level::Off
    } else if levels.contains(&Level::Remember) {
        Level::Remember
    } else if levels.contains(&Level::Ask) {
        Level::Ask
    } else {
        group
            .first()
            .map(|package| default_level(package))
            .unwrap_or_default()
    }
}

/// Gives `user`'s Android their root elevation settings as `store` keeps them: when the
/// Android is made, and after every change to the switch or to an app's level.
pub fn apply(android: &dyn UserAndroid, store: &dyn SettingsStore, user: &str) {
    let (on, apps) = stored(store, user);
    android.set_root_elevation_config(on, apps);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn fr35_level_codes_round_trip() {
        for level in [Level::Off, Level::Ask, Level::Remember] {
            assert_eq!(Level::from_code(level.code()), Some(level));
        }
        assert_eq!(Level::from_code("nonsense"), None);
    }

    #[test]
    fn fr35_default_level_is_off_and_hides_su() {
        let policy = Policy::new();
        assert_eq!(policy.level("com.example.app"), Level::Off);
        assert!(!Level::Off.su_visible());
        assert!(Level::Ask.su_visible());
        assert!(Level::Remember.su_visible());
    }

    #[test]
    fn fr35_an_app_not_allowed_is_denied() {
        let mut policy = Policy::new();
        policy.set_enabled(true);
        let now = Instant::now();
        assert_eq!(policy.decide("com.example.app", now), Decision::Deny);
        assert!(policy.su_visible_packages().is_empty());
    }

    #[test]
    fn fr35_feature_off_hides_su_and_denies_even_a_remember_app() {
        let mut policy = Policy::new();
        policy.set_level("com.backup.app", Level::Remember);
        // Feature still off.
        let now = Instant::now();
        assert_eq!(policy.decide("com.backup.app", now), Decision::Deny);
        assert!(policy.su_visible_packages().is_empty());
    }

    #[test]
    fn fr35_ask_app_prompts_then_grace_approves_without_prompt() {
        let mut policy = Policy::new();
        policy.set_enabled(true);
        policy.set_level("com.ask.app", Level::Ask);
        let base = Instant::now();

        // First call asks.
        assert_eq!(policy.decide("com.ask.app", base), Decision::PromptHost);
        policy.approved("com.ask.app", base);

        // A second call within 15 minutes does not ask.
        assert_eq!(
            policy.decide("com.ask.app", at(base, 10 * 60)),
            Decision::ApproveWithoutPrompt
        );

        // Past 15 minutes it asks again.
        assert_eq!(
            policy.decide("com.ask.app", at(base, 15 * 60 + 1)),
            Decision::PromptHost
        );
    }

    #[test]
    fn fr35_remember_app_never_prompts() {
        let mut policy = Policy::new();
        policy.set_enabled(true);
        policy.set_level("com.remember.app", Level::Remember);
        let now = Instant::now();
        assert_eq!(
            policy.decide("com.remember.app", now),
            Decision::ApproveWithoutPrompt
        );
    }

    #[test]
    fn fr35_grace_is_per_app() {
        let mut policy = Policy::new();
        policy.set_enabled(true);
        policy.set_level("com.one.app", Level::Ask);
        policy.set_level("com.two.app", Level::Ask);
        let base = Instant::now();

        policy.approved("com.one.app", base);
        // Only the approved app skips the prompt.
        assert_eq!(
            policy.decide("com.one.app", at(base, 60)),
            Decision::ApproveWithoutPrompt
        );
        assert_eq!(
            policy.decide("com.two.app", at(base, 60)),
            Decision::PromptHost
        );
    }

    #[test]
    fn fr35_changing_level_clears_grace() {
        let mut policy = Policy::new();
        policy.set_enabled(true);
        policy.set_level("com.ask.app", Level::Ask);
        let base = Instant::now();
        policy.approved("com.ask.app", base);
        assert!(policy.in_grace("com.ask.app", at(base, 60)));

        // Setting the same (or any) level again clears the grace.
        policy.set_level("com.ask.app", Level::Ask);
        assert!(!policy.in_grace("com.ask.app", at(base, 61)));
        assert_eq!(
            policy.decide("com.ask.app", at(base, 61)),
            Decision::PromptHost
        );
    }

    #[test]
    fn fr35_turning_the_feature_off_clears_every_grace() {
        let mut policy = Policy::new();
        policy.set_enabled(true);
        policy.set_level("com.ask.app", Level::Ask);
        let base = Instant::now();
        policy.approved("com.ask.app", base);

        policy.set_enabled(false);
        // The grace is gone and su is hidden from every app.
        assert!(!policy.in_grace("com.ask.app", at(base, 1)));
        assert!(policy.su_visible_packages().is_empty());

        // Turning it back on does not bring the grace back.
        policy.set_enabled(true);
        assert_eq!(
            policy.decide("com.ask.app", at(base, 2)),
            Decision::PromptHost
        );
    }

    #[test]
    fn fr35_a_restart_clears_grace_because_the_policy_is_rebuilt() {
        // A boot builds the policy from the stored levels, carrying no grace.
        let levels = vec![
            AppLevel {
                package: "com.ask.app".into(),
                level: Level::Ask,
            },
            AppLevel {
                package: "com.off.app".into(),
                level: Level::Off,
            },
            AppLevel {
                package: "com.remember.app".into(),
                level: Level::Remember,
            },
        ];
        let policy = Policy::with_levels(true, levels);
        let now = Instant::now();
        // The ask app has no standing grace after the restart.
        assert_eq!(policy.decide("com.ask.app", now), Decision::PromptHost);
        // Off apps are dropped; only ask and remember are visible, in order.
        assert_eq!(
            policy.su_visible_packages(),
            vec!["com.ask.app".to_string(), "com.remember.app".to_string()]
        );
    }

    #[test]
    fn fr35_only_remember_needs_admin_to_set() {
        assert!(!Level::Off.change_needs_admin());
        assert!(!Level::Ask.change_needs_admin());
        assert!(Level::Remember.change_needs_admin());
    }

    #[test]
    fn fr35_adb_shell_asks_for_root_like_an_app() {
        use crate::settings::{MachineSettings, MemorySettings};
        let store = MemorySettings::default();
        store
            .set_machine(MachineSettings {
                root_elevation: true,
                ..store.machine()
            })
            .unwrap();
        // Developer mode off: adb is closed, so the item is not given to Android.
        let (on, apps) = stored(&store, "u");
        assert!(on);
        assert!(apps.iter().all(|app| app.package != ADB_SHELL_PACKAGE));
        // Developer mode on: the item asks by default.
        let mut settings = store.get("u").unwrap();
        settings.developer_mode = true;
        store.set("u", &settings).unwrap();
        let (_, apps) = stored(&store, "u");
        assert!(apps.contains(&AppLevel {
            package: ADB_SHELL_PACKAGE.into(),
            level: Level::Ask,
        }));
        // Set to off it stays off, and Android no longer lets adb shell see su.
        store.set_root_app("u", ADB_SHELL_PACKAGE, "off").unwrap();
        let (_, apps) = stored(&store, "u");
        assert!(apps.iter().all(|app| app.package != ADB_SHELL_PACKAGE));
        // Back to the default, ask, the stored entry is forgotten.
        store.set_root_app("u", ADB_SHELL_PACKAGE, "ask").unwrap();
        assert!(store.root_apps("u").unwrap().is_empty());
        assert_eq!(package_for_name("adb"), ADB_SHELL_PACKAGE);
        assert_eq!(package_for_name("com.example"), "com.example");
        assert_eq!(default_level("com.example"), Level::Off);
    }

    #[test]
    fn fr35_apps_sharing_a_user_id_are_approved_as_one_group() {
        let app = |package: &str, shared: &str| crate::tasks::AppInfo {
            package_name: package.into(),
            shared_user_id: shared.into(),
            ..Default::default()
        };
        let installed = [
            app("com.shared.two", "com.shared"),
            app("com.alone", ""),
            app("com.shared.one", "com.shared"),
        ];
        assert_eq!(
            shared_user_group(&installed, "com.shared.two"),
            vec!["com.shared.two".to_string(), "com.shared.one".to_string()]
        );
        assert_eq!(
            shared_user_group(&installed, "com.alone"),
            vec!["com.alone".to_string()]
        );
        assert_eq!(
            shared_user_group(&installed, "com.unknown"),
            vec!["com.unknown".to_string()]
        );

        let saved = |levels: &[(&str, &str)]| {
            levels
                .iter()
                .map(|(package, level)| crate::settings::RootApp {
                    package: (*package).into(),
                    level: (*level).into(),
                })
                .collect::<Vec<_>>()
        };
        let group = vec!["com.shared.one".to_string(), "com.shared.two".to_string()];
        // One app set to ask: the group asks.
        assert_eq!(
            group_level(&saved(&[("com.shared.one", "ask")]), &group),
            Level::Ask
        );
        // The highest level set wins, unless any app of the group is off.
        assert_eq!(
            group_level(
                &saved(&[("com.shared.one", "ask"), ("com.shared.two", "remember")]),
                &group
            ),
            Level::Remember
        );
        assert_eq!(
            group_level(
                &saved(&[("com.shared.one", "remember"), ("com.shared.two", "off")]),
                &group
            ),
            Level::Off
        );
        assert_eq!(group_level(&[], &group), Level::Off);

        // An app set to off is handed on, so the guest can keep its whole group from root.
        let store = crate::settings::MemorySettings::default();
        store.set_root_app("u", "com.shared.two", "off").unwrap();
        let (_, apps) = stored(&store, "u");
        assert_eq!(
            apps,
            vec![AppLevel {
                package: "com.shared.two".into(),
                level: Level::Off,
            }]
        );
    }

    #[test]
    fn fr35_denied_reasons_have_words() {
        for denied in [
            DecisionDenied::Refused,
            DecisionDenied::TimedOut,
            DecisionDenied::NotAllowed,
        ] {
            assert!(!denied.describe().is_empty());
        }
    }
}
