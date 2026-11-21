//! The language the Linux host shows its own words in: its menus, notices, accessibility
//! names and prompts.
//!
//! The session's display language decides, read as gettext reads it, so DSA's words come in
//! the same language as the desktop's own programs. The locale is the first of `LC_ALL`,
//! `LC_MESSAGES` and `LANG` that is set. When it names a language (it is not `C` or `POSIX`),
//! `LANGUAGE`, a colon list of languages by preference, comes first: its first language DSA
//! has a translation for wins, and English when it has none of them, as gettext falls back to
//! the untranslated text. Without `LANGUAGE` the locale picks. Anything DSA has no translation
//! for is English.

use dsa_host_core::ui_language::UiLanguage;

/// The interface language of a session whose environment `var` reads: see the module. The
/// rule is gettext's and so the same on every POSIX host; it lives in core.
pub fn ui_language_in(var: impl Fn(&str) -> Option<String>) -> UiLanguage {
    UiLanguage::from_posix_env(var)
}

/// The interface language of this process's session, from its environment.
pub fn ui_language() -> UiLanguage {
    ui_language_in(|name| std::env::var(name).ok())
}

/// The POSIX locale name of `language`, as AT-SPI's `Locale` property and the desktop's
/// locale variables spell it.
pub fn posix_locale(language: UiLanguage) -> &'static str {
    match language {
        UiLanguage::English => "en_US",
        UiLanguage::Korean => "ko_KR",
        UiLanguage::Japanese => "ja_JP",
        UiLanguage::ChineseSimplified => "zh_CN",
        UiLanguage::ChineseTraditional => "zh_TW",
        UiLanguage::Spanish => "es_ES",
        UiLanguage::French => "fr_FR",
        UiLanguage::German => "de_DE",
        UiLanguage::PortugueseBrazil => "pt_BR",
        UiLanguage::Russian => "ru_RU",
        UiLanguage::Italian => "it_IT",
        UiLanguage::Vietnamese => "vi_VN",
        UiLanguage::Indonesian => "id_ID",
        UiLanguage::Turkish => "tr_TR",
    }
}

