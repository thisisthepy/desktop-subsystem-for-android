//! An Android app's request to put one of its shortcuts on the desktop (`requestPinShortcut`).
//!
//! The agent is Android's default launcher, so the request comes to it, and it passes the
//! request to the host, as a phone's launcher would ask on its home screen. The window manager
//! asks the user in a Windows 11 dialog ([`crate::compositor::fluent_dialog`]) with the
//! shortcut's name and the app's: [Add to desktop], the accent button, or [Cancel]. On yes the shortcut goes on the desktop
//! ([`crate::desktop_shortcuts`]), starting that shortcut of the app as its jump list task
//! does, and Android is told to pin it; otherwise Android is told no. It goes when the app is
//! uninstalled, as the app's own desktop shortcut does.
//!
//! The questions are asked one at a time on a thread of their own, so the windows never wait
//! on the user. A question nobody answers within [`ANSWER_WITHIN`] is answered no, as is one
//! that waited that long behind another.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::tasks::PinShortcutRequest;
use dsa_host_core::ui_language::UiLanguage;
use dsa_host_core::ui_text::fill;
use dsa_host_core::ui_text::pin_shortcut::pin_question_text;
use dsa_transport::Connector;

use crate::compositor::fluent_dialog::{self, Answer, DialogText};
use crate::compositor::log::{note, CompositorLogger};
use crate::desktop_shortcuts::{entry_in, Desktop};
use crate::start_menu::{ShellLinks, Shortcuts};

/// What the window manager calls itself to the service.
const BUILD: &str = "dsa-wm pin shortcuts";

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);

/// How long a question waits for the user before it is answered no.
pub const ANSWER_WITHIN: Duration = dsa_host_core::pin_questions::ASK_FOR;

/// The words of the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texts {
    language: UiLanguage,
}

impl Texts {
    pub fn english() -> Texts {
        Texts::of(UiLanguage::English)
    }

    /// The words in `language`.
    pub fn of(language: UiLanguage) -> Texts {
        Texts { language }
    }

    /// The words in the user's display language.
    pub fn for_user() -> Texts {
        Texts::of(crate::ui_language::user_language())
    }

    /// The question's words as written: its heading (`{name}` is the shortcut's name), what it
    /// says below (`{app}` is the app's name, then that the shortcut goes with the app), and
    /// its two buttons.
    pub fn templates(&self) -> [String; 4] {
        let text = pin_question_text(self.language);
        [
            text.heading.to_string(),
            format!("{}{}", text.body, text.removed_with_app),
            text.add.to_string(),
            text.cancel.to_string(),
        ]
    }

    /// The question's heading, about the shortcut named `shortcut`.
    pub fn instruction(&self, shortcut: &str) -> String {
        fill(&self.templates()[0], &[("name", shortcut)])
    }

    /// What the question says below its heading, for the app named `app`.
    pub fn content(&self, app: &str) -> String {
        fill(&self.templates()[1], &[("app", app)])
    }

    pub fn add(&self) -> &'static str {
        pin_question_text(self.language).add
    }

    pub fn cancel(&self) -> &'static str {
        pin_question_text(self.language).cancel
    }
}

/// The question as it is put to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// The dialog's title: the app's name.
    pub title: String,
    pub instruction: String,
    pub content: String,
    pub add: String,
    pub cancel: String,
}

/// The question about `request`.
pub fn question(request: &PinShortcutRequest, texts: &Texts) -> Question {
    let app = request.app_name().to_string();
    Question {
        instruction: texts.instruction(request.shortcut.label()),
        content: texts.content(&app),
        add: texts.add().to_string(),
        cancel: texts.cancel().to_string(),
        title: app,
    }
}

/// Puts a question to the user.
pub trait Ask {
    /// Whether the user said yes to `question` within `within`; no when the time ran out.
    fn ask(&self, question: &Question, within: Duration) -> bool;
}

/// The access keys of the dialog's controls: no check box, A for [Add to desktop] and C for
/// [Cancel], in every language, as the stop dialog's keys are.
pub const ACCESS_KEYS: [Option<char>; 3] = [None, Some('A'), Some('C')];

