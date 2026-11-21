//! The menu bar of an app's window host: the app menu and the Window menu.
//!
//! A Mac app's window commands live in its Window menu, so that is where the items of an
//! Android desktop window's window menu go. After macOS's own items (Minimize, Zoom) and a
//! separator come New Window (only for an app that supports several windows), Screenshot,
//! Aspect Ratio, Open by Default and App Info, then another separator, Bring All to Front,
//! and the list of the app's windows, which AppKit keeps. The same items open from a
//! secondary click on the band of a window whose app draws its own title bar, where the
//! window has no title of macOS's own to click.
//!
//! Each Android item acts on the app's front window. The window host takes the screenshot
//! itself, since it has the window's frames; every other item goes to the window manager as
//! `FromHost::MenuChosen`.
//!
//! The words are in the first of the user's preferred languages DSA has a translation for,
//! else English ([`crate::ui_language`]).

use dsa_host_core::compositor::window_hosts::MenuAction;
use dsa_host_core::compositor::window_menu::{android_items as shared_items, AndroidItem};
use dsa_host_core::ui_language::UiLanguage;
use dsa_host_core::ui_text::window_menu::dsa_menu_text;

/// One entry of the Window menu, before the window list AppKit adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowMenuItem {
    Separator,
    Minimize,
    Zoom,
    NewWindow,
    Screenshot,
    AspectRatio,
    OpenByDefault,
    AppInfo,
    BringAllToFront,
}

/// The Window menu's entries, in order. New Window is left out for an app that does not
/// support several windows, since its second task would only bring the first to the front.
pub fn window_menu(new_window: bool) -> Vec<WindowMenuItem> {
    let mut items = vec![
        WindowMenuItem::Minimize,
        WindowMenuItem::Zoom,
        WindowMenuItem::Separator,
    ];
    items.extend(android_items(new_window));
    items.extend([WindowMenuItem::Separator, WindowMenuItem::BringAllToFront]);
    items
}

/// The Android desktop window's items alone, as the band's secondary click shows them after
/// Minimize and Zoom.
pub fn android_items(new_window: bool) -> Vec<WindowMenuItem> {
    shared_items(false, new_window)
        .into_iter()
        .filter_map(WindowMenuItem::of_android)
        .collect()
}

impl WindowMenuItem {
    /// The entry of an Android item; `None` for Full screen, which macOS has of its own (the
    /// green button and the View menu).
    fn of_android(item: AndroidItem) -> Option<WindowMenuItem> {
        match item {
            AndroidItem::FullScreen => None,
            AndroidItem::NewWindow => Some(WindowMenuItem::NewWindow),
            AndroidItem::Screenshot => Some(WindowMenuItem::Screenshot),
            AndroidItem::AspectRatio => Some(WindowMenuItem::AspectRatio),
            AndroidItem::OpenByDefault => Some(WindowMenuItem::OpenByDefault),
            AndroidItem::AppInfo => Some(WindowMenuItem::AppInfo),
        }
    }

    /// The Android item this entry is; `None` for macOS's own entries and the separator.
    pub fn android(self) -> Option<AndroidItem> {
        match self {
            WindowMenuItem::NewWindow => Some(AndroidItem::NewWindow),
            WindowMenuItem::Screenshot => Some(AndroidItem::Screenshot),
            WindowMenuItem::AspectRatio => Some(AndroidItem::AspectRatio),
            WindowMenuItem::OpenByDefault => Some(AndroidItem::OpenByDefault),
            WindowMenuItem::AppInfo => Some(AndroidItem::AppInfo),
            _ => None,
        }
    }
}

/// The menu item tag that names an Android entry when it is chosen; 0 for macOS's own entries
/// and the separator, which AppKit carries out itself.
pub fn tag(item: WindowMenuItem) -> isize {
    match item {
        WindowMenuItem::NewWindow => 1,
        WindowMenuItem::Screenshot => 2,
        WindowMenuItem::AspectRatio => 3,
        WindowMenuItem::OpenByDefault => 4,
        WindowMenuItem::AppInfo => 5,
        _ => 0,
    }
}

/// The Android entry a chosen menu item's tag names.
pub fn from_tag(tag: isize) -> Option<WindowMenuItem> {
    [
        WindowMenuItem::NewWindow,
        WindowMenuItem::Screenshot,
        WindowMenuItem::AspectRatio,
        WindowMenuItem::OpenByDefault,
        WindowMenuItem::AppInfo,
    ]
    .into_iter()
    .find(|item| self::tag(*item) == tag)
}

