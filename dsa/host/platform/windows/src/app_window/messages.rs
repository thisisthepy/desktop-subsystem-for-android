//! What the window host's message boxes say when opening an app, a file or Android's files,
//! installing an APK, making a desktop shortcut or sending a dropped file ends, in the user's
//! display language ([`crate::ui_language`]). The reason after each sentence comes from the
//! service or Windows as it is.

use dsa_host_core::ui_language::UiLanguage;

use crate::ui_language::fill;

/// The sentences of the message boxes in one language, as written, with their placeholders:
/// `{name}` an app or APK, `{package}` " (its package)" or nothing, `{file}` a file's name,
/// `{why}` the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Messages {
    pub installed: &'static str,
    pub not_installed: &'static str,
    pub file_not_opened: &'static str,
    pub files_not_opened: &'static str,
    pub shortcut_not_made: &'static str,
    pub app_not_opened: &'static str,
    pub drop_not_sent: &'static str,
}

/// The sentences in `language`.
pub fn messages(language: UiLanguage) -> Messages {
    let [installed, not_installed, file_not_opened, files_not_opened, shortcut_not_made, app_not_opened, drop_not_sent] =
        match language {
            UiLanguage::English => [
                "{name}{package} is installed. Open it from the Start menu.",
                "{name} was not installed. {why}.",
                "{file} could not be opened in Android. {why}.",
                "Android's files could not be opened. {why}.",
                "A desktop shortcut to {name} could not be made. {why}.",
                "{name} could not be opened. {why}.",
                "A dropped file could not be sent to the app. {why}.",
            ],
            UiLanguage::Korean => [
                "{name}{package}이(가) 설치되었습니다. 시작 메뉴에서 여십시오.",
                "{name}을(를) 설치하지 못했습니다. {why}.",
                "{file}을(를) Android에서 열지 못했습니다. {why}.",
                "Android의 파일을 열지 못했습니다. {why}.",
                "{name}의 바탕 화면 바로 가기를 만들지 못했습니다. {why}.",
                "{name}을(를) 열지 못했습니다. {why}.",
                "끌어 놓은 파일을 앱에 보내지 못했습니다. {why}.",
            ],
            UiLanguage::Japanese => [
                "{name}{package} がインストールされました。スタート メニューから開いてください。",
                "{name} はインストールされませんでした。{why}。",
                "{file} を Android で開けませんでした。{why}。",
                "Android のファイルを開けませんでした。{why}。",
                "{name} のデスクトップ ショートカットを作成できませんでした。{why}。",
                "{name} を開けませんでした。{why}。",
                "ドロップしたファイルをアプリに送信できませんでした。{why}。",
            ],
            UiLanguage::ChineseSimplified => [
                "已安装 {name}{package}。请从“开始”菜单中打开它。",
                "未安装 {name}。{why}。",
                "无法在 Android 中打开 {file}。{why}。",
                "无法打开 Android 的文件。{why}。",
                "无法创建 {name} 的桌面快捷方式。{why}。",
                "无法打开 {name}。{why}。",
                "无法将拖放的文件发送到应用。{why}。",
            ],
            UiLanguage::ChineseTraditional => [
                "已安裝 {name}{package}。請從 [開始] 功能表開啟。",
                "未安裝 {name}。{why}。",
                "無法在 Android 中開啟 {file}。{why}。",
                "無法開啟 Android 的檔案。{why}。",
                "無法建立 {name} 的桌面捷徑。{why}。",
                "無法開啟 {name}。{why}。",
                "無法將拖放的檔案傳送到應用程式。{why}。",
            ],
            UiLanguage::Spanish => [
                "{name}{package} está instalado. Ábralo desde el menú Inicio.",
                "{name} no se instaló. {why}.",
                "No se pudo abrir {file} en Android. {why}.",
                "No se pudieron abrir los archivos de Android. {why}.",
                "No se pudo crear un acceso directo a {name} en el escritorio. {why}.",
                "No se pudo abrir {name}. {why}.",
                "No se pudo enviar a la aplicación un archivo arrastrado. {why}.",
            ],
            UiLanguage::French => [
                "{name}{package} est installé. Ouvrez-le depuis le menu Démarrer.",
                "{name} n'a pas été installé. {why}.",
                "Impossible d'ouvrir {file} dans Android. {why}.",
                "Impossible d'ouvrir les fichiers d'Android. {why}.",
                "Impossible de créer un raccourci vers {name} sur le Bureau. {why}.",
                "Impossible d'ouvrir {name}. {why}.",
                "Impossible d'envoyer un fichier déposé à l'application. {why}.",
            ],
            UiLanguage::German => [
                "{name}{package} ist installiert. Öffnen Sie es über das Startmenü.",
                "{name} wurde nicht installiert. {why}.",
                "{file} konnte nicht in Android geöffnet werden. {why}.",
                "Die Dateien von Android konnten nicht geöffnet werden. {why}.",
                "Eine Desktopverknüpfung zu {name} konnte nicht erstellt werden. {why}.",
                "{name} konnte nicht geöffnet werden. {why}.",
                "Eine abgelegte Datei konnte nicht an die App gesendet werden. {why}.",
            ],
            UiLanguage::PortugueseBrazil => [
                "{name}{package} está instalado. Abra-o pelo menu Iniciar.",
                "{name} não foi instalado. {why}.",
                "Não foi possível abrir {file} no Android. {why}.",
                "Não foi possível abrir os arquivos do Android. {why}.",
                "Não foi possível criar um atalho para {name} na área de trabalho. {why}.",
                "Não foi possível abrir {name}. {why}.",
                "Não foi possível enviar ao app um arquivo arrastado. {why}.",
            ],
            UiLanguage::Russian => [
                "{name}{package}: установлено. Откройте его из меню \u{ab}Пуск\u{bb}.",
                "{name}: не установлено. {why}.",
                "Не удалось открыть {file} в Android. {why}.",
                "Не удалось открыть файлы Android. {why}.",
                "Не удалось создать ярлык {name} на рабочем столе. {why}.",
                "Не удалось открыть {name}. {why}.",
                "Не удалось отправить перетащенный файл в приложение. {why}.",
            ],
            UiLanguage::Italian => [
                "{name}{package} è installato. Aprilo dal menu Start.",
                "{name} non è stato installato. {why}.",
                "Impossibile aprire {file} in Android. {why}.",
                "Impossibile aprire i file di Android. {why}.",
                "Impossibile creare un collegamento a {name} sul desktop. {why}.",
                "Impossibile aprire {name}. {why}.",
                "Impossibile inviare all'app un file trascinato. {why}.",
            ],
            UiLanguage::Vietnamese => [
                "Đã cài đặt {name}{package}. Hãy mở từ menu Start.",
                "Chưa cài đặt được {name}. {why}.",
                "Không thể mở {file} trong Android. {why}.",
                "Không thể mở tệp của Android. {why}.",
                "Không thể tạo lối tắt trên màn hình nền cho {name}. {why}.",
                "Không thể mở {name}. {why}.",
                "Không thể gửi tệp được thả vào ứng dụng. {why}.",
            ],
            UiLanguage::Indonesian => [
                "{name}{package} sudah terinstal. Buka dari menu Start.",
                "{name} tidak terinstal. {why}.",
                "{file} tidak dapat dibuka di Android. {why}.",
                "File Android tidak dapat dibuka. {why}.",
                "Pintasan desktop ke {name} tidak dapat dibuat. {why}.",
                "{name} tidak dapat dibuka. {why}.",
                "File yang dilepas tidak dapat dikirim ke aplikasi. {why}.",
            ],
            UiLanguage::Turkish => [
                "{name}{package} yüklendi. Başlat menüsünden açın.",
                "{name} yüklenmedi. {why}.",
                "{file}, Android'de açılamadı. {why}.",
                "Android'in dosyaları açılamadı. {why}.",
                "{name} için masaüstü kısayolu oluşturulamadı. {why}.",
                "{name} açılamadı. {why}.",
                "Bırakılan bir dosya uygulamaya gönderilemedi. {why}.",
            ],
        };
    Messages {
        installed,
        not_installed,
        file_not_opened,
        files_not_opened,
        shortcut_not_made,
        app_not_opened,
        drop_not_sent,
    }
}

