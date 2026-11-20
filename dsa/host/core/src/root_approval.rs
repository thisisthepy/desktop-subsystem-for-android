//! Answering an Android app's su call on the host, and asking the host's administrator.
//!
//! The guest's su relay sends a [`control::SuRequest`] for each su call and waits up to
//! [`ANSWER_TIMEOUT`](crate::root_elevation::ANSWER_TIMEOUT) for the [`control::SuDecision`].
//! [`RootApproval`] makes that decision for one user's Android: it asks the [`Policy`] first,
//! and only when the policy says the host's
//! administrator has to approve does it show the platform's administrator prompt
//! ([`ApprovalPrompt`]: UAC on Windows, Authorization Services on macOS, polkit on Linux). An
//! approval starts the app's grace, so the app's next su call within it is not asked again.
//!
//! The prompt has [`PROMPT_TIMEOUT`] to answer, or [`SCREEN_READER_PROMPT_TIMEOUT`] while a
//! screen reader runs ([`prompt_timeout`]): hearing the prompt's words and buttons read out and
//! choosing one takes longer than reading them. Both are shorter than su's own limit, so the
//! host's "timed out" reaches su. Whether a screen reader runs is asked of the platform
//! ([`ScreenReader`]) each time a prompt is about to open, not once at start, as one may be
//! started or stopped at any time. The prompt runs on a thread of its own: past its time the
//! request has timed out, the prompt is withdrawn where the platform can withdraw it, and its
//! late answer is ignored. A prompt that panics is a refusal, not a crash. Prompts go one at a time, so a
//! second call from an app waits for the first to be answered and is then approved within the
//! grace that answer started.
//!
//! The same prompt approves moving an app to the level that keeps root without asking
//! ([`RootApproval::approve_keep_root`]), so a user who is not an administrator can still
//! have an administrator approve it in their own session.
//!
//! Every decision goes to the host log with the app, the time, the command and the outcome,
//! so the computer's owner can read back what ran as root.
//!
//! The policy is rebuilt from the settings at each start of Android ([`RootApproval::restart`]),
//! which clears every grace. Between starts the per-app levels are read again at each request,
//! so a level changed in the settings applies to the next su call and clears that app's grace;
//! turning the whole feature off stops every approval at once, while turning it on applies only
//! from the next start, as the guest's su is set up at boot.
//!
//! The prompt is the only part that names a host OS API, so everything here runs and is
//! tested on every platform with a fake prompt.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use dsa_protocol::control;

use crate::resize_trace::utc_stamp;
use crate::root_elevation::{AppLevel, Decision, DecisionDenied, Level, Policy};
use crate::root_requester::{
    app_id, developer_shell_name, system_name, GenericIcon, Requester, FIRST_APP_ID, SHELL_APP_ID,
};
use crate::settings::SettingsStore;
use crate::tasks::AppInfo;
use crate::ui_language::UiLanguage;
use crate::ui_text::fill;

/// The file in a [`FolderApprover`]'s folder where each asked request is appended as one JSON
/// line.
pub const REQUESTS_FILE: &str = "requests.jsonl";

/// The file in a [`FolderApprover`]'s folder that holds the administrator's next answer (`allow`,
/// `deny`, `cancel` or `timeout`).
pub const ANSWER_FILE: &str = "answer";

const FOLDER_POLL: Duration = Duration::from_millis(25);

/// How long the host's administrator prompt may stay open. Shorter than the guest's
/// [`ANSWER_TIMEOUT`](crate::root_elevation::ANSWER_TIMEOUT), so the host's "timed out"
/// reaches su (and the log) before su gives up on its own.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(55);

/// How long the prompt may stay open while a screen reader runs. A person who hears the prompt
/// rather than sees it listens to the app's name, the command and each button in turn before
/// choosing, and may go back over them, which takes far longer than a glance. Still shorter
/// than the guest's [`ANSWER_TIMEOUT`](crate::root_elevation::ANSWER_TIMEOUT), so su hears the
/// host's "timed out" rather than giving up first.
pub const SCREEN_READER_PROMPT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long the prompt may stay open, by whether a screen reader runs as it opens.
pub fn prompt_timeout(screen_reader: bool) -> Duration {
    if screen_reader {
        SCREEN_READER_PROMPT_TIMEOUT
    } else {
        PROMPT_TIMEOUT
    }
}

/// Says whether a screen reader runs in the session the prompt opens in. The platform answers
/// it (each host OS keeps the flag its own way); a platform that cannot tell says no, which
/// leaves the prompt its default time.
pub type ScreenReader = Arc<dyn Fn() -> bool + Send + Sync>;

/// How often a request that waits for another prompt to close looks again.
const TURN_POLL: Duration = Duration::from_millis(50);

/// What the administrator is asked to approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Purpose {
    /// One su call: the command, or a shell.
    #[default]
    Run,
    /// Letting the app run anything as root from now on without asking each time.
    KeepRoot,
}

/// What the administrator prompt is asked about, with what the prompt shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApprovalRequest {
    /// The guest's ID of the su call, unique while its daemon runs; 0 for a level change.
    pub request_id: u64,
    /// The calling app, with its name and icon as the guest last listed them. An app the guest
    /// has not listed has its package as its name and no icon.
    pub app: AppInfo,
    /// The app's Android uid; 0 when unknown (a level change).
    pub uid: u32,
    /// The command su runs, split as a shell would; empty for an interactive shell.
    pub command: Vec<String>,
    /// Whether su has a terminal (an interactive shell).
    pub interactive: bool,
    pub purpose: Purpose,
    /// When the calling app shares its Android user ID (`android:sharedUserId`) with other apps,
    /// every app in the group, so the prompt shows all of them and says they share a user ID.
    pub shared_apps: Vec<AppInfo>,
    /// The user's first display language as a BCP 47 tag, which the prompt's words are in;
    /// empty when the host has not said, which is English.
    pub language: String,
}

/// The icon the prompt shows for who asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptIcon<'a> {
    /// The app's launcher icon, a PNG as the guest drew it.
    App(&'a [u8]),
    /// A generic icon, for a requester that is not an app.
    Generic(GenericIcon),
    /// None the guest drew: the platform shows DSA's own.
    None,
}