/// What the window manager does for an entry; `None` for Screenshot, which the window host
/// takes, and for macOS's own entries.
pub fn action(item: WindowMenuItem) -> Option<MenuAction> {
    item.android().and_then(AndroidItem::action)
}

/// The AppKit action macOS's own entries send down the responder chain; `None` for the
/// separator and the Android entries.
pub fn system_action(item: WindowMenuItem) -> Option<&'static str> {
    match item {
        WindowMenuItem::Minimize => Some("performMiniaturize:"),
        WindowMenuItem::Zoom => Some("performZoom:"),
        WindowMenuItem::BringAllToFront => Some("arrangeInFront:"),
        _ => None,
    }
}

/// The key that chooses an entry with Command, as every Mac app has it; empty for none.
pub fn key_equivalent(item: WindowMenuItem) -> &'static str {
    match item {
        WindowMenuItem::Minimize => "m",
        _ => "",
    }
}

/// The menu bar's words in one language. `{app}` in an entry stands for the app's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuText {
    pub window: &'static str,
    pub minimize: &'static str,
    pub zoom: &'static str,
    pub new_window: &'static str,
    pub screenshot: &'static str,
    pub aspect_ratio: &'static str,
    pub open_by_default: &'static str,
    pub app_info: &'static str,
    pub bring_all_to_front: &'static str,
    pub hide_app: &'static str,
    pub hide_others: &'static str,
    pub show_all: &'static str,
    pub quit_app: &'static str,
}

/// The words macOS says itself: the Window menu, its own items and the app menu.
struct OsWords {
    window: &'static str,
    minimize: &'static str,
    zoom: &'static str,
    bring_all_to_front: &'static str,
    hide_app: &'static str,
    hide_others: &'static str,
    show_all: &'static str,
    quit_app: &'static str,
}

/// The items where a Mac says DSA's word another way than the words every host shares
/// ([`dsa_menu_text`]): the capitals its menus use for each word, and the words macOS itself
/// picked for the same thing. In the order of [`UiLanguage::ALL`]: New window, Screenshot,
/// Aspect ratio, Open by default and App info. `None` takes the shared word.
const OWN_WORDS: [[Option<&str>; 5]; 14] = [
    [
        Some("New Window"),
        None,
        Some("Aspect Ratio"),
        Some("Open by Default"),
        Some("App Info"),
    ], // English
    [None, None, None, None, None],                          // Korean
    [Some("新規ウインドウ"), None, None, None, None],        // Japanese
    [None, Some("截屏"), None, None, None],                  // ChineseSimplified
    [None, Some("截圖"), None, None, Some("App 資訊")],      // ChineseTraditional
    [None, None, None, None, Some("Información de la app")], // Spanish
    [
        None,
        Some("Capture d\u{2019}écran"),
        Some("Format d\u{2019}image"),
        None,
        Some("Infos sur l\u{2019}app"),
    ], // French
    [None, Some("Bildschirmfoto"), None, None, None],        // German
    [
        Some("Nova Janela"),
        Some("Captura de Tela"),
        None,
        Some("Abrir por Padrão"),
        Some("Informações do App"),
    ], // PortugueseBrazil
    [None, None, None, None, None],                          // Russian
    [
        None,
        Some("Istantanea schermo"),
        None,
        None,
        Some("Info app"),
    ], // Italian
    [None, None, None, None, None],                          // Vietnamese
    [
        Some("Jendela Baru"),
        Some("Jepretan Layar"),
        Some("Rasio Aspek"),
        Some("Buka secara Default"),
        Some("Info App"),
    ], // Indonesian
    [
        Some("Yeni Pencere"),
        Some("Ekran Görüntüsü"),
        Some("En Boy Oranı"),
        Some("Varsayılan Olarak Aç"),
        Some("Uygulama Bilgileri"),
    ], // Turkish
];

