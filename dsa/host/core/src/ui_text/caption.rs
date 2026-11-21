//! What the title bar and the loading state of a task window say to a screen reader, in each
//! of the languages DSA's own interface is shown in.
//!
//! The names follow the words the desktops use for their own caption buttons, so a user
//! hears what they hear everywhere else. The hosts read them for their accessibility
//! trees; none of them keeps a table of its own.

use crate::compositor::caption::CaptionKind;
use crate::ui_language::UiLanguage;

/// What the window says of itself (its item status) while its app opens: until the first
/// frame it shows only the splash, and a screen reader would otherwise read an empty window
/// with no hint that something is coming. `None` once the app shows.
pub fn loading_status(language: UiLanguage, loading: bool) -> Option<&'static str> {
    loading.then_some(match language {
        UiLanguage::English => "Opening the app",
        UiLanguage::Korean => "앱을 여는 중",
        UiLanguage::Japanese => "アプリを開いています",
        UiLanguage::ChineseSimplified => "正在打开应用",
        UiLanguage::ChineseTraditional => "正在開啟應用程式",
        UiLanguage::Spanish => "Abriendo la aplicación",
        UiLanguage::French => "Ouverture de l'application",
        UiLanguage::German => "App wird geöffnet",
        UiLanguage::PortugueseBrazil => "Abrindo o app",
        UiLanguage::Russian => "Открытие приложения",
        UiLanguage::Italian => "Apertura dell'app",
        UiLanguage::Vietnamese => "Đang mở ứng dụng",
        UiLanguage::Indonesian => "Membuka aplikasi",
        UiLanguage::Turkish => "Uygulama açılıyor",
    })
}

/// What the window shown while an app waits for Android to start says of itself (its
/// accessible name), in `language`. The window shows the DSA logo and no words, so a screen
/// reader still hears why the app has not opened yet.
pub fn starting_android(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::English => "Starting Android\u{2026}",
        UiLanguage::Korean => "Android를 시작하는 중\u{2026}",
        UiLanguage::Japanese => "Android を起動しています\u{2026}",
        UiLanguage::ChineseSimplified => "正在启动 Android\u{2026}",
        UiLanguage::ChineseTraditional => "正在啟動 Android\u{2026}",
        UiLanguage::Spanish => "Iniciando Android\u{2026}",
        UiLanguage::French => "Démarrage d'Android\u{2026}",
        UiLanguage::German => "Android wird gestartet\u{2026}",
        UiLanguage::PortugueseBrazil => "Iniciando o Android\u{2026}",
        UiLanguage::Russian => "Запуск Android\u{2026}",
        UiLanguage::Italian => "Avvio di Android\u{2026}",
        UiLanguage::Vietnamese => "Đang khởi động Android\u{2026}",
        UiLanguage::Indonesian => "Memulai Android\u{2026}",
        UiLanguage::Turkish => "Android başlatılıyor\u{2026}",
    }
}

/// The title bar buttons' names in `language`, as Windows names its own caption buttons:
/// Back, System menu, Minimize, Maximize, Restore and Close.
pub fn caption_names(language: UiLanguage) -> [&'static str; 6] {
    match language {
        UiLanguage::English => [
            "Back",
            "System menu",
            "Minimize",
            "Maximize",
            "Restore",
            "Close",
        ],
        UiLanguage::Korean => [
            "뒤로",
            "시스템 메뉴",
            "최소화",
            "최대화",
            "이전 크기로",
            "닫기",
        ],
        UiLanguage::Japanese => [
            "戻る",
            "システム メニュー",
            "最小化",
            "最大化",
            "元に戻す (縮小)",
            "閉じる",
        ],
        UiLanguage::ChineseSimplified => {
            ["返回", "系统菜单", "最小化", "最大化", "向下还原", "关闭"]
        }
        UiLanguage::ChineseTraditional => {
            ["返回", "系統功能表", "最小化", "最大化", "還原", "關閉"]
        }
        UiLanguage::Spanish => [
            "Atrás",
            "Menú del sistema",
            "Minimizar",
            "Maximizar",
            "Restaurar",
            "Cerrar",
        ],
        UiLanguage::French => [
            "Retour",
            "Menu système",
            "Réduire",
            "Agrandir",
            "Restaurer",
            "Fermer",
        ],
        UiLanguage::German => [
            "Zurück",
            "Systemmenü",
            "Minimieren",
            "Maximieren",
            "Wiederherstellen",
            "Schließen",
        ],
        UiLanguage::PortugueseBrazil => [
            "Voltar",
            "Menu do sistema",
            "Minimizar",
            "Maximizar",
            "Restaurar",
            "Fechar",
        ],
        UiLanguage::Russian => [
            "Назад",
            "Системное меню",
            "Свернуть",
            "Развернуть",
            "Восстановить",
            "Закрыть",
        ],
        UiLanguage::Italian => [
            "Indietro",
            "Menu di sistema",
            "Riduci a icona",
            "Ingrandisci",
            "Ripristina",
            "Chiudi",
        ],
        UiLanguage::Vietnamese => [
            "Quay lại",
            "Menu hệ thống",
            "Thu nhỏ",
            "Phóng to",
            "Khôi phục",
            "Đóng",
        ],
        UiLanguage::Indonesian => [
            "Kembali",
            "Menu sistem",
            "Minimalkan",
            "Maksimalkan",
            "Pulihkan",
            "Tutup",
        ],
        UiLanguage::Turkish => [
            "Geri",
            "Sistem menüsü",
            "Simge durumuna küçült",
            "Ekranı kapla",
            "Önceki boyut",
            "Kapat",
        ],
    }
}