impl ApprovalRequest {
    /// The request a su call makes. `app` is the app as the guest last listed it.
    pub fn from_su(request: &control::SuRequest, app: Option<&AppInfo>) -> ApprovalRequest {
        let packages = package_group(&request.package);
        let shared_apps = if packages.len() > 1 {
            packages
                .iter()
                .map(|pkg| match app {
                    Some(info) if info.package_name == *pkg => info.clone(),
                    _ => AppInfo {
                        package_name: (*pkg).to_string(),
                        ..Default::default()
                    },
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut main_app = app.cloned().unwrap_or_else(|| AppInfo {
            package_name: request.package.clone(),
            ..Default::default()
        });
        if packages.len() > 1 {
            main_app.package_name = request.package.clone();
        }
        ApprovalRequest {
            request_id: request.request_id,
            app: main_app,
            uid: request.uid,
            command: request.command.clone(),
            interactive: request.interactive,
            purpose: Purpose::Run,
            shared_apps,
            language: String::new(),
        }
    }

    /// Has the prompt speak the language `tag` (a BCP 47 tag), the user's first display
    /// language. An empty tag leaves it as it is.
    pub fn with_language(mut self, tag: &str) -> ApprovalRequest {
        if !tag.trim().is_empty() {
            self.language = tag.trim().to_string();
        }
        self
    }

    /// The language the prompt's words are in: the interface language for the user's first
    /// display language, English when DSA has no translation for it.
    pub fn language(&self) -> UiLanguage {
        UiLanguage::from_tag_or_english(&self.language)
    }

    /// Sets the apps in a shared user ID group, with their names and icons as the guest listed
    /// them.
    pub fn with_shared_apps(mut self, shared_apps: Vec<AppInfo>) -> ApprovalRequest {
        if shared_apps.len() > 1 {
            self.app.package_name = shared_apps
                .iter()
                .map(|app| app.package_name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if self.app.label.is_empty() {
                if let Some(first) = shared_apps.first() {
                    self.app.label = first.label.clone();
                }
            }
            if self.app.icon_png.is_empty() {
                if let Some(first) = shared_apps.iter().find(|app| !app.icon_png.is_empty()) {
                    self.app.icon_png = first.icon_png.clone();
                }
            }
        }
        self.shared_apps = shared_apps;
        self
    }

    /// The request to let `app` keep root without asking each time.
    pub fn keep_root(app: AppInfo) -> ApprovalRequest {
        ApprovalRequest {
            app,
            purpose: Purpose::KeepRoot,
            ..Default::default()
        }
    }

    /// The requesting app's package.
    pub fn package(&self) -> &str {
        &self.app.package_name
    }

    /// The app's launcher icon as a PNG image; empty when the guest has not drawn one.
    pub fn icon_png(&self) -> &[u8] {
        &self.app.icon_png
    }

    /// Who asks: `adb shell` (Android's shell uid, or the shell package), one of Android's
    /// own services (a uid below the first app's), the apps of a shared user ID, or one app.
    pub fn requester(&self) -> Requester {
        let known_uid = self.purpose == Purpose::Run && self.uid != 0;
        let id = app_id(self.uid);
        if crate::root_elevation::is_adb_shell(self.package()) || (known_uid && id == SHELL_APP_ID)
        {
            Requester::DeveloperShell
        } else if known_uid && id < FIRST_APP_ID {
            Requester::System
        } else if self.shared_apps.len() > 1 {
            Requester::SharedApps
        } else {
            Requester::App
        }
    }

    /// The name the log and the prompt's English give who asks: see [`Self::name_in`].
    pub fn app_name(&self) -> String {
        self.name_in(UiLanguage::English)
    }

    /// Who asks, by name in `language`: an app by its label (its package when the guest gave
    /// none), the apps of a shared user ID by theirs, `adb shell` as the developer shell, and
    /// Android's own services in words.
    pub fn name_in(&self, language: UiLanguage) -> String {
        match self.requester() {
            Requester::DeveloperShell => developer_shell_name(language).to_string(),
            Requester::System => system_name(self.uid, language).to_string(),
            Requester::SharedApps => self
                .shared_apps
                .iter()
                .map(label_or_package)
                .collect::<Vec<_>>()
                .join(", "),
            Requester::App => label_or_package(&self.app).to_string(),
        }
    }

    /// The smaller second line under the name: the package (packages, for a shared user ID or
    /// an Android service), with the uid for a requester that is not an app. Empty for an app
    /// the guest gave no label, whose name is its package already.
    pub fn detail_in(&self, language: UiLanguage) -> String {
        const SHOWN: usize = 3;
        let words = prompt_words(language);
        let packages = package_group(self.package());
        let listed = |packages: &[&str]| {
            let mut text = packages
                .iter()
                .take(SHOWN)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            if packages.len() > SHOWN {
                let more = (packages.len() - SHOWN).to_string();
                text.push_str(&fill(words.and_more, &[("more", &more)]));
            }
            text
        };
        match self.requester() {
            Requester::DeveloperShell => {
                let uid = if self.uid == 0 {
                    SHELL_APP_ID
                } else {
                    self.uid
                };
                format!("{} (uid {uid})", crate::root_elevation::ADB_SHELL_PACKAGE)
            }
            Requester::System if packages.is_empty() => format!("uid {}", self.uid),
            Requester::System => format!("{} (uid {})", listed(&packages), self.uid),
            Requester::SharedApps => {
                let shared: Vec<&str> = self
                    .shared_apps
                    .iter()
                    .map(|app| app.package_name.as_str())
                    .collect();
                format!("{}{}", listed(&shared), words.shared_user_id)
            }
            Requester::App => {
                if self.app.label.trim().is_empty() {
                    String::new()
                } else {
                    self.package().to_string()
                }
            }
        }
    }

    /// The icon the prompt shows: the app's launcher icon; for a shared user ID, the first
    /// app's that has one; a generic icon for the developer shell and Android's services.
    pub fn prompt_icon(&self) -> PromptIcon<'_> {
        let requester = self.requester();
        if let Some(icon) = requester.generic_icon() {
            return PromptIcon::Generic(icon);
        }
        let drawn = if requester == Requester::SharedApps {
            self.shared_apps
                .iter()
                .chain(std::iter::once(&self.app))
                .map(|app| app.icon_png.as_slice())
                .find(|png| !png.is_empty())
        } else {
            Some(self.app.icon_png.as_slice()).filter(|png| !png.is_empty())
        };
        drawn.map_or(PromptIcon::None, PromptIcon::App)
    }

    /// What is asked for, in words for a prompt that shows it on a line of its own: the
    /// command as a shell would read it, "an interactive shell", or keeping root.
    pub fn command_text(&self) -> String {
        self.command_text_in(UiLanguage::English)
    }

    /// [`Self::command_text`] in `language`.
    pub fn command_text_in(&self, language: UiLanguage) -> String {
        let words = prompt_words(language);
        match self.purpose {
            Purpose::Run if !self.command.is_empty() => {
                command_text(&self.command, self.interactive)
            }
            Purpose::Run if self.interactive => words.interactive_shell.to_string(),
            Purpose::Run => words.shell.to_string(),
            Purpose::KeepRoot => words.any_command.to_string(),
        }
    }

    /// The command as one line, each word quoted when it needs to be. Empty for a shell.
    pub fn command_line(&self) -> String {
        self.command
            .iter()
            .map(|word| quote_word(word))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// What the app wants, in words for a sentence and the log: the command in backticks, or
    /// a shell.
    pub fn what(&self) -> String {
        match self.purpose {
            Purpose::KeepRoot => "root without asking each time".to_string(),
            Purpose::Run if self.command.is_empty() => command_text(&[], self.interactive),
            Purpose::Run => format!("`{}`", self.command_line()),
        }
    }

    /// The sentence the administrator prompt shows, in English: see [`Self::message_in`].
    pub fn message(&self) -> String {
        self.message_in(UiLanguage::English)
    }

    /// The sentence the administrator prompt shows in `language`: who asks, by name with the
    /// package beside it, and what they want. When several apps share the user ID, it lists
    /// all of them and says they share a user ID.
    pub fn message_in(&self, language: UiLanguage) -> String {
        let words = prompt_words(language);
        // Only a command is named in the sentence; a shell and keeping root have sentences of
        // their own.
        let what = format!("`{}`", self.command_line());
        let named = |app: &AppInfo| {
            let label = app.label.trim();
            if label.is_empty() || label == app.package_name {
                app.package_name.clone()
            } else {
                fill(
                    words.labelled,
                    &[("label", label), ("package", &app.package_name)],
                )
            }
        };
        let requester = self.requester();
        if requester == Requester::SharedApps {
            let names = self
                .shared_apps
                .iter()
                .map(named)
                .collect::<Vec<_>>()
                .join(", ");
            let sentence = match self.purpose {
                Purpose::KeepRoot => words.shared_keep_root,
                Purpose::Run if self.command.is_empty() => words.shared_shell,
                Purpose::Run => words.shared_run,
            };
            return fill(sentence, &[("names", &names), ("what", &what)]);
        }
        let who = match requester {
            Requester::DeveloperShell => self.name_in(language),
            Requester::System => fill(
                words.service_uid,
                &[
                    ("name", &self.name_in(language)),
                    ("uid", &self.uid.to_string()),
                ],
            ),
            _ => fill(words.android_app, &[("app", &named(&self.app))]),
        };
        let sentence = match self.purpose {
            Purpose::KeepRoot => words.keep_root,
            Purpose::Run if self.command.is_empty() => words.open_shell,
            Purpose::Run => words.run,
        };
        fill(sentence, &[("who", &who), ("what", &what)])
    }
}

/// The words of the administrator prompt in one language. `{who}` is who asks, `{names}` the
/// apps of a shared user ID, `{what}` the command in backticks.
struct PromptWords {
    /// After the first packages of a long list: `{more}` is how many more there are.
    and_more: &'static str,
    /// After the packages of a shared user ID.
    shared_user_id: &'static str,
    interactive_shell: &'static str,
    shell: &'static str,
    /// What keeping root asks for, on a line of its own.
    any_command: &'static str,
    /// An app with a label of its own: `{label}` and `{package}`.
    labelled: &'static str,
    /// One of Android's own services: `{name}` and `{uid}`.
    service_uid: &'static str,
    /// An app that asks: `{app}`.
    android_app: &'static str,
    keep_root: &'static str,
    open_shell: &'static str,
    run: &'static str,
    shared_keep_root: &'static str,
    shared_shell: &'static str,
    shared_run: &'static str,
}

fn prompt_words(language: UiLanguage) -> PromptWords {
    match language {
        UiLanguage::English => PromptWords {
            and_more: " and {more} more",
            shared_user_id: ": one shared user ID",
            interactive_shell: "an interactive shell",
            shell: "a shell",
            any_command: "any command, from now on without asking each time",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "The Android app {app}",
            keep_root: "{who} wants to run commands as root without asking each time.",
            open_shell: "{who} wants to open a root shell.",
            run: "{who} wants to run {what} as root.",
            shared_keep_root: "The Android apps {names} share a user ID and want to run commands \
                               as root without asking each time.",
            shared_shell: "The Android apps {names} share a user ID and want to open a root \
                           shell.",
            shared_run: "The Android apps {names} share a user ID and want to run {what} as \
                         root.",
        },
        UiLanguage::Korean => PromptWords {
            and_more: " 외 {more}개",
            shared_user_id: ": 사용자 ID를 함께 씀",
            interactive_shell: "대화형 셸",
            shell: "셸",
            any_command: "앞으로 묻지 않고 모든 명령",
            labelled: "{label}({package})",
            service_uid: "{name}(uid {uid})",
            android_app: "Android 앱 {app}",
            keep_root: "{who}에서 앞으로 묻지 않고 루트 권한으로 명령을 실행하려고 합니다.",
            open_shell: "{who}에서 루트 셸을 열려고 합니다.",
            run: "{who}에서 다음 명령을 루트 권한으로 실행하려고 합니다: {what}",
            shared_keep_root: "사용자 ID를 함께 쓰는 Android 앱 {names}에서 앞으로 묻지 않고 \
                               루트 권한으로 명령을 실행하려고 합니다.",
            shared_shell: "사용자 ID를 함께 쓰는 Android 앱 {names}에서 루트 셸을 열려고 \
                           합니다.",
            shared_run: "사용자 ID를 함께 쓰는 Android 앱 {names}에서 다음 명령을 루트 권한으로 \
                         실행하려고 합니다: {what}",
        },
        UiLanguage::Japanese => PromptWords {
            and_more: " ほか {more} 件",
            shared_user_id: ": 共有ユーザー ID",
            interactive_shell: "対話型シェル",
            shell: "シェル",
            any_command: "今後は毎回確認せずにすべてのコマンド",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "Android アプリ {app}",
            keep_root: "{who} が、今後は毎回確認せずに root としてコマンドを実行しようとして\
                        います。",
            open_shell: "{who} が root シェルを開こうとしています。",
            run: "{who} が次のコマンドを root として実行しようとしています: {what}",
            shared_keep_root: "ユーザー ID を共有する Android アプリ {names} が、今後は毎回確認\
                               せずに root としてコマンドを実行しようとしています。",
            shared_shell: "ユーザー ID を共有する Android アプリ {names} が root シェルを開こう\
                           としています。",
            shared_run: "ユーザー ID を共有する Android アプリ {names} が次のコマンドを root \
                         として実行しようとしています: {what}",
        },
        UiLanguage::ChineseSimplified => PromptWords {
            and_more: " 及其他 {more} 个",
            shared_user_id: "：共享一个用户 ID",
            interactive_shell: "交互式 shell",
            shell: "shell",
            any_command: "今后无需每次询问即可运行的任何命令",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "Android 应用 {app}",
            keep_root: "{who} 想要今后无需每次询问即以 root 身份运行命令。",
            open_shell: "{who} 想要打开 root shell。",
            run: "{who} 想要以 root 身份运行以下命令：{what}",
            shared_keep_root: "共享用户 ID 的 Android 应用 {names} 想要今后无需每次询问即以 \
                               root 身份运行命令。",
            shared_shell: "共享用户 ID 的 Android 应用 {names} 想要打开 root shell。",
            shared_run: "共享用户 ID 的 Android 应用 {names} 想要以 root 身份运行以下命令：\
                         {what}",
        },
        UiLanguage::ChineseTraditional => PromptWords {
            and_more: " 及其他 {more} 個",
            shared_user_id: "：共用一個使用者 ID",
            interactive_shell: "互動式 shell",
            shell: "shell",
            any_command: "今後不必每次詢問即可執行的任何命令",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "Android 應用程式 {app}",
            keep_root: "{who} 想要今後不必每次詢問即以 root 身分執行命令。",
            open_shell: "{who} 想要開啟 root shell。",
            run: "{who} 想要以 root 身分執行下列命令：{what}",
            shared_keep_root: "共用使用者 ID 的 Android 應用程式 {names} 想要今後不必每次詢問即以 \
                               root 身分執行命令。",
            shared_shell: "共用使用者 ID 的 Android 應用程式 {names} 想要開啟 root shell。",
            shared_run: "共用使用者 ID 的 Android 應用程式 {names} 想要以 root 身分執行下列命令：\
                         {what}",
        },
        UiLanguage::Spanish => PromptWords {
            and_more: " y {more} más",
            shared_user_id: ": un ID de usuario compartido",
            interactive_shell: "un shell interactivo",
            shell: "un shell",
            any_command: "cualquier comando, a partir de ahora sin preguntar cada vez",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "La aplicación de Android {app}",
            keep_root: "{who} quiere ejecutar comandos como root sin preguntar cada vez.",
            open_shell: "{who} quiere abrir un shell de root.",
            run: "{who} quiere ejecutar {what} como root.",
            shared_keep_root: "Las aplicaciones de Android {names} comparten un ID de usuario y \
                               quieren ejecutar comandos como root sin preguntar cada vez.",
            shared_shell: "Las aplicaciones de Android {names} comparten un ID de usuario y \
                           quieren abrir un shell de root.",
            shared_run: "Las aplicaciones de Android {names} comparten un ID de usuario y quieren \
                         ejecutar {what} como root.",
        },
        UiLanguage::French => PromptWords {
            and_more: " et {more} de plus",
            shared_user_id: " : un ID utilisateur partagé",
            interactive_shell: "un shell interactif",
            shell: "un shell",
            any_command: "toute commande, désormais sans demander à chaque fois",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "L'application Android {app}",
            keep_root: "{who} souhaite exécuter des commandes en tant que root sans demander à \
                        chaque fois.",
            open_shell: "{who} souhaite ouvrir un shell root.",
            run: "{who} souhaite exécuter {what} en tant que root.",
            shared_keep_root: "Les applications Android {names} partagent un ID utilisateur et \
                               souhaitent exécuter des commandes en tant que root sans demander \
                               à chaque fois.",
            shared_shell: "Les applications Android {names} partagent un ID utilisateur et \
                           souhaitent ouvrir un shell root.",
            shared_run: "Les applications Android {names} partagent un ID utilisateur et \
                         souhaitent exécuter {what} en tant que root.",
        },
        UiLanguage::German => PromptWords {
            and_more: " und {more} weitere",
            shared_user_id: ": eine gemeinsame Benutzer-ID",
            interactive_shell: "eine interaktive Shell",
            shell: "eine Shell",
            any_command: "beliebige Befehle, ab jetzt ohne jedes Mal zu fragen",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "Die Android-App {app}",
            keep_root: "{who} möchte Befehle als root ausführen, ohne jedes Mal zu fragen.",
            open_shell: "{who} möchte eine root-Shell öffnen.",
            run: "{who} möchte {what} als root ausführen.",
            shared_keep_root: "Die Android-Apps {names} teilen sich eine Benutzer-ID und möchten \
                               Befehle als root ausführen, ohne jedes Mal zu fragen.",
            shared_shell: "Die Android-Apps {names} teilen sich eine Benutzer-ID und möchten \
                           eine root-Shell öffnen.",
            shared_run: "Die Android-Apps {names} teilen sich eine Benutzer-ID und möchten \
                         {what} als root ausführen.",
        },
        UiLanguage::PortugueseBrazil => PromptWords {
            and_more: " e mais {more}",
            shared_user_id: ": um ID de usuário compartilhado",
            interactive_shell: "um shell interativo",
            shell: "um shell",
            any_command: "qualquer comando, de agora em diante sem perguntar toda vez",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "O app Android {app}",
            keep_root: "{who} quer executar comandos como root sem perguntar toda vez.",
            open_shell: "{who} quer abrir um shell root.",
            run: "{who} quer executar {what} como root.",
            shared_keep_root: "Os apps Android {names} compartilham um ID de usuário e querem \
                               executar comandos como root sem perguntar toda vez.",
            shared_shell: "Os apps Android {names} compartilham um ID de usuário e querem abrir \
                           um shell root.",
            shared_run: "Os apps Android {names} compartilham um ID de usuário e querem executar \
                         {what} como root.",
        },
        UiLanguage::Russian => PromptWords {
            and_more: " и ещё {more}",
            shared_user_id: ": общий идентификатор пользователя",
            interactive_shell: "интерактивная оболочка",
            shell: "оболочка",
            any_command: "любые команды, отныне без запроса каждый раз",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "Приложение Android {app}",
            keep_root: "{who} хочет выполнять команды от имени root без запроса каждый раз.",
            open_shell: "{who} хочет открыть оболочку root.",
            run: "{who} хочет выполнить {what} от имени root.",
            shared_keep_root: "Приложения Android {names} используют общий идентификатор \
                               пользователя и хотят выполнять команды от имени root без запроса \
                               каждый раз.",
            shared_shell: "Приложения Android {names} используют общий идентификатор \
                           пользователя и хотят открыть оболочку root.",
            shared_run: "Приложения Android {names} используют общий идентификатор пользователя \
                         и хотят выполнить {what} от имени root.",
        },
        UiLanguage::Italian => PromptWords {
            and_more: " e altri {more}",
            shared_user_id: ": un ID utente condiviso",
            interactive_shell: "una shell interattiva",
            shell: "una shell",
            any_command: "qualsiasi comando, d'ora in poi senza chiedere ogni volta",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "L'app Android {app}",
            keep_root: "{who} vuole eseguire comandi come root senza chiedere ogni volta.",
            open_shell: "{who} vuole aprire una shell root.",
            run: "{who} vuole eseguire {what} come root.",
            shared_keep_root: "Le app Android {names} condividono un ID utente e vogliono \
                               eseguire comandi come root senza chiedere ogni volta.",
            shared_shell: "Le app Android {names} condividono un ID utente e vogliono aprire una \
                           shell root.",
            shared_run: "Le app Android {names} condividono un ID utente e vogliono eseguire \
                         {what} come root.",
        },
        UiLanguage::Vietnamese => PromptWords {
            and_more: " và {more} gói khác",
            shared_user_id: ": dùng chung một ID người dùng",
            interactive_shell: "một shell tương tác",
            shell: "một shell",
            any_command: "mọi lệnh, từ nay không hỏi lại mỗi lần",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "Ứng dụng Android {app}",
            keep_root: "{who} muốn chạy lệnh với quyền root mà không hỏi mỗi lần.",
            open_shell: "{who} muốn mở một shell root.",
            run: "{who} muốn chạy {what} với quyền root.",
            shared_keep_root: "Các ứng dụng Android {names} dùng chung một ID người dùng và muốn \
                               chạy lệnh với quyền root mà không hỏi mỗi lần.",
            shared_shell: "Các ứng dụng Android {names} dùng chung một ID người dùng và muốn mở \
                           một shell root.",
            shared_run: "Các ứng dụng Android {names} dùng chung một ID người dùng và muốn chạy \
                         {what} với quyền root.",
        },
        UiLanguage::Indonesian => PromptWords {
            and_more: " dan {more} lainnya",
            shared_user_id: ": satu ID pengguna bersama",
            interactive_shell: "shell interaktif",
            shell: "shell",
            any_command: "perintah apa pun, mulai sekarang tanpa bertanya setiap kali",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "Aplikasi Android {app}",
            keep_root: "{who} ingin menjalankan perintah sebagai root tanpa bertanya setiap \
                        kali.",
            open_shell: "{who} ingin membuka shell root.",
            run: "{who} ingin menjalankan {what} sebagai root.",
            shared_keep_root: "Aplikasi Android {names} berbagi satu ID pengguna dan ingin \
                               menjalankan perintah sebagai root tanpa bertanya setiap kali.",
            shared_shell: "Aplikasi Android {names} berbagi satu ID pengguna dan ingin membuka \
                           shell root.",
            shared_run: "Aplikasi Android {names} berbagi satu ID pengguna dan ingin menjalankan \
                         {what} sebagai root.",
        },
        UiLanguage::Turkish => PromptWords {
            and_more: " ve {more} tane daha",
            shared_user_id: ": ortak bir kullanıcı kimliği",
            interactive_shell: "etkileşimli bir kabuk",
            shell: "bir kabuk",
            any_command: "bundan sonra her seferinde sormadan her komut",
            labelled: "{label} ({package})",
            service_uid: "{name} (uid {uid})",
            android_app: "{app} Android uygulaması",
            keep_root: "{who}, her seferinde sormadan komutları root olarak çalıştırmak istiyor.",
            open_shell: "{who}, bir root kabuğu açmak istiyor.",
            run: "{who}, {what} komutunu root olarak çalıştırmak istiyor.",
            shared_keep_root: "{names} Android uygulamaları ortak bir kullanıcı kimliği \
                               kullanıyor ve her seferinde sormadan komutları root olarak \
                               çalıştırmak istiyor.",
            shared_shell: "{names} Android uygulamaları ortak bir kullanıcı kimliği kullanıyor ve \
                           bir root kabuğu açmak istiyor.",
            shared_run: "{names} Android uygulamaları ortak bir kullanıcı kimliği kullanıyor ve \
                         {what} komutunu root olarak çalıştırmak istiyor.",
        },
    }
}

/// An app's label, or its package when the guest gave none.
fn label_or_package(app: &AppInfo) -> &str {
    let label = app.label.trim();
    if label.is_empty() {
        &app.package_name
    } else {
        label
    }
}

/// Splits a su request's package field into the packages of a shared user ID group: the guest
/// names every app that runs under the caller's uid, separated by commas.
pub fn package_group(package: &str) -> Vec<&str> {
    package
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// What su runs, in words: the command with each word that needs it quoted, or "an
/// interactive shell" when there is no command.
pub fn command_text(command: &[String], interactive: bool) -> String {
    if command.is_empty() {
        return if interactive {
            "an interactive shell".to_string()
        } else {
            "a shell".to_string()
        };
    }
    command
        .iter()
        .map(|word| quote_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A word quoted for a shell when it holds anything but plain characters.
fn quote_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// How the platform's administrator prompt answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAnswer {
    /// An administrator approved: the platform confirmed the answer came with administrator
    /// rights (an elevated process, an authorization right, a polkit decision).
    Approved,
    /// The prompt refused: wrong credentials, a refusal, or the account may not approve.
    Denied,
    /// The person dismissed the prompt without answering.
    Cancelled,
    /// The person at the computer is not one of its administrators, and no administrator
    /// answered for them.
    NotAdministrator,
    /// Nobody answered within the time the prompt had.
    TimedOut,
    /// The prompt could not be shown, or its answer could not be read; the detail says why.
    Unavailable(String),
}

/// The host OS's administrator prompt for one user.
pub trait ApprovalPrompt: Send + Sync {
    /// Shows the prompt for `request` in the user's session, naming the app and showing its
    /// icon where the platform can, and waits at most `timeout` for the answer. It must not
    /// approve on anything less than an answer made with administrator rights. It runs on a
    /// thread of its own, so a prompt that does not return in time only costs that thread.
    fn ask(&self, request: &ApprovalRequest, timeout: Duration) -> PromptAnswer;

    /// Takes down the prompt for `request` after it timed out, where the platform can. Its
    /// later answer, if any, is ignored either way.
    fn withdraw(&self, request: &ApprovalRequest) {
        let _ = request;
    }
}

/// An [`ApprovalPrompt`] backed by a folder on disk, for automated boot checks where no desktop
/// prompt is open. Each prompt appends one JSON line to [`REQUESTS_FILE`] and reads its answer
/// from [`ANSWER_FILE`].
#[derive(Debug, Clone)]
pub struct FolderApprover {
    dir: PathBuf,
}

impl FolderApprover {
    pub fn new(dir: &Path) -> FolderApprover {
        FolderApprover {
            dir: dir.to_path_buf(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn record_request(&self, request: &ApprovalRequest) -> Result<(), String> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| format!("creating {}: {e}", self.dir.display()))?;
        let path = self.dir.join(REQUESTS_FILE);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("opening {}: {e}", path.display()))?;
        let command = request
            .command
            .iter()
            .map(|arg| format!("\"{}\"", escape_json(arg)))
            .collect::<Vec<_>>()
            .join(",");
        let line = format!(
            "{{\"request_id\":{},\"package\":\"{}\",\"app_name\":\"{}\",\"uid\":{},\"icon_bytes\":{},\"command\":[{}],\"interactive\":{},\"message\":\"{}\"}}\n",
            request.request_id,
            escape_json(request.package()),
            escape_json(&request.app_name()),
            request.uid,
            request.icon_png().len(),
            command,
            request.interactive,
            escape_json(&request.message()),
        );
        file.write_all(line.as_bytes())
            .map_err(|e| format!("writing {}: {e}", path.display()))
    }
}

impl ApprovalPrompt for FolderApprover {
    fn ask(&self, request: &ApprovalRequest, timeout: Duration) -> PromptAnswer {
        if let Err(why) = self.record_request(request) {
            return PromptAnswer::Unavailable(why);
        }
        let path = self.dir.join(ANSWER_FILE);
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(text) = fs::read_to_string(&path) {
                let word = text.trim();
                if !word.is_empty() {
                    return match word {
                        "allow" | "approve" | "approved" => PromptAnswer::Approved,
                        "deny" | "refuse" | "refused" => PromptAnswer::Denied,
                        "cancel" | "cancelled" | "dismiss" => PromptAnswer::Cancelled,
                        "not-admin" | "not_administrator" => PromptAnswer::NotAdministrator,
                        "timeout" => {
                            let left = deadline.saturating_duration_since(Instant::now());
                            if !left.is_zero() {
                                thread::sleep(left);
                            }
                            PromptAnswer::TimedOut
                        }
                        other => PromptAnswer::Unavailable(format!(
                            "unknown answer {other:?} in {}",
                            path.display()
                        )),
                    };
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return PromptAnswer::TimedOut;
            }
            thread::sleep(FOLDER_POLL.min(deadline - now));
        }
    }
}

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The end of one request to the administrator prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Approved,
    /// Refused by the prompt.
    Denied,
    /// Dismissed by the person: denied too.
    Cancelled,
    /// The person is not an administrator and none answered: denied.
    NotAdministrator,
    /// No answer within the prompt's time.
    TimedOut,
    /// The prompt could not be shown: denied, with the reason.
    Unavailable(String),
}

impl Outcome {
    fn of(answer: PromptAnswer) -> Outcome {
        match answer {
            PromptAnswer::Approved => Outcome::Approved,
            PromptAnswer::Denied => Outcome::Denied,
            PromptAnswer::Cancelled => Outcome::Cancelled,
            PromptAnswer::NotAdministrator => Outcome::NotAdministrator,
            PromptAnswer::TimedOut => Outcome::TimedOut,
            PromptAnswer::Unavailable(why) => Outcome::Unavailable(why),
        }
    }

    /// The result the policy's caller gets: approved, or denied with the reason.
    pub fn result(&self) -> Result<(), DecisionDenied> {
        match self {
            Outcome::Approved => Ok(()),
            Outcome::Denied
            | Outcome::Cancelled
            | Outcome::NotAdministrator
            | Outcome::Unavailable(_) => Err(DecisionDenied::Refused),
            Outcome::TimedOut => Err(DecisionDenied::TimedOut),
        }
    }

    /// The sentence su prints and the log keeps, empty when approved.
    pub fn reason(&self) -> String {
        match self {
            Outcome::Approved => String::new(),
            Outcome::Denied => DecisionDenied::Refused.describe().to_string(),
            Outcome::Cancelled => {
                "the computer's administrator dismissed the root request".to_string()
            }
            Outcome::NotAdministrator => "the root request needs one of the computer's \
                                          administrators, and you are not one"
                .to_string(),
            Outcome::TimedOut => DecisionDenied::TimedOut.describe().to_string(),
            Outcome::Unavailable(why) => {
                format!("the computer could not ask its administrator: {why}")
            }
        }
    }

    /// How it went, for the log.
    fn how(&self, timeout: Duration) -> String {
        match self {
            Outcome::Approved => "approved by the computer's administrator".to_string(),
            Outcome::Denied => "refused by the computer's administrator".to_string(),
            Outcome::Cancelled => "refused: the administrator prompt was dismissed".to_string(),
            Outcome::NotAdministrator => {
                "refused: the person asked is not one of the computer's administrators".to_string()
            }
            Outcome::TimedOut => format!(
                "refused: timed out with no answer within {} seconds",
                timeout.as_secs()
            ),
            Outcome::Unavailable(why) => {
                format!("refused: the administrator prompt could not be shown ({why})")
            }
        }
    }
}

/// Asks `prompt` about `request` and waits at most `timeout` for its answer. A prompt that
/// does not answer in time is withdrawn and the request has timed out; a prompt that fails
/// (its thread panics) leaves the request denied, and the host keeps running.
pub fn ask_admin(
    prompt: &Arc<dyn ApprovalPrompt>,
    request: &ApprovalRequest,
    timeout: Duration,
) -> Outcome {
    let (tx, rx) = mpsc::channel();
    let (asked, question) = (Arc::clone(prompt), request.clone());
    let spawned = thread::Builder::new()
        .name("dsa-admin-prompt".into())
        .spawn(move || {
            let _ = tx.send(asked.ask(&question, timeout));
        });
    if let Err(why) = spawned {
        return Outcome::Unavailable(format!("no thread for the prompt: {why}"));
    }
    match rx.recv_timeout(timeout) {
        Ok(answer) => Outcome::of(answer),
        Err(RecvTimeoutError::Timeout) => {
            prompt.withdraw(request);
            Outcome::TimedOut
        }
        Err(RecvTimeoutError::Disconnected) => {
            Outcome::Unavailable("the prompt ended without an answer".to_string())
        }
    }
}

/// Says whether the feature is on and each app's level, as the settings have them now.
pub type LevelSource = Arc<dyn Fn() -> (bool, Vec<AppLevel>) + Send + Sync>;

/// A [`LevelSource`] that reads `user`'s levels from `store` at each request. A level the
/// store cannot read is off.
pub fn levels_from_settings(store: Arc<dyn SettingsStore>, user: &str) -> LevelSource {
    let user = user.to_string();
    Arc::new(move || crate::root_elevation::stored(&*store, &user))
}

/// Where the decisions are written: the host log.
pub type LogLine = Arc<dyn Fn(&str) + Send + Sync>;

/// The host log of the service: its standard error.
pub fn service_log() -> LogLine {
    Arc::new(|line: &str| eprintln!("{line}"))
}

/// The policy for the running boot, and whether the boot was started with the feature on and
/// has not seen it turned off since.
struct BootPolicy {
    booted_enabled: bool,
    policy: Policy,
}

/// Decides each su call of one user's Android.
pub struct RootApproval {
    user: String,
    levels: LevelSource,
    prompt: Arc<dyn ApprovalPrompt>,
    log: LogLine,
    /// A fixed time for every prompt, which tests set; `None` picks it by [`prompt_timeout`].
    timeout: Option<Duration>,
    /// Whether a screen reader runs, asked as each prompt is about to open.
    screen_reader: Option<ScreenReader>,
    boot: Mutex<BootPolicy>,
    /// Held while a prompt is open, so the user sees one prompt at a time and a second call
    /// from the same app finds the grace the first one's approval started.
    turn: Mutex<()>,
}

impl RootApproval {
    /// Decides `user`'s su calls by the settings in `store`, asking through `prompt`, and
    /// writes each decision with `log`. The policy is that of a boot started now.
    pub fn new(
        user: &str,
        store: Arc<dyn SettingsStore>,
        prompt: Arc<dyn ApprovalPrompt>,
        log: LogLine,
    ) -> RootApproval {
        let levels = levels_from_settings(store, user);
        RootApproval::with_levels(user, prompt, levels, log)
    }

    /// The same, with the switch and the levels read from `levels` at each request.
    pub fn with_levels(
        user: &str,
        prompt: Arc<dyn ApprovalPrompt>,
        levels: LevelSource,
        log: LogLine,
    ) -> RootApproval {
        let approval = RootApproval {
            user: user.to_string(),
            levels,
            prompt,
            log,
            timeout: None,
            screen_reader: None,
            boot: Mutex::new(BootPolicy {
                booted_enabled: false,
                policy: Policy::new(),
            }),
            turn: Mutex::new(()),
        };
        approval.restart();
        approval
    }

    /// Gives every prompt `timeout`, whether or not a screen reader runs, instead of the time
    /// [`prompt_timeout`] picks.
    pub fn with_timeout(mut self, timeout: Duration) -> RootApproval {
        self.timeout = Some(timeout);
        self
    }

    /// Asks `screen_reader` whether a screen reader runs each time a prompt is about to open,
    /// and gives the prompt [`SCREEN_READER_PROMPT_TIMEOUT`] while one does. Without it, every
    /// prompt has [`PROMPT_TIMEOUT`].
    pub fn with_screen_reader(mut self, screen_reader: ScreenReader) -> RootApproval {
        self.screen_reader = Some(screen_reader);
        self
    }

    /// The time the prompt about to open has: the fixed one, or the one for whether a screen
    /// reader runs now. A screen reader check that panics counts as none running, so the prompt
    /// still opens with its default time.
    fn prompt_time(&self) -> Duration {
        if let Some(fixed) = self.timeout {
            return fixed;
        }
        let running = self.screen_reader.as_ref().is_some_and(|check| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check())).unwrap_or(false)
        });
        prompt_timeout(running)
    }

    /// Writes each decision with `log` instead.
    pub fn with_log(mut self, log: LogLine) -> RootApproval {
        self.log = log;
        self
    }

    /// Starts the policy of a new boot of Android: the feature as the settings say now, the
    /// stored levels, and no grace for any app.
    pub fn restart(&self) {
        let (enabled, levels) = (self.levels)();
        *self.boot() = BootPolicy {
            booted_enabled: enabled,
            policy: Policy::with_levels(enabled, levels),
        };
    }

    fn boot(&self) -> MutexGuard<'_, BootPolicy> {
        self.boot.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Brings the boot's policy up to the settings: the feature turned off since the boot
    /// stops approvals, and a level changed since the last request applies (and clears that
    /// app's grace). Returns the decision for `package` now, and its level.
    ///
    /// Apps that share an Android user ID (`package` holds their comma-separated names) are
    /// decided as one group: if any app in the group is explicitly `Off` in the settings, the
    /// whole group is `Off` and its grace is cleared; otherwise the group takes its level and
    /// grace together.
    fn decide_now(&self, package: &str) -> (Decision, Level) {
        let (enabled, levels) = (self.levels)();
        let mut boot = self.boot();
        // Off applies at once and stays off for the rest of the boot: the guest's su guard
        // and relay are set up at boot, so turning the feature on again waits for the next
        // start of Android.
        if !enabled {
            boot.booted_enabled = false;
        }
        let enabled = boot.booted_enabled;
        let policy = &mut boot.policy;
        if policy.enabled() != enabled {
            policy.set_enabled(enabled);
        }
        let group = package_group(package);
        let group_has_explicit_off = group.len() > 1
            && group.iter().any(|member| {
                levels
                    .iter()
                    .any(|app| app.package == *member && app.level == Level::Off)
            });
        for app in policy.app_levels() {
            if !levels
                .iter()
                .any(|a| a.package == app.package && a.level != Level::Off)
            {
                policy.set_level(&app.package, Level::Off);
            }
        }
        for app in &levels {
            if policy.level(&app.package) != app.level {
                policy.set_level(&app.package, app.level);
            }
        }
        if group.len() <= 1 {
            let member = group.first().copied().unwrap_or(package);
            return (policy.decide(member, Instant::now()), policy.level(member));
        }
        if !enabled || group_has_explicit_off {
            for member in &group {
                if policy.level(member) != Level::Off {
                    policy.set_level(member, Level::Off);
                }
            }
            return (Decision::Deny, Level::Off);
        }
        let mut group_level = Level::Off;
        for member in &group {
            match policy.level(member) {
                Level::Remember => group_level = Level::Remember,
                Level::Ask if group_level == Level::Off => group_level = Level::Ask,
                _ => {}
            }
        }
        let now = Instant::now();
        let decision = match group_level {
            Level::Off => Decision::Deny,
            Level::Remember => Decision::ApproveWithoutPrompt,
            Level::Ask => {
                if group.iter().any(|member| policy.in_grace(member, now)) {
                    Decision::ApproveWithoutPrompt
                } else {
                    Decision::PromptHost
                }
            }
        };
        (decision, group_level)
    }

    fn record_approval(&self, package: &str) {
        let mut boot = self.boot();
        let now = Instant::now();
        let group = package_group(package);
        if group.is_empty() {
            boot.policy.approved(package, now);
            return;
        }
        for member in group {
            boot.policy.approved(member, now);
        }
    }

    /// Decides one su call. `app` is the calling app as the guest last listed it, for the name
    /// and icon the prompt shows. Blocks while the prompt is open, at most the prompt's time.
    pub fn decide(
        &self,
        request: &control::SuRequest,
        app: Option<&AppInfo>,
    ) -> control::SuDecision {
        self.answer(&ApprovalRequest::from_su(request, app))
    }

    /// Answers one su request: by the policy, and by the administrator prompt when the policy
    /// asks for one. Writes one line with the decision to the log.
    pub fn answer(&self, request: &ApprovalRequest) -> control::SuDecision {
        let asked = Instant::now();
        let package = request.package();
        let (decision, level) = self.decide_now(package);
        let result = match decision {
            Decision::Deny => Err((
                DecisionDenied::NotAllowed.describe().to_string(),
                "refused: the app may not use root".to_string(),
            )),
            Decision::ApproveWithoutPrompt => Ok(self.approve_without_prompt(package, level)),
            // The prompt's time counts from when su asked, so a call that waited for another
            // prompt to close still has its answer before su's own limit.
            Decision::PromptHost => {
                let timeout = self.prompt_time();
                self.prompt(request, asked + timeout, timeout)
            }
        };
        let (approved, reason, how) = match result {
            Ok(how) => (true, String::new(), how),
            Err((reason, how)) => (false, reason, how),
        };
        self.write(request, &how);
        control::SuDecision {
            request_id: request.request_id,
            approved,
            reason,
        }
    }

    /// A call the policy approves at once: a remember app, which also renews its standing
    /// state, or an app within its grace. Says how, for the log.
    fn approve_without_prompt(&self, package: &str, level: Level) -> String {
        if level == Level::Remember {
            self.record_approval(package);
            "approved without asking (the app may keep root)".to_string()
        } else {
            "approved without asking (within the grace of an earlier approval)".to_string()
        }
    }

    /// Waits for the prompt's turn, decides again (an approval in another prompt may have
    /// started the app's grace meanwhile), and asks the administrator. `Ok` says how it was
    /// approved; `Err` gives the reason su prints and how it went for the log.
    fn prompt(
        &self,
        request: &ApprovalRequest,
        deadline: Instant,
        timeout: Duration,
    ) -> Result<String, (String, String)> {
        let package = request.package();
        let timed_out = || {
            let outcome = Outcome::TimedOut;
            (outcome.reason(), outcome.how(timeout))
        };
        let Some(_turn) = self.wait_for_turn(deadline) else {
            return Err(timed_out());
        };
        let (decision, level) = self.decide_now(package);
        match decision {
            Decision::Deny => {
                return Err((
                    DecisionDenied::NotAllowed.describe().to_string(),
                    "refused: the app may not use root".to_string(),
                ))
            }
            Decision::ApproveWithoutPrompt => {
                return Ok(self.approve_without_prompt(package, level))
            }
            Decision::PromptHost => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out());
        }
        let outcome = ask_admin(&self.prompt, request, left);
        if outcome != Outcome::Approved {
            return Err((outcome.reason(), outcome.how(timeout)));
        }
        // The feature or the level may have changed while the prompt was open.
        if self.decide_now(package).0 == Decision::Deny {
            return Err((
                DecisionDenied::NotAllowed.describe().to_string(),
                "refused: the app may no longer use root".to_string(),
            ));
        }
        self.record_approval(package);
        Ok(outcome.how(timeout))
    }

    /// The prompt's turn, or `None` when `deadline` passes first.
    fn wait_for_turn(&self, deadline: Instant) -> Option<MutexGuard<'_, ()>> {
        loop {
            match self.turn.try_lock() {
                Ok(turn) => return Some(turn),
                Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(TURN_POLL);
                }
            }
        }
    }

    /// Asks the administrator to let `app` keep root without asking each time, as moving it
    /// to that level needs. Blocks at most the prompt's time. `Err` says why not.
    pub fn approve_keep_root(&self, app: AppInfo) -> Result<(), String> {
        self.approve_keep_root_request(ApprovalRequest::keep_root(app))
    }

    /// [`Self::approve_keep_root`] for a request made with [`ApprovalRequest::keep_root`],
    /// which may carry the language the prompt speaks.
    pub fn approve_keep_root_request(&self, request: ApprovalRequest) -> Result<(), String> {
        let asked = Instant::now();
        let timeout = self.prompt_time();
        let deadline = asked + timeout;
        let outcome = match self.wait_for_turn(deadline) {
            None => Outcome::TimedOut,
            Some(_turn) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    Outcome::TimedOut
                } else {
                    ask_admin(&self.prompt, &request, left)
                }
            }
        };
        self.write(&request, &outcome.how(timeout));
        match outcome {
            Outcome::Approved => Ok(()),
            other => Err(other.reason()),
        }
    }

    /// One line in the host log about `request`, with the time, the user and the app.
    fn write(&self, request: &ApprovalRequest, how: &str) {
        let since_epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        let id = if request.purpose == Purpose::Run {
            format!(" (request {})", request.request_id)
        } else {
            String::new()
        };
        (self.log)(&format!(
            "root: {} {}: {} ({}, uid {}) asked to run {}{id}: {how}",
            utc_stamp(since_epoch),
            self.user,
            request.app_name(),
            request.package(),
            request.uid,
            request.what(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A prompt that gives the same answer each time, after `delay`, and counts its prompts.
    struct FakePrompt {
        answer: PromptAnswer,
        delay: Duration,
        asked: AtomicUsize,
        withdrawn: AtomicUsize,
        seen: Mutex<Vec<ApprovalRequest>>,
        /// The time each prompt was given.
        timeouts: Mutex<Vec<Duration>>,
    }

    impl FakePrompt {
        fn new(answer: PromptAnswer) -> Arc<FakePrompt> {
            FakePrompt::slow(answer, Duration::ZERO)
        }
        fn slow(answer: PromptAnswer, delay: Duration) -> Arc<FakePrompt> {
            Arc::new(FakePrompt {
                answer,
                delay,
                asked: AtomicUsize::new(0),
                withdrawn: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
                timeouts: Mutex::new(Vec::new()),
            })
        }
    }

    impl ApprovalPrompt for FakePrompt {
        fn ask(&self, request: &ApprovalRequest, timeout: Duration) -> PromptAnswer {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(request.clone());
            self.timeouts.lock().unwrap().push(timeout);
            thread::sleep(self.delay);
            self.answer.clone()
        }
        fn withdraw(&self, _request: &ApprovalRequest) {
            self.withdrawn.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct PanickingPrompt;

    impl ApprovalPrompt for PanickingPrompt {
        fn ask(&self, _request: &ApprovalRequest, _timeout: Duration) -> PromptAnswer {
            panic!("the prompt broke");
        }
    }

    fn levels(enabled: bool, apps: &[(&str, Level)]) -> LevelSource {
        let apps: Vec<AppLevel> = apps
            .iter()
            .map(|(package, level)| AppLevel {
                package: package.to_string(),
                level: *level,
            })
            .collect();
        Arc::new(move || (enabled, apps.clone()))
    }

    fn request(package: &str) -> ApprovalRequest {
        ApprovalRequest {
            request_id: 7,
            app: AppInfo {
                package_name: package.to_string(),
                label: "Root Checker".to_string(),
                icon_png: vec![0x89, b'P', b'N', b'G'],
                ..Default::default()
            },
            uid: 10_123,
            command: vec!["id".to_string()],
            interactive: false,
            purpose: Purpose::Run,
            shared_apps: Vec::new(),
            language: String::new(),
        }
    }

    fn quiet() -> LogLine {
        Arc::new(|_: &str| {})
    }

    fn logged() -> (LogLine, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        (
            Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_string())),
            lines,
        )
    }

    fn elevation(prompt: Arc<dyn ApprovalPrompt>, source: LevelSource) -> RootApproval {
        RootApproval::with_levels("ann", prompt, source, quiet())
    }

    #[test]
    fn fr35_each_prompt_answer_maps_to_approved_or_denied() {
        let cases = [
            (PromptAnswer::Approved, Outcome::Approved, Ok(())),
            (
                PromptAnswer::Denied,
                Outcome::Denied,
                Err(DecisionDenied::Refused),
            ),
            (
                PromptAnswer::Cancelled,
                Outcome::Cancelled,
                Err(DecisionDenied::Refused),
            ),
            (
                PromptAnswer::NotAdministrator,
                Outcome::NotAdministrator,
                Err(DecisionDenied::Refused),
            ),
            (
                PromptAnswer::TimedOut,
                Outcome::TimedOut,
                Err(DecisionDenied::TimedOut),
            ),
            (
                PromptAnswer::Unavailable("no polkit agent".into()),
                Outcome::Unavailable("no polkit agent".into()),
                Err(DecisionDenied::Refused),
            ),
        ];
        for (answer, outcome, result) in cases {
            let prompt: Arc<dyn ApprovalPrompt> = FakePrompt::new(answer);
            let got = ask_admin(&prompt, &request("com.ask.app"), Duration::from_secs(5));
            assert_eq!(got, outcome);
            assert_eq!(got.result(), result);
        }
    }

    #[test]
    fn fr35_a_prompt_that_does_not_answer_in_time_times_out_and_is_withdrawn() {
        let fake = FakePrompt::slow(PromptAnswer::Approved, Duration::from_secs(2));
        let prompt: Arc<dyn ApprovalPrompt> = fake.clone();
        let got = ask_admin(&prompt, &request("com.ask.app"), Duration::from_millis(50));
        assert_eq!(got, Outcome::TimedOut);
        assert_eq!(got.result(), Err(DecisionDenied::TimedOut));
        assert_eq!(fake.withdrawn.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fr35_a_prompt_that_breaks_denies_and_the_host_keeps_running() {
        let prompt: Arc<dyn ApprovalPrompt> = Arc::new(PanickingPrompt);
        let got = ask_admin(&prompt, &request("com.ask.app"), Duration::from_secs(5));
        assert!(matches!(got, Outcome::Unavailable(_)));
        assert_eq!(got.result(), Err(DecisionDenied::Refused));
    }

    #[test]
    fn fr35_an_ask_app_is_approved_by_the_prompt_and_then_within_grace() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let (log, lines) = logged();
        let root =
            elevation(fake.clone(), levels(true, &[("com.ask.app", Level::Ask)])).with_log(log);
        let first = root.answer(&request("com.ask.app"));
        assert!(first.approved);
        assert_eq!(first.request_id, 7);
        assert!(first.reason.is_empty());
        let second = root.answer(&request("com.ask.app"));
        assert!(second.approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].app_name(), "Root Checker");
        assert_eq!(
            seen[0].message(),
            "The Android app Root Checker (com.ask.app) wants to run `id` as root."
        );
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("Root Checker (com.ask.app, uid 10123)"));
        assert!(lines[0].contains("`id`"));
        assert!(lines[0].contains("approved by the computer's administrator"));
        // The time, as an ISO 8601 UTC stamp.
        assert!(lines[0].contains('T') && lines[0].contains("Z "));
        assert!(lines[1].contains("within the grace"));
    }

    #[test]
    fn fr35_adb_shell_asks_for_root_like_an_app() {
        use crate::root_elevation::ADB_SHELL_PACKAGE;
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let root = elevation(
            fake.clone(),
            levels(true, &[(ADB_SHELL_PACKAGE, Level::Ask)]),
        );
        let mut shell = request(ADB_SHELL_PACKAGE);
        shell.app.label = "Shell".into();
        shell.uid = 2000;
        assert!(root.answer(&shell).approved);
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].app_name(), "adb shell");
        assert_eq!(seen[0].message(), "adb shell wants to run `id` as root.");
        drop(seen);
        // Refused, su fails.
        let refusing = FakePrompt::new(PromptAnswer::Denied);
        let root = elevation(refusing, levels(true, &[(ADB_SHELL_PACKAGE, Level::Ask)]));
        assert!(!root.answer(&shell).approved);
        // Left out of the levels (developer mode off), adb shell may not ask.
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let root = elevation(fake.clone(), levels(true, &[]));
        assert!(!root.answer(&shell).approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn fr35_the_root_prompt_names_an_app_by_its_label_and_icon() {
        let mut app = request("com.example.backup");
        app.app.label = "백업 도구".into();
        app.app.icon_png = vec![0x89, b'P', b'N', b'G', 1];
        app.app.caption_icon_png = vec![0x89, b'P', b'N', b'G', 2];
        let app = app.with_language("ko-KR");
        assert_eq!(app.requester(), Requester::App);
        assert_eq!(app.language(), UiLanguage::Korean);
        // The label is the name; the package is only the second line.
        assert_eq!(app.name_in(app.language()), "백업 도구");
        assert_eq!(app.detail_in(app.language()), "com.example.backup");
        // The launcher icon, as the app list and the window's program show it.
        assert_eq!(
            app.prompt_icon(),
            PromptIcon::App(&[0x89, b'P', b'N', b'G', 1])
        );
        assert_eq!(
            app.message_in(UiLanguage::Korean),
            "Android 앱 백업 도구(com.example.backup)에서 다음 명령을 루트 권한으로 \
             실행하려고 합니다: `id`"
        );
        assert_eq!(
            app.message_in(UiLanguage::English),
            "The Android app 백업 도구 (com.example.backup) wants to run `id` as root."
        );
        // An app the guest has not described is named by its package, with no second line.
        let mut bare = request("com.example.bare");
        bare.app.label.clear();
        bare.app.icon_png.clear();
        assert_eq!(bare.name_in(UiLanguage::Korean), "com.example.bare");
        assert_eq!(bare.detail_in(UiLanguage::Korean), "");
        assert_eq!(bare.prompt_icon(), PromptIcon::None);
    }

    #[test]
    fn fr35_the_root_prompt_names_the_adb_shell_as_the_adb_shell() {
        use crate::root_elevation::ADB_SHELL_PACKAGE;
        let mut shell = request(ADB_SHELL_PACKAGE);
        // Whatever label the guest gives the shell package, it is not the name shown.
        shell.app.label = "Shell".into();
        shell.uid = 2000;
        assert_eq!(shell.requester(), Requester::DeveloperShell);
        assert_eq!(shell.name_in(UiLanguage::Korean), "adb 셸");
        assert_eq!(shell.name_in(UiLanguage::English), "adb shell");
        assert_eq!(
            shell.detail_in(UiLanguage::English),
            "com.android.shell (uid 2000)"
        );
        assert_eq!(shell.prompt_icon(), PromptIcon::Generic(GenericIcon::Shell));
        assert_eq!(
            shell.message_in(UiLanguage::Korean),
            "adb 셸에서 다음 명령을 루트 권한으로 실행하려고 합니다: `id`"
        );
        let mut interactive = shell.clone();
        interactive.command.clear();
        interactive.interactive = true;
        assert_eq!(
            interactive.message_in(UiLanguage::Korean),
            "adb 셸에서 루트 셸을 열려고 합니다."
        );
        assert_eq!(interactive.command_text_in(UiLanguage::Korean), "대화형 셸");
        // The shell uid names the shell even when the package list left the package out, and
        // in a second Android user.
        let mut by_uid = request("");
        by_uid.uid = 102_000;
        assert_eq!(by_uid.requester(), Requester::DeveloperShell);
        // Keeping root for the shell is asked under the same name.
        let keep = ApprovalRequest::keep_root(shell.app.clone());
        assert_eq!(keep.name_in(UiLanguage::Korean), "adb 셸");
    }

    #[test]
    fn fr35_the_root_prompt_names_android_system_uids_in_words() {
        let mut system = request("android, com.android.settings, com.android.providers.settings, com.android.location.fused");
        system.uid = 1000;
        system.app.label = "Android System".into();
        assert_eq!(system.requester(), Requester::System);
        assert_eq!(system.name_in(UiLanguage::Korean), "Android 시스템");
        assert_eq!(system.name_in(UiLanguage::English), "Android system");
        assert_eq!(
            system.detail_in(UiLanguage::Korean),
            "android, com.android.settings, com.android.providers.settings 외 1개 (uid 1000)"
        );
        assert_eq!(
            system.prompt_icon(),
            PromptIcon::Generic(GenericIcon::System)
        );
        assert_eq!(
            system.message_in(UiLanguage::English),
            "Android system (uid 1000) wants to run `id` as root."
        );
        // An app uid is never taken for Android's own.
        assert_eq!(request("com.ask.app").requester(), Requester::App);
    }

    #[test]
    fn fr35_the_root_prompt_names_a_shared_user_id_by_its_apps() {
        let su = control::SuRequest {
            request_id: 4,
            package: "com.shared.one, com.shared.two".into(),
            uid: 10_150,
            command: vec!["id".into()],
            interactive: false,
        };
        let shared = ApprovalRequest::from_su(&su, None)
            .with_shared_apps(vec![
                AppInfo {
                    package_name: "com.shared.one".into(),
                    label: "공유 하나".into(),
                    ..Default::default()
                },
                AppInfo {
                    package_name: "com.shared.two".into(),
                    label: "공유 둘".into(),
                    icon_png: vec![0x89, b'P', b'N', b'G', 9],
                    ..Default::default()
                },
            ])
            .with_language("ko");
        assert_eq!(shared.requester(), Requester::SharedApps);
        assert_eq!(shared.name_in(UiLanguage::Korean), "공유 하나, 공유 둘");
        assert_eq!(
            shared.detail_in(UiLanguage::Korean),
            "com.shared.one, com.shared.two: 사용자 ID를 함께 씀"
        );
        // The first app in the group that has an icon.
        assert_eq!(
            shared.prompt_icon(),
            PromptIcon::App(&[0x89, b'P', b'N', b'G', 9])
        );
        assert!(shared
            .message_in(UiLanguage::Korean)
            .starts_with("사용자 ID를 함께 쓰는 Android 앱 공유 하나(com.shared.one), 공유 둘(com.shared.two)에서"));
    }

    /// The prompt speaks the user's first display language when DSA has it, else English, as
    /// the interface languages map system locales.
    #[test]
    fn fr17_the_root_prompt_language_follows_the_users_first_language() {
        let cases = [
            ("ko-KR", UiLanguage::Korean),
            ("ko_KR.UTF-8", UiLanguage::Korean),
            ("KO", UiLanguage::Korean),
            ("en-US", UiLanguage::English),
            ("ja-JP", UiLanguage::Japanese),
            ("zh-Hant-HK", UiLanguage::ChineseTraditional),
            ("zh-SG", UiLanguage::ChineseSimplified),
            ("pt-PT", UiLanguage::PortugueseBrazil),
            ("in-ID", UiLanguage::Indonesian),
            ("nl-NL", UiLanguage::English),
            ("", UiLanguage::English),
        ];
        for (tag, language) in cases {
            assert_eq!(
                request("com.ask.app").with_language(tag).language(),
                language,
                "{tag}"
            );
        }
    }

    /// Every requester and every purpose has its prompt in every interface language: the
    /// sentence names who asks and the command as English does, nothing is left empty or
    /// unfilled, and no em dash shows.
    #[test]
    fn fr17_the_root_prompt_reads_in_every_language() {
        let mut shell = request(crate::root_elevation::ADB_SHELL_PACKAGE);
        shell.uid = 2000;
        let mut system = request("android");
        system.uid = 1000;
        let mut keep = ApprovalRequest::keep_root(request("com.ask.app").app);
        keep.app.label = "Root Checker".into();
        let mut interactive = request("com.ask.app");
        interactive.command.clear();
        interactive.interactive = true;
        let mut many = request("a.one, a.two, a.three, a.four, a.five");
        many.uid = 1013;
        let shared_apps: Vec<AppInfo> = ["com.shared.one", "com.shared.two"]
            .iter()
            .map(|package| AppInfo {
                package_name: (*package).into(),
                label: format!("Label {package}"),
                ..Default::default()
            })
            .collect();
        let shared = request("com.shared.one, com.shared.two").with_shared_apps(shared_apps);
        let mut shared_keep = shared.clone();
        shared_keep.purpose = Purpose::KeepRoot;
        let mut shared_shell = shared.clone();
        shared_shell.command.clear();
        let requests = [
            request("com.ask.app"),
            shell,
            system,
            keep,
            interactive,
            many,
            shared,
            shared_keep,
            shared_shell,
        ];
        let dash = char::from_u32(0x2014).unwrap();
        for language in UiLanguage::ALL {
            for asked in &requests {
                let message = asked.message_in(language);
                let name = asked.name_in(language);
                let detail = asked.detail_in(language);
                let command = asked.command_text_in(language);
                assert!(!message.trim().is_empty(), "{language:?}");
                assert!(!name.trim().is_empty(), "{language:?}");
                assert!(!command.trim().is_empty(), "{language:?}");
                for words in [&message, &name, &detail, &command] {
                    assert!(!words.contains(dash), "{language:?}: {words}");
                    for left in ["{who}", "{what}", "{names}", "{app}", "{more}", "{uid}"] {
                        assert!(!words.contains(left), "{language:?}: {words}");
                    }
                }
                if asked.purpose == Purpose::Run && !asked.command.is_empty() {
                    assert!(message.contains("`id`"), "{language:?}: {message}");
                }
                match asked.requester() {
                    Requester::App => {
                        assert!(
                            message.contains(&asked.app.label),
                            "{language:?}: {message}"
                        )
                    }
                    Requester::SharedApps => {
                        for app in &asked.shared_apps {
                            assert!(message.contains(&app.label), "{language:?}: {message}");
                            assert!(message.contains(&app.package_name), "{language:?}");
                        }
                    }
                    Requester::System => assert!(
                        message.contains(&format!("uid {}", asked.uid)),
                        "{language:?}: {message}"
                    ),
                    Requester::DeveloperShell => {
                        assert!(message.contains("adb"), "{language:?}: {message}")
                    }
                }
            }
            // A long list says how many more packages there are.
            let more = requests[5].detail_in(language);
            assert!(more.contains('2'), "{language:?}: {more}");
        }
    }

    #[test]
    fn fr17_a_command_with_braces_is_shown_as_it_is() {
        let mut asked = request("com.ask.app");
        asked.command = vec!["echo".into(), "{who}{what}".into()];
        for language in UiLanguage::ALL {
            let message = asked.message_in(language);
            assert!(
                message.contains("`echo '{who}{what}'`"),
                "{language:?}: {message}"
            );
        }
    }

    #[test]
    fn fr35_a_refused_dismissed_or_timed_out_prompt_fails_su_with_its_reason() {
        for (answer, delay, words) in [
            (PromptAnswer::Denied, Duration::ZERO, "refused"),
            (PromptAnswer::Cancelled, Duration::ZERO, "dismissed"),
            (PromptAnswer::Approved, Duration::from_secs(2), "timed out"),
        ] {
            let fake = FakePrompt::slow(answer, delay);
            let (log, lines) = logged();
            let root = elevation(fake.clone(), levels(true, &[("com.ask.app", Level::Ask)]))
                .with_timeout(Duration::from_millis(100))
                .with_log(log);
            let decision = root.answer(&request("com.ask.app"));
            assert!(!decision.approved);
            assert!(decision.reason.contains(words), "{}", decision.reason);
            assert!(lines.lock().unwrap()[0].contains("refused"));
            // No grace was started: the next request asks again.
            let _ = root.answer(&request("com.ask.app"));
            assert_eq!(fake.asked.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn fr35_remember_and_off_apps_never_reach_the_prompt() {
        let fake = FakePrompt::new(PromptAnswer::Denied);
        let (log, lines) = logged();
        let root = elevation(
            fake.clone(),
            levels(true, &[("com.remember.app", Level::Remember)]),
        )
        .with_log(log);
        assert!(root.answer(&request("com.remember.app")).approved);
        let off = root.answer(&request("com.other.app"));
        assert!(!off.approved);
        assert_eq!(off.reason, DecisionDenied::NotAllowed.describe());
        assert_eq!(fake.asked.load(Ordering::SeqCst), 0);
        let lines = lines.lock().unwrap();
        assert!(lines[0].contains("without asking"));
        assert!(lines[1].contains("may not use root"));
    }

    #[test]
    fn fr35_a_restart_clears_the_grace_and_asks_again() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let root = elevation(fake.clone(), levels(true, &[("com.ask.app", Level::Ask)]));
        root.answer(&request("com.ask.app"));
        root.restart();
        root.answer(&request("com.ask.app"));
        assert_eq!(fake.asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fr35_a_level_change_in_the_settings_clears_the_grace() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let level = Arc::new(Mutex::new(Level::Ask));
        let current = Arc::clone(&level);
        let source: LevelSource = Arc::new(move || {
            (
                true,
                vec![AppLevel {
                    package: "com.ask.app".into(),
                    level: *current.lock().unwrap(),
                }],
            )
        });
        let root = elevation(fake.clone(), source);
        root.answer(&request("com.ask.app"));
        *level.lock().unwrap() = Level::Off;
        assert!(!root.answer(&request("com.ask.app")).approved);
        *level.lock().unwrap() = Level::Ask;
        assert!(root.answer(&request("com.ask.app")).approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fr35_requests_that_arrive_together_get_one_prompt() {
        let fake = FakePrompt::slow(PromptAnswer::Approved, Duration::from_millis(200));
        let root = Arc::new(elevation(
            fake.clone(),
            levels(true, &[("com.ask.app", Level::Ask)]),
        ));
        let others: Vec<_> = (0..3)
            .map(|_| {
                let root = Arc::clone(&root);
                thread::spawn(move || root.answer(&request("com.ask.app")).approved)
            })
            .collect();
        for other in others {
            assert!(other.join().unwrap());
        }
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fr35_the_levels_follow_the_computers_switch_and_the_users_apps() {
        use crate::settings::{MemorySettings, SettingsStore};
        let store = Arc::new(MemorySettings::default());
        store.set_root_app("ann", "com.ask.app", "ask").unwrap();
        store
            .set_root_app("ann", "com.keep.app", "remember")
            .unwrap();
        store.set_root_app("bob", "com.bob.app", "ask").unwrap();
        let source = levels_from_settings(store.clone(), "ann");
        let (enabled, apps) = source();
        assert!(!enabled);
        assert_eq!(apps.len(), 2);
        let mut machine = store.machine();
        machine.root_elevation = true;
        store.set_machine(machine).unwrap();
        let (enabled, apps) = source();
        assert!(enabled);
        assert!(apps.contains(&AppLevel {
            package: "com.keep.app".into(),
            level: Level::Remember,
        }));
        assert!(!apps.iter().any(|a| a.package == "com.bob.app"));
    }

    #[test]
    fn fr35_the_prompt_names_a_shell_and_quotes_the_command() {
        let mut shell = request("com.ask.app");
        shell.command.clear();
        shell.interactive = true;
        shell.app.label.clear();
        assert_eq!(
            shell.message(),
            "The Android app com.ask.app wants to open a root shell."
        );
        let mut quoted = request("com.ask.app");
        quoted.command = vec!["sh".into(), "-c".into(), "echo 'hi' > /x".into()];
        assert_eq!(quoted.command_line(), r"sh -c 'echo '\''hi'\'' > /x'");
    }

    #[test]
    fn fr35_keeping_root_is_asked_of_the_administrator_under_the_apps_name() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let (log, lines) = logged();
        let root = elevation(fake.clone(), levels(true, &[])).with_log(log);
        let app = request("com.keep.app").app;
        assert_eq!(root.approve_keep_root(app.clone()), Ok(()));
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].purpose, Purpose::KeepRoot);
        assert_eq!(seen[0].icon_png(), app.icon_png.as_slice());
        assert_eq!(
            seen[0].message(),
            "The Android app Root Checker (com.keep.app) wants to run commands as root \
             without asking each time."
        );
        assert!(lines.lock().unwrap()[0].contains("approved by the computer's administrator"));

        let refusing = elevation(FakePrompt::new(PromptAnswer::Denied), levels(true, &[]));
        let refused = refusing.approve_keep_root(app).unwrap_err();
        assert!(refused.contains("refused"), "{refused}");
    }

    #[test]
    fn fr35_apps_sharing_a_user_id_are_approved_as_one_group() {
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let current = Arc::new(Mutex::new(vec![AppLevel {
            package: "com.shared.one".into(),
            level: Level::Ask,
        }]));
        let shared_levels = Arc::clone(&current);
        let source: LevelSource = Arc::new(move || (true, shared_levels.lock().unwrap().clone()));
        let root = elevation(fake.clone(), source);

        let su = control::SuRequest {
            request_id: 11,
            package: "com.shared.one, com.shared.two".into(),
            uid: 10_150,
            command: vec!["id".into()],
            interactive: false,
        };
        let req = ApprovalRequest::from_su(&su, None).with_shared_apps(vec![
            AppInfo {
                package_name: "com.shared.one".into(),
                label: "Shared One".into(),
                ..Default::default()
            },
            AppInfo {
                package_name: "com.shared.two".into(),
                label: "Shared Two".into(),
                ..Default::default()
            },
        ]);
        let first = root.answer(&req);
        assert!(first.approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
        let seen = fake.seen.lock().unwrap();
        assert!(seen[0].message().contains("Shared One (com.shared.one)"));
        assert!(seen[0].message().contains("Shared Two (com.shared.two)"));
        assert!(seen[0].message().contains("share a user ID"));
        drop(seen);

        // The second app in the group shares the grace started by the first approval.
        let second = root.answer(&req);
        assert!(second.approved);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);

        // If any app in the group is set to Off, the whole group is denied and cannot use root.
        current.lock().unwrap().push(AppLevel {
            package: "com.shared.two".into(),
            level: Level::Off,
        });
        let denied = root.answer(&req);
        assert!(!denied.approved);
        assert_eq!(denied.reason, DecisionDenied::NotAllowed.describe());
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fr35_the_folder_approver_records_each_request_and_answers_from_its_file() {
        let dir = std::env::temp_dir().join(format!(
            "dsa-folder-approver-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let approver = FolderApprover::new(&dir);
        fs::write(dir.join(ANSWER_FILE), "allow\n").unwrap();
        let answer = approver.ask(&request("com.ask.app"), Duration::from_secs(1));
        assert_eq!(answer, PromptAnswer::Approved);
        let recorded = fs::read_to_string(dir.join(REQUESTS_FILE)).unwrap();
        assert!(recorded.contains("\"package\":\"com.ask.app\""));
        assert!(recorded.contains("\"app_name\":\"Root Checker\""));
        assert!(recorded.contains("\"uid\":10123"));
        assert!(recorded.contains("\"icon_bytes\":4"));

        fs::write(dir.join(ANSWER_FILE), "deny\n").unwrap();
        let denied = approver.ask(&request("com.ask.app"), Duration::from_secs(1));
        assert_eq!(denied, PromptAnswer::Denied);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr18_prompt_waits_longer_while_a_screen_reader_runs() {
        assert_eq!(prompt_timeout(false), PROMPT_TIMEOUT);
        assert_eq!(prompt_timeout(true), SCREEN_READER_PROMPT_TIMEOUT);
        assert!(SCREEN_READER_PROMPT_TIMEOUT > PROMPT_TIMEOUT);

        // The screen reader is asked about as each prompt opens, so one started after DSA
        // started still gets the longer time, and one stopped gives the default back.
        let running = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let asked = Arc::new(AtomicUsize::new(0));
        let (flag, count) = (Arc::clone(&running), Arc::clone(&asked));
        let fake = FakePrompt::new(PromptAnswer::Denied);
        let root = elevation(fake.clone(), levels(true, &[("com.ask.app", Level::Ask)]))
            .with_screen_reader(Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                flag.load(Ordering::SeqCst)
            }));
        let _ = root.answer(&request("com.ask.app"));
        running.store(true, Ordering::SeqCst);
        let _ = root.answer(&request("com.ask.app"));
        assert!(root.approve_keep_root(request("com.ask.app").app).is_err());
        running.store(false, Ordering::SeqCst);
        let _ = root.answer(&request("com.ask.app"));
        let timeouts = fake.timeouts.lock().unwrap().clone();
        assert_eq!(timeouts.len(), 4);
        assert!(timeouts[0] <= PROMPT_TIMEOUT);
        assert!(timeouts[1] > PROMPT_TIMEOUT && timeouts[1] <= SCREEN_READER_PROMPT_TIMEOUT);
        assert!(timeouts[2] > PROMPT_TIMEOUT && timeouts[2] <= SCREEN_READER_PROMPT_TIMEOUT);
        assert!(timeouts[3] <= PROMPT_TIMEOUT);
        assert_eq!(asked.load(Ordering::SeqCst), 4);

        // A call the policy answers without a prompt does not ask about a screen reader, which
        // on some hosts means starting a process in the user's session.
        let asked = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&asked);
        let root = elevation(
            FakePrompt::new(PromptAnswer::Approved),
            levels(true, &[("com.keep.app", Level::Remember)]),
        )
        .with_screen_reader(Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            true
        }));
        assert!(root.answer(&request("com.keep.app")).approved);
        assert!(!root.answer(&request("com.off.app")).approved);
        assert_eq!(asked.load(Ordering::SeqCst), 0);

        // A check that breaks leaves the prompt its default time rather than failing it.
        let fake = FakePrompt::new(PromptAnswer::Approved);
        let root = elevation(fake.clone(), levels(true, &[("com.ask.app", Level::Ask)]))
            .with_screen_reader(Arc::new(|| -> bool {
                panic!("the screen reader check broke")
            }));
        assert!(root.answer(&request("com.ask.app")).approved);
        assert!(fake.timeouts.lock().unwrap()[0] <= PROMPT_TIMEOUT);
    }

    #[test]
    fn fr18_host_timeout_still_reaches_su_first() {
        use crate::root_elevation::ANSWER_TIMEOUT;
        // Whichever time the prompt has, the host answers "timed out" before su stops waiting,
        // with room for the answer to cross to the guest.
        for screen_reader in [false, true] {
            let prompt = prompt_timeout(screen_reader);
            assert!(
                prompt + Duration::from_secs(5) <= ANSWER_TIMEOUT,
                "a prompt of {prompt:?} leaves su's {ANSWER_TIMEOUT:?} no room"
            );
        }
    }

    #[test]
    fn fr18_su_waits_past_the_longer_prompt() {
        // With a screen reader, a prompt nobody answers ends in the host's own "timed out",
        // which names the longer time, and that time is within what su waits.
        let fake = FakePrompt::new(PromptAnswer::TimedOut);
        let (log, lines) = logged();
        let root = elevation(fake.clone(), levels(true, &[("com.ask.app", Level::Ask)]))
            .with_screen_reader(Arc::new(|| true))
            .with_log(log);
        let decision = root.answer(&request("com.ask.app"));
        assert!(!decision.approved);
        assert_eq!(decision.reason, DecisionDenied::TimedOut.describe());
        let given = fake.timeouts.lock().unwrap()[0];
        assert!(given > PROMPT_TIMEOUT);
        assert!(given < crate::root_elevation::ANSWER_TIMEOUT);
        let line = lines.lock().unwrap()[0].clone();
        assert!(
            line.contains(&format!(
                "within {} seconds",
                SCREEN_READER_PROMPT_TIMEOUT.as_secs()
            )),
            "{line}"
        );
    }
}