/// The menu bar's words in `lang`, as macOS's own menus say them: its own words, DSA's shared
/// words in the Mac's usual form, and an ellipsis after the items that ask for more, as on
/// every Mac.
pub fn menu_text(lang: UiLanguage) -> MenuText {
    use std::sync::OnceLock;
    static TEXTS: OnceLock<Vec<MenuText>> = OnceLock::new();
    let texts = TEXTS.get_or_init(|| {
        UiLanguage::ALL
            .into_iter()
            .map(|lang| {
                let os = os_words(lang);
                let shared = dsa_menu_text(lang);
                let own = OWN_WORDS[lang.index()];
                // Leaked once for the whole run, so the words stay `'static`.
                let word =
                    |own: Option<&'static str>, shared: &str, ellipsis: bool| -> &'static str {
                        let mut word = own.unwrap_or(shared).to_string();
                        if ellipsis {
                            word.push('\u{2026}');
                        }
                        Box::leak(word.into_boxed_str())
                    };
                MenuText {
                    window: os.window,
                    minimize: os.minimize,
                    zoom: os.zoom,
                    new_window: word(own[0], shared.new_window, false),
                    screenshot: word(own[1], shared.screenshot, false),
                    aspect_ratio: word(own[2], shared.aspect_ratio, true),
                    open_by_default: word(own[3], shared.open_by_default, true),
                    app_info: word(own[4], shared.app_info, false),
                    bring_all_to_front: os.bring_all_to_front,
                    hide_app: os.hide_app,
                    hide_others: os.hide_others,
                    show_all: os.show_all,
                    quit_app: os.quit_app,
                }
            })
            .collect()
    });
    texts[lang.index()]
}

/// macOS's own words in `lang`.
fn os_words(lang: UiLanguage) -> OsWords {
    match lang {
        UiLanguage::English => OsWords {
            window: "Window",
            minimize: "Minimize",
            zoom: "Zoom",
            bring_all_to_front: "Bring All to Front",
            hide_app: "Hide {app}",
            hide_others: "Hide Others",
            show_all: "Show All",
            quit_app: "Quit {app}",
        },
        UiLanguage::Korean => OsWords {
            window: "윈도우",
            minimize: "최소화",
            zoom: "확대/축소",
            bring_all_to_front: "모두 앞으로 가져오기",
            hide_app: "{app} 가리기",
            hide_others: "기타 가리기",
            show_all: "모두 보기",
            quit_app: "{app} 종료",
        },
        UiLanguage::Japanese => OsWords {
            window: "ウインドウ",
            minimize: "しまう",
            zoom: "拡大/縮小",
            bring_all_to_front: "すべてを手前に移動",
            hide_app: "{app}を非表示",
            hide_others: "ほかを非表示",
            show_all: "すべてを表示",
            quit_app: "{app}を終了",
        },
        UiLanguage::ChineseSimplified => OsWords {
            window: "窗口",
            minimize: "最小化",
            zoom: "缩放",
            bring_all_to_front: "前置全部窗口",
            hide_app: "隐藏\u{201c}{app}\u{201d}",
            hide_others: "隐藏其他",
            show_all: "全部显示",
            quit_app: "退出\u{201c}{app}\u{201d}",
        },
        UiLanguage::ChineseTraditional => OsWords {
            window: "視窗",
            minimize: "縮到最小",
            zoom: "縮放",
            bring_all_to_front: "將此程式所有視窗移至最前",
            hide_app: "隱藏\u{300c}{app}\u{300d}",
            hide_others: "隱藏其他",
            show_all: "顯示全部",
            quit_app: "結束\u{300c}{app}\u{300d}",
        },
        UiLanguage::Spanish => OsWords {
            window: "Ventana",
            minimize: "Minimizar",
            zoom: "Zoom",
            bring_all_to_front: "Traer todo al frente",
            hide_app: "Ocultar {app}",
            hide_others: "Ocultar otros",
            show_all: "Mostrar todo",
            quit_app: "Salir de {app}",
        },
        UiLanguage::French => OsWords {
            window: "Fenêtre",
            minimize: "Placer dans le Dock",
            zoom: "Réduire/agrandir",
            bring_all_to_front: "Tout ramener au premier plan",
            hide_app: "Masquer {app}",
            hide_others: "Masquer les autres",
            show_all: "Tout afficher",
            quit_app: "Quitter {app}",
        },
        UiLanguage::German => OsWords {
            window: "Fenster",
            minimize: "Im Dock ablegen",
            zoom: "Zoomen",
            bring_all_to_front: "Alle nach vorne bringen",
            hide_app: "{app} ausblenden",
            hide_others: "Andere ausblenden",
            show_all: "Alle einblenden",
            quit_app: "{app} beenden",
        },
        UiLanguage::PortugueseBrazil => OsWords {
            window: "Janela",
            minimize: "Minimizar",
            zoom: "Zoom",
            bring_all_to_front: "Trazer Todas para a Frente",
            hide_app: "Ocultar {app}",
            hide_others: "Ocultar Outros",
            show_all: "Mostrar Tudo",
            quit_app: "Encerrar {app}",
        },
        UiLanguage::Russian => OsWords {
            window: "Окно",
            minimize: "Свернуть",
            zoom: "Изменить масштаб",
            bring_all_to_front: "Переместить все окна вперед",
            hide_app: "Скрыть {app}",
            hide_others: "Скрыть остальные",
            show_all: "Показать все",
            quit_app: "Завершить {app}",
        },
        UiLanguage::Italian => OsWords {
            window: "Finestra",
            minimize: "Contrai",
            zoom: "Zoom",
            bring_all_to_front: "Porta tutto in primo piano",
            hide_app: "Nascondi {app}",
            hide_others: "Nascondi altre",
            show_all: "Mostra tutte",
            quit_app: "Esci da {app}",
        },
        UiLanguage::Vietnamese => OsWords {
            window: "Cửa sổ",
            minimize: "Thu nhỏ",
            zoom: "Thu/phóng",
            bring_all_to_front: "Đưa tất cả ra trước",
            hide_app: "Ẩn {app}",
            hide_others: "Ẩn ứng dụng khác",
            show_all: "Hiển thị tất cả",
            quit_app: "Thoát {app}",
        },
        UiLanguage::Indonesian => OsWords {
            window: "Jendela",
            minimize: "Minimalkan",
            zoom: "Perbesar/Perkecil",
            bring_all_to_front: "Bawa Semua ke Depan",
            hide_app: "Sembunyikan {app}",
            hide_others: "Sembunyikan Lainnya",
            show_all: "Tampilkan Semua",
            quit_app: "Keluar dari {app}",
        },
        UiLanguage::Turkish => OsWords {
            window: "Pencere",
            minimize: "Küçült",
            zoom: "Büyüt/Küçült",
            bring_all_to_front: "Tümünü Öne Getir",
            hide_app: "{app} Uygulamasını Gizle",
            hide_others: "Diğerlerini Gizle",
            show_all: "Tümünü Göster",
            quit_app: "{app} Uygulamasından Çık",
        },
    }
}