/// The locale keys a translated value of a desktop entry, an AppStream file or a polkit
/// policy is written under for `language`; none for English, the untranslated value. A
/// desktop looks a key up by language and region and then by language alone, so a region
/// that shares a translation with another gets a key of its own: Hong Kong and Macao read
/// Traditional Chinese, Singapore Simplified, and every Portuguese region (`pt`) Brazilian
/// Portuguese.
pub fn locale_keys(language: UiLanguage) -> &'static [&'static str] {
    match language {
        UiLanguage::English => &[],
        UiLanguage::Korean => &["ko"],
        UiLanguage::Japanese => &["ja"],
        UiLanguage::ChineseSimplified => &["zh_CN", "zh_SG"],
        UiLanguage::ChineseTraditional => &["zh_TW", "zh_HK", "zh_MO"],
        UiLanguage::Spanish => &["es"],
        UiLanguage::French => &["fr"],
        UiLanguage::German => &["de"],
        UiLanguage::PortugueseBrazil => &["pt_BR", "pt"],
        UiLanguage::Russian => &["ru"],
        UiLanguage::Italian => &["it"],
        UiLanguage::Vietnamese => &["vi"],
        UiLanguage::Indonesian => &["id"],
        UiLanguage::Turkish => &["tr"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn fr17_the_session_locale_picks_the_interface_language() {
        let cases = [
            ("ko_KR.UTF-8", UiLanguage::Korean),
            ("ja_JP.UTF-8", UiLanguage::Japanese),
            ("zh_TW.UTF-8", UiLanguage::ChineseTraditional),
            ("zh_HK.UTF-8", UiLanguage::ChineseTraditional),
            ("zh_CN.UTF-8", UiLanguage::ChineseSimplified),
            ("zh_SG", UiLanguage::ChineseSimplified),
            ("pt_PT.UTF-8", UiLanguage::PortugueseBrazil),
            ("pt_BR.UTF-8", UiLanguage::PortugueseBrazil),
            ("en_US.UTF-8", UiLanguage::English),
            ("nl_NL.UTF-8", UiLanguage::English),
        ];
        for (locale, language) in cases {
            assert_eq!(
                ui_language_in(env(&[("LANG", locale)])),
                language,
                "{locale}"
            );
        }
    }

    #[test]
    fn fr17_lc_all_and_lc_messages_come_before_lang() {
        assert_eq!(
            ui_language_in(env(&[("LC_ALL", "en_GB.UTF-8"), ("LANG", "ko_KR.UTF-8")])),
            UiLanguage::English
        );
        assert_eq!(
            ui_language_in(env(&[
                ("LC_MESSAGES", "de_DE.UTF-8"),
                ("LANG", "ko_KR.UTF-8")
            ])),
            UiLanguage::German
        );
        // An empty variable is not set.
        assert_eq!(
            ui_language_in(env(&[("LC_ALL", ""), ("LANG", "fr_FR.UTF-8")])),
            UiLanguage::French
        );
    }

    #[test]
    fn fr17_language_lists_the_languages_by_preference() {
        // The first DSA has a translation for wins.
        assert_eq!(
            ui_language_in(env(&[("LANGUAGE", "nl:de"), ("LANG", "nl_NL.UTF-8")])),
            UiLanguage::German
        );
        assert_eq!(
            ui_language_in(env(&[("LANGUAGE", "ko:en"), ("LANG", "en_US.UTF-8")])),
            UiLanguage::Korean
        );
        assert_eq!(
            ui_language_in(env(&[("LANGUAGE", "en_US:ko"), ("LANG", "ko_KR.UTF-8")])),
            UiLanguage::English
        );
        assert_eq!(
            ui_language_in(env(&[("LANGUAGE", "zh_TW:zh_CN"), ("LANG", "en_US.UTF-8")])),
            UiLanguage::ChineseTraditional
        );
        // None of them translated is English, as gettext shows the untranslated text.
        assert_eq!(
            ui_language_in(env(&[("LANGUAGE", "nl:sv"), ("LANG", "de_DE.UTF-8")])),
            UiLanguage::English
        );
        // An empty list is no list.
        assert_eq!(
            ui_language_in(env(&[("LANGUAGE", ":"), ("LANG", "it_IT.UTF-8")])),
            UiLanguage::Italian
        );
    }

    #[test]
    fn fr17_the_c_locale_and_no_locale_are_english() {
        // Under the C locale gettext ignores LANGUAGE.
        assert_eq!(
            ui_language_in(env(&[("LANGUAGE", "fr"), ("LANG", "C")])),
            UiLanguage::English
        );
        assert_eq!(
            ui_language_in(env(&[("LANGUAGE", "fr"), ("LC_ALL", "C.UTF-8")])),
            UiLanguage::English
        );
        assert_eq!(ui_language_in(env(&[("LANG", "C")])), UiLanguage::English);
        assert_eq!(
            ui_language_in(env(&[("LANG", "POSIX")])),
            UiLanguage::English
        );
        assert_eq!(ui_language_in(env(&[])), UiLanguage::English);
        assert_eq!(
            ui_language_in(env(&[("LANGUAGE", "fr")])),
            UiLanguage::English
        );
    }

    /// The value of `key[locale]=` in a desktop entry, if it has one.
    fn desktop_value<'a>(entry: &'a str, key: &str, locale: &str) -> Option<&'a str> {
        let prefix = format!("{key}[{locale}]=");
        entry
            .lines()
            .find_map(|line| line.strip_prefix(prefix.as_str()))
    }

    #[test]
    fn fr17_the_packaged_desktop_entries_speak_every_interface_language() {
        let entries = [
            (
                "dsa.desktop",
                include_str!("../packaging/desktop/dsa.desktop"),
            ),
            (
                "dsa-apk.desktop",
                include_str!("../packaging/desktop/dsa-apk.desktop"),
            ),
            (
                "dsa-open.desktop",
                include_str!("../packaging/desktop/dsa-open.desktop"),
            ),
            (
                "dsa-share.desktop",
                include_str!("../packaging/desktop/dsa-share.desktop"),
            ),
        ];
        for (file, entry) in entries {
            assert!(
                !entry.contains(char::from_u32(0x2014).unwrap()),
                "{file} has an em dash"
            );
            for key in ["Name", "Comment"] {
                let english = entry
                    .lines()
                    .find_map(|line| line.strip_prefix(&format!("{key}=")))
                    .unwrap_or_else(|| panic!("{file} has no {key}"));
                for language in UiLanguage::ALL {
                    let keys = locale_keys(language);
                    let Some(first) = keys.first() else {
                        continue;
                    };
                    let translated = desktop_value(entry, key, first)
                        .unwrap_or_else(|| panic!("{file} has no {key}[{first}]"));
                    assert!(!translated.trim().is_empty(), "{file} {key}[{first}]");
                    assert_ne!(translated, english, "{file} {key}[{first}] is English");
                    // A region that reads the same translation carries the same words.
                    for region in &keys[1..] {
                        assert_eq!(
                            desktop_value(entry, key, region),
                            Some(translated),
                            "{file} {key}[{region}]"
                        );
                    }
                    // Product names are never translated.
                    for name in ["Android", "DSA"] {
                        if english.contains(name) {
                            assert!(
                                translated.contains(name),
                                "{file} {key}[{first}] drops {name}: {translated}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fr17_the_packaged_file_types_and_software_center_entry_speak_every_interface_language() {
        let mime = include_str!("../packaging/mime/io.github.thisisthepy.DSA.apk-bundles.xml");
        let appstream =
            include_str!("../packaging/appstream/io.github.thisisthepy.DSA.metainfo.xml");
        for text in [mime, appstream] {
            assert!(!text.contains(char::from_u32(0x2014).unwrap()));
        }
        for language in UiLanguage::ALL {
            for key in locale_keys(language) {
                assert!(
                    mime.contains(&format!("<comment xml:lang=\"{key}\">")),
                    "the split APK type has no comment in {key}"
                );
            }
            if let Some(first) = locale_keys(language).first() {
                assert!(
                    appstream.contains(&format!("<summary xml:lang=\"{first}\">")),
                    "the package's AppStream entry has no summary in {first}"
                );
                assert_eq!(
                    appstream
                        .matches(&format!("<p xml:lang=\"{first}\">"))
                        .count(),
                    2,
                    "the package's AppStream entry has not both paragraphs in {first}"
                );
            }
        }
    }

    #[test]
    fn fr17_every_language_has_a_locale_and_its_keys() {
        let mut keys = Vec::new();
        for language in UiLanguage::ALL {
            let locale = posix_locale(language);
            assert_eq!(UiLanguage::from_tag(locale), Some(language), "{locale}");
            for key in locale_keys(language) {
                assert_eq!(UiLanguage::from_tag(key), Some(language), "{key}");
                keys.push(*key);
            }
        }
        assert!(locale_keys(UiLanguage::English).is_empty());
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "a key twice");
    }
}