/// A title bar button's name in `language`. The maximize button is named Restore while the
/// window is `maximized` (or fills its monitor), as the glyph it shows then says.
pub fn caption_name(kind: CaptionKind, maximized: bool, language: UiLanguage) -> &'static str {
    let [back, system_menu, minimize, maximize, restore, close] = caption_names(language);
    match kind {
        CaptionKind::Back => back,
        CaptionKind::SystemMenu => system_menu,
        CaptionKind::Minimize => minimize,
        CaptionKind::Maximize if maximized => restore,
        CaptionKind::Maximize => maximize,
        CaptionKind::Close => close,
    }
}

/// The back button's name in `language`.
pub fn back_name(language: UiLanguage) -> &'static str {
    caption_name(CaptionKind::Back, false, language)
}

/// What the loading item says in `language`.
pub fn loading_name(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::English => "Loading",
        UiLanguage::Korean => "불러오는 중",
        UiLanguage::Japanese => "読み込み中",
        UiLanguage::ChineseSimplified => "正在加载",
        UiLanguage::ChineseTraditional => "正在載入",
        UiLanguage::Spanish => "Cargando",
        UiLanguage::French => "Chargement",
        UiLanguage::German => "Wird geladen",
        UiLanguage::PortugueseBrazil => "Carregando",
        UiLanguage::Russian => "Загрузка",
        UiLanguage::Italian => "Caricamento",
        UiLanguage::Vietnamese => "Đang tải",
        UiLanguage::Indonesian => "Memuat",
        UiLanguage::Turkish => "Yükleniyor",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [CaptionKind; 5] = [
        CaptionKind::Back,
        CaptionKind::SystemMenu,
        CaptionKind::Minimize,
        CaptionKind::Maximize,
        CaptionKind::Close,
    ];

    fn problem(language: UiLanguage, what: &str, english: &str, text: &str) -> Option<String> {
        let tag = language.tag();
        if text.trim().is_empty() {
            return Some(format!("{what} is empty in {tag}"));
        }
        if text.contains(char::from_u32(0x2014).unwrap()) {
            return Some(format!("{what} has an em dash in {tag}"));
        }
        if language != UiLanguage::English && text == english {
            return Some(format!("{what} is still English in {tag}"));
        }
        None
    }

    #[test]
    fn fr7_boot_splash_keeps_saying_android_is_starting_to_screen_readers() {
        for language in UiLanguage::ALL {
            let name = starting_android(language);
            assert!(name.contains("Android"), "{language:?}: {name}");
            if let Some(wrong) = problem(
                language,
                "the starting window's name",
                starting_android(UiLanguage::English),
                name,
            ) {
                panic!("{wrong}");
            }
        }
    }

    #[test]
    fn fr18_caption_buttons_are_named_in_every_ui_language() {
        for language in UiLanguage::ALL {
            for kind in KINDS {
                let english = caption_name(kind, false, UiLanguage::English);
                let name = caption_name(kind, false, language);
                if let Some(wrong) = problem(language, &format!("{kind:?}"), english, name) {
                    panic!("{wrong}");
                }
            }
            if let Some(wrong) = problem(
                language,
                "the loading status",
                loading_status(UiLanguage::English, true).unwrap(),
                loading_status(language, true).unwrap(),
            ) {
                panic!("{wrong}");
            }
            assert_eq!(loading_status(language, false), None);
            if let Some(wrong) = problem(
                language,
                "the loading item",
                loading_name(UiLanguage::English),
                loading_name(language),
            ) {
                panic!("{wrong}");
            }
        }
    }

    #[test]
    fn fr18_maximize_is_named_restore_while_maximized() {
        for language in UiLanguage::ALL {
            let maximize = caption_name(CaptionKind::Maximize, false, language);
            let restore = caption_name(CaptionKind::Maximize, true, language);
            assert_ne!(maximize, restore, "{language:?}");
            // The other buttons keep their names.
            for kind in [CaptionKind::Minimize, CaptionKind::Close, CaptionKind::Back] {
                assert_eq!(
                    caption_name(kind, true, language),
                    caption_name(kind, false, language)
                );
            }
        }
        assert_eq!(
            caption_name(CaptionKind::Maximize, true, UiLanguage::English),
            "Restore"
        );
        assert_eq!(
            caption_name(CaptionKind::Maximize, false, UiLanguage::English),
            "Maximize"
        );
    }

    #[test]
    fn fr18_windows_wording_of_the_buttons_is_unchanged() {
        assert_eq!(back_name(UiLanguage::Korean), "뒤로");
        assert_eq!(back_name(UiLanguage::English), "Back");
        assert_eq!(back_name(UiLanguage::Japanese), "戻る");
        assert_eq!(back_name(UiLanguage::German), "Zurück");
        assert_eq!(
            caption_name(CaptionKind::SystemMenu, false, UiLanguage::Korean),
            "시스템 메뉴"
        );
        assert_eq!(
            caption_name(CaptionKind::Maximize, true, UiLanguage::Korean),
            "이전 크기로"
        );
        assert_eq!(
            loading_status(UiLanguage::English, true),
            Some("Opening the app")
        );
        assert_eq!(loading_name(UiLanguage::Korean), "불러오는 중");
    }
}