/// The label of a Window menu entry; `None` for the separator.
pub fn label(text: &MenuText, item: WindowMenuItem) -> Option<&'static str> {
    Some(match item {
        WindowMenuItem::Separator => return None,
        WindowMenuItem::Minimize => text.minimize,
        WindowMenuItem::Zoom => text.zoom,
        WindowMenuItem::NewWindow => text.new_window,
        WindowMenuItem::Screenshot => text.screenshot,
        WindowMenuItem::AspectRatio => text.aspect_ratio,
        WindowMenuItem::OpenByDefault => text.open_by_default,
        WindowMenuItem::AppInfo => text.app_info,
        WindowMenuItem::BringAllToFront => text.bring_all_to_front,
    })
}

/// One entry of the app menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppMenuItem {
    Separator,
    /// Its label, its AppKit action, its key with Command, and whether Option goes with it.
    Item(&'static str, &'static str, &'static str, bool),
}

/// The app menu: Hide, Hide Others, Show All and Quit, as every Mac app has them. Quitting
/// the window host closes the app's windows, which ends their tasks, as closing them does.
pub fn app_menu(text: &MenuText) -> Vec<AppMenuItem> {
    vec![
        AppMenuItem::Item(text.hide_app, "hide:", "h", false),
        AppMenuItem::Item(text.hide_others, "hideOtherApplications:", "h", true),
        AppMenuItem::Item(text.show_all, "unhideAllApplications:", "", false),
        AppMenuItem::Separator,
        AppMenuItem::Item(text.quit_app, "terminate:", "q", false),
    ]
}