/// What the window shown while an app waits for Android says of itself to a screen reader,
/// in `language`. It shows the DSA logo instead of these words.
pub fn starting(language: UiLanguage) -> &'static str {
    dsa_host_core::ui_text::caption::starting_android(language)
}

/// `template` with `{name}`, `{package}`, `{file}` and `{why}` filled in.
pub fn say(template: &str, values: &[(&str, &str)]) -> String {
    fill(template, values)
}

/// The message in the user's display language that `name` could not be opened.
pub fn app_not_opened(name: &str, why: &str) -> String {
    let words = messages(crate::ui_language::user_language());
    say(words.app_not_opened, &[("name", name), ("why", why)])
}

/// The message in the user's display language that `file` could not be opened in Android.
pub fn file_not_opened(file: &str, why: &str) -> String {
    let words = messages(crate::ui_language::user_language());
    say(words.file_not_opened, &[("file", file), ("why", why)])
}

/// The message in the user's display language that Android's files could not be opened.
pub fn files_not_opened(why: &str) -> String {
    let words = messages(crate::ui_language::user_language());
    say(words.files_not_opened, &[("why", why)])
}

/// The message in the user's display language that no desktop shortcut to `name` was made.
pub fn shortcut_not_made(name: &str, why: &str) -> String {
    let words = messages(crate::ui_language::user_language());
    say(words.shortcut_not_made, &[("name", name), ("why", why)])
}

/// The message in the user's display language that a dropped file did not reach the app.
pub fn drop_not_sent(why: &str) -> String {
    let words = messages(crate::ui_language::user_language());
    say(words.drop_not_sent, &[("why", why)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_language::assert_translation;

    fn all(words: &Messages) -> [(&'static str, &'static str); 7] {
        [
            ("installed", words.installed),
            ("not installed", words.not_installed),
            ("file not opened", words.file_not_opened),
            ("files not opened", words.files_not_opened),
            ("shortcut not made", words.shortcut_not_made),
            ("app not opened", words.app_not_opened),
            ("drop not sent", words.drop_not_sent),
        ]
    }

    #[test]
    fn fr17_the_window_hosts_messages_speak_every_language() {
        let english = messages(UiLanguage::English);
        for language in UiLanguage::ALL {
            let words = messages(language);
            for ((what, en), (_, text)) in all(&english).into_iter().zip(all(&words)) {
                assert_translation(language, what, en, text);
                if language != UiLanguage::English {
                    assert_ne!(text, en, "{what} has no translation in {language:?}");
                }
            }
            for (_, template) in all(&words) {
                let filled = say(
                    template,
                    &[
                        ("name", "Notes"),
                        ("package", " (com.example.notes)"),
                        ("file", "notes.pdf"),
                        ("why", "the service is not running"),
                    ],
                );
                assert!(!filled.contains('{'), "{language:?}: {filled}");
            }
            assert_translation(
                language,
                "Starting Android",
                starting(UiLanguage::English),
                starting(language),
            );
            assert!(starting(language).contains("Android"));
        }
    }
}
