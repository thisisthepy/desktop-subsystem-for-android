//! The words of the items DSA adds to a task window's menu, in each of the languages the
//! interface is shown in.
//!
//! Every host offers the same items (Full screen, New window, Screenshot, Aspect ratio, Open
//! by default, App info, More window options) and a screenshot's file name, whatever the
//! desktop. The words are written once here, plainly. What a desktop adds on top is its own
//! convention and stays with its host: an access key marked in the label, an ellipsis after
//! an item that asks for more, or a capital at the start of each word.
//!
//! The words of the items a desktop names itself (Restore, Move, Size, Minimize, Maximize,
//! Close, Zoom) are not here.

use crate::ui_language::UiLanguage;

/// The words of DSA's own window menu items in one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DsaMenuText {
    pub full_screen: &'static str,
    pub new_window: &'static str,
    pub screenshot: &'static str,
    pub aspect_ratio: &'static str,
    pub open_by_default: &'static str,
    pub app_info: &'static str,
    /// The item that opens the window's other options.
    pub more_options: &'static str,
    /// What a screenshot's file name starts with, as the desktop's own screenshot tool names
    /// its files. A name any file system accepts.
    pub screenshot_file: &'static str,
}

/// The words of DSA's own window menu items in `language`.
pub fn dsa_menu_text(language: UiLanguage) -> DsaMenuText {
    match language {
        UiLanguage::English => DsaMenuText {
            full_screen: "Full screen",
            new_window: "New window",
            screenshot: "Screenshot",
            aspect_ratio: "Aspect ratio",
            open_by_default: "Open by default",
            app_info: "App info",
            more_options: "More window options",
            screenshot_file: "Screenshot",
        },
        UiLanguage::Korean => DsaMenuText {
            full_screen: "전체 화면",
            new_window: "새 창",
            screenshot: "스크린샷",
            aspect_ratio: "화면 비율",
            open_by_default: "링크 열기 설정",
            app_info: "앱 정보",
            more_options: "창 메뉴 더 보기",
            screenshot_file: "스크린샷",
        },
        UiLanguage::Japanese => DsaMenuText {
            full_screen: "全画面表示",
            new_window: "新しいウィンドウ",
            screenshot: "スクリーンショット",
            aspect_ratio: "縦横比",
            open_by_default: "デフォルトで開く",
            app_info: "アプリ情報",
            more_options: "その他のウィンドウ オプション",
            screenshot_file: "スクリーンショット",
        },
        UiLanguage::ChineseSimplified => DsaMenuText {
            full_screen: "全屏",
            new_window: "新建窗口",
            screenshot: "屏幕截图",
            aspect_ratio: "宽高比",
            open_by_default: "默认打开",
            app_info: "应用信息",
            more_options: "更多窗口选项",
            screenshot_file: "屏幕截图",
        },
        UiLanguage::ChineseTraditional => DsaMenuText {
            full_screen: "全螢幕",
            new_window: "新增視窗",
            screenshot: "螢幕擷取畫面",
            aspect_ratio: "長寬比",
            open_by_default: "預設開啟",
            app_info: "應用程式資訊",
            more_options: "更多視窗選項",
            screenshot_file: "螢幕擷取畫面",
        },
        UiLanguage::Spanish => DsaMenuText {
            full_screen: "Pantalla completa",
            new_window: "Nueva ventana",
            screenshot: "Captura de pantalla",
            aspect_ratio: "Relación de aspecto",
            open_by_default: "Abrir de forma predeterminada",
            app_info: "Información de la aplicación",
            more_options: "Más opciones de ventana",
            screenshot_file: "Captura de pantalla",
        },
        UiLanguage::French => DsaMenuText {
            full_screen: "Plein écran",
            new_window: "Nouvelle fenêtre",
            screenshot: "Capture d'écran",
            aspect_ratio: "Rapport hauteur/largeur",
            open_by_default: "Ouvrir par défaut",
            app_info: "Infos sur l'appli",
            more_options: "Plus d'options de fenêtre",
            screenshot_file: "Capture d'écran",
        },
        UiLanguage::German => DsaMenuText {
            full_screen: "Vollbild",
            new_window: "Neues Fenster",
            screenshot: "Screenshot",
            aspect_ratio: "Seitenverhältnis",
            open_by_default: "Standardmäßig öffnen",
            app_info: "App-Info",
            more_options: "Weitere Fensteroptionen",
            screenshot_file: "Screenshot",
        },
        UiLanguage::PortugueseBrazil => DsaMenuText {
            full_screen: "Tela inteira",
            new_window: "Nova janela",
            screenshot: "Captura de tela",
            aspect_ratio: "Proporção",
            open_by_default: "Abrir por padrão",
            app_info: "Informações do app",
            more_options: "Mais opções de janela",
            screenshot_file: "Captura de tela",
        },
        UiLanguage::Russian => DsaMenuText {
            full_screen: "Во весь экран",
            new_window: "Новое окно",
            screenshot: "Снимок экрана",
            aspect_ratio: "Соотношение сторон",
            open_by_default: "Открывать по умолчанию",
            app_info: "О приложении",
            more_options: "Другие параметры окна",
            screenshot_file: "Снимок экрана",
        },
        UiLanguage::Italian => DsaMenuText {
            full_screen: "Schermo intero",
            new_window: "Nuova finestra",
            screenshot: "Screenshot",
            aspect_ratio: "Proporzioni",
            open_by_default: "Apri per impostazione predefinita",
            app_info: "Informazioni app",
            more_options: "Altre opzioni finestra",
            screenshot_file: "Screenshot",
        },
        UiLanguage::Vietnamese => DsaMenuText {
            full_screen: "Toàn màn hình",
            new_window: "Cửa sổ mới",
            screenshot: "Ảnh chụp màn hình",
            aspect_ratio: "Tỷ lệ khung hình",
            open_by_default: "Mở theo mặc định",
            app_info: "Thông tin ứng dụng",
            more_options: "Tùy chọn cửa sổ khác",
            screenshot_file: "Ảnh chụp màn hình",
        },
        UiLanguage::Indonesian => DsaMenuText {
            full_screen: "Layar penuh",
            new_window: "Jendela baru",
            screenshot: "Tangkapan layar",
            aspect_ratio: "Rasio aspek",
            open_by_default: "Buka secara default",
            app_info: "Info aplikasi",
            more_options: "Opsi jendela lainnya",
            screenshot_file: "Tangkapan layar",
        },
        UiLanguage::Turkish => DsaMenuText {
            full_screen: "Tam ekran",
            new_window: "Yeni pencere",
            screenshot: "Ekran görüntüsü",
            aspect_ratio: "En boy oranı",
            open_by_default: "Varsayılan olarak aç",
            app_info: "Uygulama bilgisi",
            more_options: "Diğer pencere seçenekleri",
            screenshot_file: "Ekran görüntüsü",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(text: DsaMenuText) -> [&'static str; 8] {
        [
            text.full_screen,
            text.new_window,
            text.screenshot,
            text.aspect_ratio,
            text.open_by_default,
            text.app_info,
            text.more_options,
            text.screenshot_file,
        ]
    }

    #[test]
    fn fr22_dsa_menu_text_is_translated_in_every_language() {
        let english = fields(dsa_menu_text(UiLanguage::English));
        for language in UiLanguage::ALL {
            let words = fields(dsa_menu_text(language));
            for (n, (word, en)) in words.iter().zip(english).enumerate() {
                let tag = language.tag();
                assert!(!word.trim().is_empty(), "item {n} is empty in {tag}");
                assert!(
                    !word.contains(char::from_u32(0x2014).unwrap()),
                    "item {n} has an em dash in {tag}"
                );
                // A mark for an access key or a shortcut belongs to the host, not here.
                assert!(
                    !word.contains('&'),
                    "item {n} carries an access key in {tag}"
                );
                if language != UiLanguage::English && !["Screenshot"].contains(&en) {
                    // Some languages say the English word as it stands (Italian
                    // "Screenshot"); the items that are phrases are always translated.
                    assert!(
                        *word != en || en.split(' ').count() == 1,
                        "item {n} is still English in {tag}"
                    );
                }
            }
            // A file name every file system accepts.
            assert!(!dsa_menu_text(language)
                .screenshot_file
                .contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']));
        }
    }

    #[test]
    fn fr22_windows_wording_of_the_dsa_items_is_unchanged() {
        let english = dsa_menu_text(UiLanguage::English);
        assert_eq!(english.new_window, "New window");
        assert_eq!(english.open_by_default, "Open by default");
        let korean = dsa_menu_text(UiLanguage::Korean);
        assert_eq!(korean.app_info, "앱 정보");
        assert_eq!(korean.screenshot_file, "스크린샷");
        assert_eq!(korean.open_by_default, "링크 열기 설정");
        assert_eq!(
            dsa_menu_text(UiLanguage::German).screenshot_file,
            "Screenshot"
        );
        assert_eq!(
            dsa_menu_text(UiLanguage::French).aspect_ratio,
            "Rapport hauteur/largeur"
        );
    }
}
