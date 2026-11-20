//! The words of the question an app's request to put a shortcut on the desktop
//! (`requestPinShortcut`) is asked with, in each of the languages the interface is shown in.
//!
//! Every host asks the same question: a heading about the shortcut, a line about the app that
//! asks, and [Add to desktop] and [Cancel]. A host that removes the shortcut with its app says
//! so after the line.

use crate::ui_language::UiLanguage;

/// The question's words in one language. The heading has `{name}`, the shortcut's name; the
/// body has `{app}`, the app's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinQuestionText {
    pub heading: &'static str,
    /// What the question says below its heading.
    pub body: &'static str,
    /// What a host adds after the body when the shortcut goes with the app, with the space or
    /// the lack of one its language needs.
    pub removed_with_app: &'static str,
    pub add: &'static str,
    pub cancel: &'static str,
}

/// The question's words in `language`.
pub fn pin_question_text(language: UiLanguage) -> PinQuestionText {
    let words = |heading, body, removed_with_app, add, cancel| PinQuestionText {
        heading,
        body,
        removed_with_app,
        add,
        cancel,
    };
    match language {
        UiLanguage::English => words(
            "Add \u{201c}{name}\u{201d} to the desktop?",
            "{app} wants to put this shortcut on your desktop.",
            " It goes when the app is uninstalled.",
            "Add to desktop",
            "Cancel",
        ),
        UiLanguage::Korean => words(
            "바탕 화면에 \u{201c}{name}\u{201d} 바로 가기를 추가할까요?",
            "{app}에서 이 바로 가기를 바탕 화면에 두려고 합니다.",
            " 앱을 지우면 바로 가기도 사라집니다.",
            "바탕 화면에 추가",
            "취소",
        ),
        UiLanguage::Japanese => words(
            "デスクトップに「{name}」を追加しますか?",
            "{app} がこのショートカットをデスクトップに置こうとしています。",
            "アプリをアンインストールすると、ショートカットも削除されます。",
            "デスクトップに追加",
            "キャンセル",
        ),
        UiLanguage::ChineseSimplified => words(
            "要将\u{201c}{name}\u{201d}添加到桌面吗?",
            "{app} 想将此快捷方式放到你的桌面上。",
            "卸载该应用后，快捷方式也会被删除。",
            "添加到桌面",
            "取消",
        ),
        UiLanguage::ChineseTraditional => words(
            "要將「{name}」新增到桌面嗎?",
            "{app} 想要將這個捷徑放到您的桌面上。",
            "解除安裝該應用程式後，捷徑也會一併移除。",
            "新增到桌面",
            "取消",
        ),
        UiLanguage::Spanish => words(
            "¿Agregar \u{201c}{name}\u{201d} al escritorio?",
            "{app} quiere poner este acceso directo en el escritorio.",
            " Se quitará cuando se desinstale la aplicación.",
            "Agregar al escritorio",
            "Cancelar",
        ),
        UiLanguage::French => words(
            "Ajouter \u{ab}\u{a0}{name}\u{a0}\u{bb} au Bureau\u{a0}?",
            "{app} veut placer ce raccourci sur votre Bureau.",
            " Il sera supprimé lors de la désinstallation de l'application.",
            "Ajouter au Bureau",
            "Annuler",
        ),
        UiLanguage::German => words(
            "\u{201e}{name}\u{201c} zum Desktop hinzufügen?",
            "{app} möchte diese Verknüpfung auf Ihrem Desktop ablegen.",
            " Sie wird entfernt, wenn die App deinstalliert wird.",
            "Zum Desktop hinzufügen",
            "Abbrechen",
        ),
        UiLanguage::PortugueseBrazil => words(
            "Adicionar \u{201c}{name}\u{201d} à área de trabalho?",
            "{app} quer colocar este atalho na sua área de trabalho.",
            " Ele será removido quando o app for desinstalado.",
            "Adicionar à área de trabalho",
            "Cancelar",
        ),
        UiLanguage::Russian => words(
            "Добавить \u{ab}{name}\u{bb} на рабочий стол?",
            "{app} хочет поместить этот ярлык на рабочий стол.",
            " Он будет удален при удалении приложения.",
            "Добавить на рабочий стол",
            "Отмена",
        ),
        UiLanguage::Italian => words(
            "Aggiungere \u{201c}{name}\u{201d} al desktop?",
            "{app} vuole aggiungere questo collegamento al desktop.",
            " Verrà rimosso quando l'app viene disinstallata.",
            "Aggiungi al desktop",
            "Annulla",
        ),
        UiLanguage::Vietnamese => words(
            "Thêm \u{201c}{name}\u{201d} vào màn hình nền?",
            "{app} muốn đặt lối tắt này trên màn hình nền của bạn.",
            " Lối tắt sẽ bị xóa khi ứng dụng được gỡ cài đặt.",
            "Thêm vào màn hình nền",
            "Hủy",
        ),
        UiLanguage::Indonesian => words(
            "Tambahkan \u{201c}{name}\u{201d} ke desktop?",
            "{app} ingin menaruh pintasan ini di desktop Anda.",
            " Pintasan akan hilang saat aplikasi dihapus instalannya.",
            "Tambahkan ke desktop",
            "Batal",
        ),
        UiLanguage::Turkish => words(
            "\u{201c}{name}\u{201d} masaüstüne eklensin mi?",
            "{app}, bu kısayolu masaüstünüze koymak istiyor.",
            " Uygulama kaldırıldığında kısayol da kaldırılır.",
            "Masaüstüne ekle",
            "İptal",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_language::UiLanguage;
    use crate::ui_text::fill;

    #[test]
    fn fr7_every_language_asks_with_the_shortcut_the_app_and_two_buttons() {
        for language in UiLanguage::ALL {
            let text = pin_question_text(language);
            assert!(text.heading.contains("{name}"), "{language:?}");
            assert!(text.body.contains("{app}"), "{language:?}");
            assert!(!text.removed_with_app.is_empty(), "{language:?}");
            assert!(
                !text.add.is_empty() && !text.cancel.is_empty(),
                "{language:?}"
            );
            for line in [
                text.heading,
                text.body,
                text.removed_with_app,
                text.add,
                text.cancel,
            ] {
                assert!(
                    !line.contains(char::from_u32(0x2014).unwrap()),
                    "{language:?}: an em dash"
                );
            }
        }
    }

    #[test]
    fn fr7_the_english_question_names_the_shortcut_and_the_app() {
        let text = pin_question_text(UiLanguage::English);
        assert_eq!(
            fill(text.heading, &[("name", "Ana")]),
            "Add \u{201c}Ana\u{201d} to the desktop?"
        );
        assert_eq!(
            fill(text.body, &[("app", "Chat")]),
            "Chat wants to put this shortcut on your desktop."
        );
        assert_eq!((text.add, text.cancel), ("Add to desktop", "Cancel"));
    }
}
