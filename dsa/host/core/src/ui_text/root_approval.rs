//! The words of the administrator approval dialog that a host shows itself, when the system's
//! own prompt is not the one asking (Windows with User Account Control off): who asks, what it
//! runs and how long the person has, in each of DSA's interface languages.

use std::time::Duration;

use crate::root_approval::ApprovalRequest;
use crate::ui_language::UiLanguage;
use crate::ui_text::fill;

/// The words of the program's own dialog, which it shows when UAC is off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmText {
    pub caption: String,
    pub text: String,
}

/// The words of the program's own dialog in one language, as written: the name of an app
/// that has none, the caption (`{label}`), the text (`{who}` asks to run `{command}`), and the
/// sentence that says how long it has (`{time}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmWords {
    pub unnamed: &'static str,
    pub caption: &'static str,
    pub text: &'static str,
    pub time_limit: &'static str,
}

/// The program's own dialog's words in `language`.
pub fn confirm_words(language: UiLanguage) -> ConfirmWords {
    let [unnamed, caption, text, time_limit] = match language {
        UiLanguage::English => [
            "An Android app",
            "{label}: administrator approval",
            "{who}\n\nwants to run this as root in Android:\n\n{command}\n\nUser Account \
             Control is off on this computer, so Desktop Subsystem for Android asks you \
             itself. Allow it?",
            "If nobody answers within {time}, the request is refused.",
        ],
        UiLanguage::Korean => [
            "Android 앱",
            "{label}: 관리자 승인",
            "{who}\n\nAndroid에서 다음을 루트 권한으로 실행하려고 합니다.\n\n{command}\n\n\
             이 컴퓨터는 사용자 계정 컨트롤이 꺼져 있어서 Desktop Subsystem for Android가 \
             직접 묻습니다. 허용할까요?",
            "{time} 안에 답하지 않으면 요청은 거부됩니다.",
        ],
        UiLanguage::Japanese => [
            "Android アプリ",
            "{label}: 管理者の承認",
            "{who}\n\nが Android で次のコマンドを root として実行しようとしています:\n\n\
             {command}\n\nこのコンピューターではユーザー アカウント制御がオフになっているため、\
             Desktop Subsystem for Android が直接確認しています。許可しますか?",
            "{time}以内に応答がない場合、要求は拒否されます。",
        ],
        UiLanguage::ChineseSimplified => [
            "一个 Android 应用",
            "{label}: 管理员批准",
            "{who}\n\n想要在 Android 中以 root 身份运行以下内容:\n\n{command}\n\n此电脑的\
             用户帐户控制已关闭，因此由 Desktop Subsystem for Android 直接询问你。是否允许?",
            "如果在 {time}内无人应答，请求将被拒绝。",
        ],
        UiLanguage::ChineseTraditional => [
            "一個 Android 應用程式",
            "{label}: 系統管理員核准",
            "{who}\n\n想要在 Android 中以 root 身分執行以下內容:\n\n{command}\n\n這部電腦的\
             使用者帳戶控制已關閉，因此由 Desktop Subsystem for Android 直接詢問您。要允許嗎?",
            "如果在 {time}內沒有人回應，要求將會遭到拒絕。",
        ],
        UiLanguage::Spanish => [
            "Una aplicación de Android",
            "{label}: aprobación del administrador",
            "{who}\n\nquiere ejecutar esto como root en Android:\n\n{command}\n\nEl Control de \
             cuentas de usuario está desactivado en este equipo, así que Desktop Subsystem for \
             Android se lo pregunta directamente. ¿Desea permitirlo?",
            "Si nadie responde en {time}, se rechazará la solicitud.",
        ],
        UiLanguage::French => [
            "Une application Android",
            "{label}\u{a0}: approbation de l'administrateur",
            "{who}\n\nveut exécuter ceci en tant que root dans Android\u{a0}:\n\n{command}\n\n\
             Le Contrôle de compte d'utilisateur est désactivé sur cet ordinateur\u{a0}: \
             Desktop Subsystem for Android vous le demande donc directement. Voulez-vous \
             l'autoriser\u{a0}?",
            "Si personne ne répond dans un délai de {time}, la demande sera refusée.",
        ],
        UiLanguage::German => [
            "Eine Android-App",
            "{label}: Administratorgenehmigung",
            "{who}\n\nmöchte Folgendes in Android als root ausführen:\n\n{command}\n\nDie \
             Benutzerkontensteuerung ist auf diesem Computer deaktiviert, daher fragt Desktop \
             Subsystem for Android Sie selbst. Möchten Sie es zulassen?",
            "Wenn innerhalb von {time} niemand antwortet, wird die Anfrage abgelehnt.",
        ],
        UiLanguage::PortugueseBrazil => [
            "Um app Android",
            "{label}: aprovação do administrador",
            "{who}\n\nquer executar isto como root no Android:\n\n{command}\n\nO Controle de \
             Conta de Usuário está desativado neste computador, então o Desktop Subsystem for \
             Android pergunta a você diretamente. Permitir?",
            "Se ninguém responder em {time}, a solicitação será recusada.",
        ],
        UiLanguage::Russian => [
            "Приложение Android",
            "{label}: подтверждение администратора",
            "{who}\n\nхочет выполнить в Android от имени root:\n\n{command}\n\nНа этом \
             компьютере отключен контроль учетных записей, поэтому Desktop Subsystem for \
             Android спрашивает вас сам. Разрешить?",
            "Если никто не ответит в течение {time}, запрос будет отклонен.",
        ],
        UiLanguage::Italian => [
            "Un'app Android",
            "{label}: approvazione dell'amministratore",
            "{who}\n\nvuole eseguire questo comando come root in Android:\n\n{command}\n\nIl \
             Controllo dell'account utente è disattivato su questo computer, quindi Desktop \
             Subsystem for Android te lo chiede direttamente. Consentire?",
            "Se nessuno risponde entro {time}, la richiesta verrà rifiutata.",
        ],
        UiLanguage::Vietnamese => [
            "Một ứng dụng Android",
            "{label}: phê duyệt của quản trị viên",
            "{who}\n\nmuốn chạy lệnh sau với quyền root trong Android:\n\n{command}\n\nKiểm \
             soát Tài khoản Người dùng đang tắt trên máy tính này, nên Desktop Subsystem for \
             Android trực tiếp hỏi bạn. Cho phép?",
            "Nếu không ai trả lời trong vòng {time}, yêu cầu sẽ bị từ chối.",
        ],
        UiLanguage::Indonesian => [
            "Aplikasi Android",
            "{label}: persetujuan administrator",
            "{who}\n\ningin menjalankan ini sebagai root di Android:\n\n{command}\n\nKontrol \
             Akun Pengguna nonaktif di komputer ini, jadi Desktop Subsystem for Android \
             menanyakannya langsung kepada Anda. Izinkan?",
            "Jika tidak ada yang menjawab dalam {time}, permintaan akan ditolak.",
        ],
        UiLanguage::Turkish => [
            "Bir Android uygulaması",
            "{label}: yönetici onayı",
            "{who}\n\nAndroid'de şunu root olarak çalıştırmak istiyor:\n\n{command}\n\nBu \
             bilgisayarda Kullanıcı Hesabı Denetimi kapalı olduğundan Desktop Subsystem for \
             Android size doğrudan soruyor. İzin verilsin mi?",
            "{time} içinde kimse yanıt vermezse istek reddedilir.",
        ],
    };
    ConfirmWords {
        unnamed,
        caption,
        text,
        time_limit,
    }
}