/// `label` with the app's name in the place of `{app}`.
pub fn with_app(label: &str, app: &str) -> String {
    label.replace("{app}", app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr22_the_window_menu_carries_the_android_window_menu() {
        use WindowMenuItem::*;
        assert_eq!(
            window_menu(true),
            vec![
                Minimize,
                Zoom,
                Separator,
                NewWindow,
                Screenshot,
                AspectRatio,
                OpenByDefault,
                AppInfo,
                Separator,
                BringAllToFront,
            ]
        );
    }

    #[test]
    fn fr22_new_window_is_left_out_for_an_app_with_one_window() {
        assert!(!window_menu(false).contains(&WindowMenuItem::NewWindow));
        assert!(!android_items(false).contains(&WindowMenuItem::NewWindow));
        assert_eq!(
            android_items(false).first(),
            Some(&WindowMenuItem::Screenshot)
        );
    }

    #[test]
    fn fr22_each_android_item_is_carried_out_where_it_belongs() {
        for item in android_items(true) {
            assert_eq!(from_tag(tag(item)), Some(item));
        }
        assert_eq!(from_tag(0), None);
        assert_eq!(
            action(WindowMenuItem::NewWindow),
            Some(MenuAction::NewWindow)
        );
        assert_eq!(
            action(WindowMenuItem::AspectRatio),
            Some(MenuAction::AspectRatio)
        );
        assert_eq!(
            action(WindowMenuItem::OpenByDefault),
            Some(MenuAction::OpenByDefault)
        );
        assert_eq!(action(WindowMenuItem::AppInfo), Some(MenuAction::AppInfo));
        // The window host takes the screenshot itself.
        assert_eq!(action(WindowMenuItem::Screenshot), None);
        // macOS carries out its own entries.
        assert_eq!(
            system_action(WindowMenuItem::Minimize),
            Some("performMiniaturize:")
        );
        assert_eq!(tag(WindowMenuItem::Minimize), 0);
    }

    #[test]
    fn fr22_the_menus_speak_the_users_language() {
        let english = menu_text(UiLanguage::English);
        let korean = menu_text(UiLanguage::Korean);
        for item in window_menu(true) {
            if item == WindowMenuItem::Separator {
                assert_eq!(label(&english, item), None);
                continue;
            }
            let (en, ko) = (
                label(&english, item).unwrap(),
                label(&korean, item).unwrap(),
            );
            assert!(!en.is_empty() && !ko.is_empty() && en != ko, "{item:?}");
        }
        assert_eq!(label(&korean, WindowMenuItem::AppInfo), Some("앱 정보"));
        assert_eq!(with_app(english.quit_app, "Notes"), "Quit Notes");
        assert_eq!(with_app(korean.hide_app, "메모"), "메모 가리기");
        let german = menu_text(UiLanguage::German);
        assert_eq!(with_app(german.quit_app, "Notizen"), "Notizen beenden");
    }

    #[test]
    fn fr17_every_menu_entry_is_in_every_language() {
        for lang in UiLanguage::ALL {
            let text = menu_text(lang);
            let mut words: Vec<&str> = window_menu(true)
                .into_iter()
                .filter_map(|item| label(&text, item))
                .collect();
            words.push(text.window);
            for entry in app_menu(&text) {
                if let AppMenuItem::Item(title, ..) = entry {
                    words.push(title);
                }
            }
            for word in &words {
                assert!(!word.trim().is_empty(), "{lang:?} has an empty entry");
                assert!(!crate::ui_language::has_em_dash(word), "{lang:?}: {word}");
            }
            // Hide and Quit name the app, wherever the language puts its name.
            for named in [text.hide_app, text.quit_app] {
                assert_eq!(
                    dsa_host_core::ui_language::placeholders(named),
                    ["app"],
                    "{lang:?}: {named}"
                );
            }
            for unnamed in [text.hide_others, text.show_all, text.window] {
                assert!(
                    dsa_host_core::ui_language::placeholders(unnamed).is_empty(),
                    "{lang:?}: {unnamed}"
                );
            }
            // The entries that open a dialog end in an ellipsis, as on every Mac.
            for dialog in [text.aspect_ratio, text.open_by_default] {
                assert!(dialog.ends_with('\u{2026}'), "{lang:?}: {dialog}");
            }
            if lang != UiLanguage::English {
                assert_ne!(text.window, english_window(), "{lang:?} is not translated");
            }
        }
    }

    fn english_window() -> &'static str {
        menu_text(UiLanguage::English).window
    }
}