/// The Windows 11 dialog's words for `question`: the heading as its title, the app's request as
/// its body, no check box, and [Add to desktop] as the primary (accent) button.
pub fn dialog_text(question: &Question) -> DialogText {
    DialogText {
        title: question.instruction.clone(),
        body: question.content.clone(),
        check: None,
        primary: question.add.clone(),
        secondary: question.cancel.clone(),
        keys: ACCESS_KEYS,
    }
}

/// Whether the dialog's answer is yes: only its primary button is. Cancel, Escape, closing it
/// and the time running out are no.
pub fn accepted(answer: Answer) -> bool {
    matches!(answer, Answer::Confirmed { .. })
}

/// The dialog showing `text`, which closes as cancelled once `within` has passed.
pub fn dialog_request(text: &DialogText, within: Duration) -> fluent_dialog::Request<'_> {
    fluent_dialog::Request {
        text,
        timeout: Some(within),
    }
}

/// Asks in the Windows 11 dialog, which closes as no once `within` has passed.
pub struct FluentDialogAsk;

impl Ask for FluentDialogAsk {
    fn ask(&self, question: &Question, within: Duration) -> bool {
        let text = dialog_text(question);
        accepted(fluent_dialog::show(dialog_request(&text, within)))
    }
}

/// Where a pinned shortcut is made: the user's desktop, and the folder of the apps' programs.
pub struct Place {
    pub desktop: Desktop,
    pub apps_dir: PathBuf,
}

impl Place {
    /// The user's own desktop and apps folder.
    pub fn for_user() -> Option<Place> {
        Some(Place {
            desktop: Desktop::default_for_user()?,
            apps_dir: crate::app_exe::default_apps_dir()?,
        })
    }

    /// Makes the desktop shortcut that starts the shortcut of `request`; returns its path.
    pub fn make(
        &self,
        request: &PinShortcutRequest,
        shortcuts: &dyn Shortcuts,
    ) -> Result<PathBuf, String> {
        let entry = entry_in(&self.apps_dir, &request.package_name, &request.app_label)?;
        let app_dir = self.apps_dir.join(&request.package_name);
        self.desktop
            .place_shortcut(&entry, &request.shortcut, &app_dir, shortcuts)
    }
}

/// Decides one request: asks the user unless it waited too long already, makes the desktop
/// shortcut on yes, and returns the answer for Android (yes only when the shortcut was made)
/// and what to log.
pub fn decide(
    request: &PinShortcutRequest,
    waited: Duration,
    ask: &dyn Ask,
    texts: &Texts,
    place: Option<&Place>,
    shortcuts: &dyn Shortcuts,
) -> (bool, String) {
    let what = format!(
        "{}'s shortcut {:?}",
        request.package_name,
        request.shortcut.label()
    );
    let Some(within) = ANSWER_WITHIN
        .checked_sub(waited)
        .filter(|left| !left.is_zero())
    else {
        return (
            false,
            format!("{what} waited too long to be asked; declined"),
        );
    };
    let Some(place) = place else {
        return (
            false,
            format!("{what}: the desktop cannot be found; declined"),
        );
    };
    if !ask.ask(&question(request, texts), within) {
        return (false, format!("{what}: the user declined"));
    }
    match place.make(request, shortcuts) {
        Ok(path) => (
            true,
            format!("{what} is on the desktop: {}", path.display()),
        ),
        Err(why) => (false, format!("{what} could not go on the desktop: {why}")),
    }
}

/// Asks the user about each request that arrives on `requests`, until it closes, and gives
/// Android the answers through the service on `connector`.
pub fn serve(
    requests: Receiver<(PinShortcutRequest, Instant)>,
    mut connector: Box<dyn Connector>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let _com = crate::start_menu::ComApartment::enter();
    let texts = Texts::for_user();
    let place = Place::for_user();
    while let Ok((request, arrived)) = requests.recv() {
        let (accepted, said) = decide(
            &request,
            arrived.elapsed(),
            &FluentDialogAsk,
            &texts,
            place.as_ref(),
            &ShellLinks,
        );
        note(logger.as_deref(), &format!("pin shortcut: {said}"));
        let answered = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|mut client| client.answer_pin_shortcut(request.request_id, accepted));
        if let Err(why) = answered {
            note(
                logger.as_deref(),
                &format!(
                    "pin shortcut: Android was not told the answer to request {}: {why}",
                    request.request_id
                ),
            );
        }
    }
}