/// The program's own dialog for `label` (and the `detail` line under it) wanting to run
/// `command`, in `language`.
pub fn confirm_text(label: &str, detail: &str, command: &str, language: UiLanguage) -> ConfirmText {
    let words = confirm_words(language);
    let label = match label.trim() {
        "" => words.unnamed,
        label => label,
    };
    let who = if detail.trim().is_empty() {
        label.to_string()
    } else {
        format!("{label}\n{}", detail.trim())
    };
    ConfirmText {
        caption: fill(words.caption, &[("label", label)]),
        text: fill(words.text, &[("who", &who), ("command", command)]),
    }
}

/// The program's own dialog for `words`: [`confirm_text`], then how long it has to be answered
/// when the service said, as one sentence a screen reader reads with the rest. The time is
/// written once rather than counted down, so nothing is read out every second, and the person
/// still knows how long they have.
pub fn confirm_text_for(words: &AskedWords) -> ConfirmText {
    let mut shown = confirm_text(&words.label, &words.detail, &words.command, words.language);
    if let Some(limit) = time_limit_text(words.seconds, words.language) {
        shown.text.push_str("\n\n");
        shown.text.push_str(&limit);
    }
    shown
}

/// `seconds` in words in `language`, in whole minutes when it is a whole number of them, as
/// the time limit's sentence uses it.
pub fn time_words(seconds: u64, language: UiLanguage) -> String {
    let whole_minutes = seconds % 60 == 0;
    let n = if whole_minutes { seconds / 60 } else { seconds };
    let (one, many) = match (language, whole_minutes) {
        (UiLanguage::English, true) => ("1 minute", "{n} minutes"),
        (UiLanguage::English, false) => ("1 second", "{n} seconds"),
        (UiLanguage::Korean, true) => ("1분", "{n}분"),
        (UiLanguage::Korean, false) => ("1초", "{n}초"),
        (UiLanguage::Japanese, true) => ("1 分", "{n} 分"),
        (UiLanguage::Japanese, false) => ("1 秒", "{n} 秒"),
        (UiLanguage::ChineseSimplified | UiLanguage::ChineseTraditional, true) => {
            if language == UiLanguage::ChineseSimplified {
                ("1 分钟", "{n} 分钟")
            } else {
                ("1 分鐘", "{n} 分鐘")
            }
        }
        (UiLanguage::ChineseSimplified | UiLanguage::ChineseTraditional, false) => {
            ("1 秒", "{n} 秒")
        }
        (UiLanguage::Spanish, true) => ("1 minuto", "{n} minutos"),
        (UiLanguage::Spanish, false) => ("1 segundo", "{n} segundos"),
        (UiLanguage::French, true) => ("1 minute", "{n} minutes"),
        (UiLanguage::French, false) => ("1 seconde", "{n} secondes"),
        (UiLanguage::German, true) => ("1 Minute", "{n} Minuten"),
        (UiLanguage::German, false) => ("1 Sekunde", "{n} Sekunden"),
        (UiLanguage::PortugueseBrazil, true) => ("1 minuto", "{n} minutos"),
        (UiLanguage::PortugueseBrazil, false) => ("1 segundo", "{n} segundos"),
        // "в течение" takes the genitive: 1, 21, 31 ... минуты; every other number минут.
        (UiLanguage::Russian, true) => ("{n} минуты", "{n} минут"),
        (UiLanguage::Russian, false) => ("{n} секунды", "{n} секунд"),
        (UiLanguage::Italian, true) => ("1 minuto", "{n} minuti"),
        (UiLanguage::Italian, false) => ("1 secondo", "{n} secondi"),
        (UiLanguage::Vietnamese, true) => ("1 phút", "{n} phút"),
        (UiLanguage::Vietnamese, false) => ("1 giây", "{n} giây"),
        (UiLanguage::Indonesian, true) => ("1 menit", "{n} menit"),
        (UiLanguage::Indonesian, false) => ("1 detik", "{n} detik"),
        (UiLanguage::Turkish, true) => ("1 dakika", "{n} dakika"),
        (UiLanguage::Turkish, false) => ("1 saniye", "{n} saniye"),
    };
    let singular = if language == UiLanguage::Russian {
        n % 10 == 1 && n % 100 != 11
    } else {
        n == 1
    };
    let number = n.to_string();
    fill(if singular { one } else { many }, &[("n", &number)])
}

/// The sentence that says the dialog is refused after `seconds`, in `language`; `None` for no
/// time given.
pub fn time_limit_text(seconds: u64, language: UiLanguage) -> Option<String> {
    if seconds == 0 {
        return None;
    }
    let time = time_words(seconds, language);
    Some(fill(confirm_words(language).time_limit, &[("time", &time)]))
}

/// What the program's own dialog shows, when UAC is off: who asks by name, the line under it,
/// the command, the language it is in, and how many seconds it has to be answered (0 when
/// the service did not say).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AskedWords {
    pub label: String,
    pub detail: String,
    pub command: String,
    pub language: UiLanguage,
    pub seconds: u64,
}

impl AskedWords {
    /// The words for `request`, in the user's language.
    pub fn of(request: &ApprovalRequest) -> AskedWords {
        let language = request.language();
        AskedWords {
            label: request.name_in(language),
            detail: request.detail_in(language),
            command: request.command_text_in(language),
            language,
            seconds: 0,
        }
    }

    /// The same words, saying the dialog has `timeout` to be answered, in whole seconds.
    pub fn within(mut self, timeout: Duration) -> AskedWords {
        self.seconds = timeout.as_secs();
        self
    }
}
