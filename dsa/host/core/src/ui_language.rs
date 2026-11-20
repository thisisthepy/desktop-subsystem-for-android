//! The languages DSA's own user interface is shown in, and how the system's display language
//! picks one.
//!
//! Every host surface (menus, notices, accessibility names, prompts) shows its words in one of
//! the fourteen languages the README is translated into. The system's display language picks
//! it; a language DSA has no translation for falls back to English. Chinese follows the script:
//! Traditional for `zh-Hant` and the regions that write it (Taiwan, Hong Kong, Macao),
//! Simplified otherwise. Portuguese is Brazilian Portuguese in every region, and Indonesian
//! answers to its old code `in` as well.
//!
//! A platform reads its display language in its own way (a Windows language ID, `LANGUAGE`
//! and the POSIX locale variables, `NSLocale.preferredLanguages`) and hands the tags to
//! [`UiLanguage::from_tag`] or [`UiLanguage::first_supported`].

/// A language DSA's user interface is translated into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum UiLanguage {
    #[default]
    English,
    Korean,
    Japanese,
    /// Simplified Chinese, `zh-CN`.
    ChineseSimplified,
    /// Traditional Chinese, `zh-TW`.
    ChineseTraditional,
    Spanish,
    French,
    German,
    /// Brazilian Portuguese, `pt-BR`.
    PortugueseBrazil,
    Russian,
    Italian,
    Vietnamese,
    Indonesian,
    Turkish,
}

impl UiLanguage {
    /// The language a POSIX session whose environment `var` reads shows messages in, as gettext
    /// picks it: the first of `LC_ALL`, `LC_MESSAGES` and `LANG` that is set names the locale; when
    /// it names a language (not `C` or `POSIX`), `LANGUAGE`, a colon list by preference, comes
    /// first. Anything DSA has no translation for is English.
    pub fn from_posix_env(var: impl Fn(&str) -> Option<String>) -> UiLanguage {
        let Some(locale) = posix_messages_locale(&var) else {
            return UiLanguage::English;
        };
        if is_posix_c_locale(&locale) {
            return UiLanguage::English;
        }
        if let Some(list) = var("LANGUAGE") {
            let languages: Vec<&str> = list
                .split(':')
                .map(str::trim)
                .filter(|language| !language.is_empty())
                .collect();
            if !languages.is_empty() {
                return UiLanguage::first_supported(languages);
            }
        }
        UiLanguage::from_tag_or_english(&locale)
    }

    /// Every language, English first, in the order the README lists its translations.
    pub const ALL: [UiLanguage; 14] = [
        UiLanguage::English,
        UiLanguage::Korean,
        UiLanguage::Japanese,
        UiLanguage::ChineseSimplified,
        UiLanguage::ChineseTraditional,
        UiLanguage::Spanish,
        UiLanguage::French,
        UiLanguage::German,
        UiLanguage::PortugueseBrazil,
        UiLanguage::Russian,
        UiLanguage::Italian,
        UiLanguage::Vietnamese,
        UiLanguage::Indonesian,
        UiLanguage::Turkish,
    ];

    /// The language's tag, as the README's translations are named: `en`, `ko`, `ja`,
    /// `zh-CN`, `zh-TW`, `es`, `fr`, `de`, `pt-BR`, `ru`, `it`, `vi`, `id`, `tr`.
    pub fn tag(self) -> &'static str {
        match self {
            UiLanguage::English => "en",
            UiLanguage::Korean => "ko",
            UiLanguage::Japanese => "ja",
            UiLanguage::ChineseSimplified => "zh-CN",
            UiLanguage::ChineseTraditional => "zh-TW",
            UiLanguage::Spanish => "es",
            UiLanguage::French => "fr",
            UiLanguage::German => "de",
            UiLanguage::PortugueseBrazil => "pt-BR",
            UiLanguage::Russian => "ru",
            UiLanguage::Italian => "it",
            UiLanguage::Vietnamese => "vi",
            UiLanguage::Indonesian => "id",
            UiLanguage::Turkish => "tr",
        }
    }

    /// The position of the language in [`UiLanguage::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// The translation DSA shows for the language tag `tag`, or `None` when DSA has none and
    /// the caller should try the next preferred language or fall back to English.
    ///
    /// `tag` may be a BCP 47 tag (`ko-KR`, `zh-Hant-HK`), a POSIX locale (`pt_BR.UTF-8`,
    /// `sr_RS@latin`) or a Windows locale name (`zh-TW`), in any case. `C` and `POSIX` name no
    /// language.
    pub fn from_tag(tag: &str) -> Option<UiLanguage> {
        // Drop a POSIX encoding (`.UTF-8`) and modifier (`@latin`).
        let tag = tag.trim();
        let tag = tag.split(['.', '@']).next().unwrap_or_default();
        let mut parts = tag
            .split(['-', '_'])
            .filter(|part| !part.is_empty())
            .map(str::to_ascii_lowercase);
        let language = parts.next()?;
        let rest: Vec<String> = parts.collect();
        Some(match language.as_str() {
            "en" => UiLanguage::English,
            "ko" => UiLanguage::Korean,
            "ja" => UiLanguage::Japanese,
            "zh" => {
                let traditional = rest
                    .iter()
                    .any(|part| matches!(part.as_str(), "hant" | "tw" | "hk" | "mo"))
                    && !rest.iter().any(|part| part == "hans");
                if traditional {
                    UiLanguage::ChineseTraditional
                } else {
                    UiLanguage::ChineseSimplified
                }
            }
            "es" => UiLanguage::Spanish,
            "fr" => UiLanguage::French,
            "de" => UiLanguage::German,
            "pt" => UiLanguage::PortugueseBrazil,
            "ru" => UiLanguage::Russian,
            "it" => UiLanguage::Italian,
            "vi" => UiLanguage::Vietnamese,
            "id" | "in" => UiLanguage::Indonesian,
            "tr" => UiLanguage::Turkish,
            _ => return None,
        })
    }

    /// The language for `tag`, English when DSA has no translation for it.
    pub fn from_tag_or_english(tag: &str) -> UiLanguage {
        UiLanguage::from_tag(tag).unwrap_or_default()
    }

    /// The first of the user's preferred languages, most preferred first, that DSA has a
    /// translation for, else English. A preference list such as macOS's or gettext's
    /// `LANGUAGE` goes on to the next language when DSA has none for the first.
    pub fn first_supported<'a>(tags: impl IntoIterator<Item = &'a str>) -> UiLanguage {
        tags.into_iter()
            .find_map(UiLanguage::from_tag)
            .unwrap_or_default()
    }
}

/// The `{0}`, `{1}`, ... and `{name}` placeholders of a text, sorted, for tests that check a
/// translation keeps the placeholders of the English text.
pub fn placeholders(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        if let Some(stripped) = after.strip_prefix('{') {
            // `{{` is a literal brace.
            rest = stripped;
            continue;
        }
        match after.find('}') {
            Some(close) => {
                found.push(after[..close].to_string());
                rest = &after[close + 1..];
            }
            None => break,
        }
    }
    found.sort();
    found
}

/// The locale gettext would read messages in, from `LC_ALL`, `LC_MESSAGES` or `LANG`, the
/// first that is set and not empty, as `var` reads them.
fn posix_messages_locale(var: &impl Fn(&str) -> Option<String>) -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| var(name).filter(|value| !value.trim().is_empty()))
}

/// Whether `locale` is the C locale (`C`, `POSIX`, `C.UTF-8`), which names no language and
/// under which gettext ignores `LANGUAGE`.
fn is_posix_c_locale(locale: &str) -> bool {
    let name = locale.trim().split(['.', '@']).next().unwrap_or_default();
    name == "C" || name == "POSIX"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr17_every_language_has_its_readme_tag() {
        let tags: Vec<&str> = UiLanguage::ALL.iter().map(|l| l.tag()).collect();
        assert_eq!(
            tags,
            [
                "en", "ko", "ja", "zh-CN", "zh-TW", "es", "fr", "de", "pt-BR", "ru", "it", "vi",
                "id", "tr"
            ]
        );
        for (index, language) in UiLanguage::ALL.iter().enumerate() {
            assert_eq!(language.index(), index);
            assert_eq!(UiLanguage::from_tag(language.tag()), Some(*language));
        }
    }

    #[test]
    fn fr17_system_language_tags_pick_their_translation() {
        let cases = [
            ("en-US", UiLanguage::English),
            ("en_GB.UTF-8", UiLanguage::English),
            ("ko", UiLanguage::Korean),
            ("ko-KR", UiLanguage::Korean),
            ("ko_KR.UTF-8", UiLanguage::Korean),
            ("ja-JP", UiLanguage::Japanese),
            ("es-MX", UiLanguage::Spanish),
            ("fr-CA", UiLanguage::French),
            ("de-CH", UiLanguage::German),
            ("ru-RU", UiLanguage::Russian),
            ("it-IT", UiLanguage::Italian),
            ("vi-VN", UiLanguage::Vietnamese),
            ("id-ID", UiLanguage::Indonesian),
            ("in_ID", UiLanguage::Indonesian),
            ("tr-TR", UiLanguage::Turkish),
        ];
        for (tag, language) in cases {
            assert_eq!(UiLanguage::from_tag(tag), Some(language), "{tag}");
        }
    }

    #[test]
    fn fr17_chinese_follows_its_script_and_region() {
        for tag in [
            "zh-Hant",
            "zh-TW",
            "zh_TW.UTF-8",
            "zh-HK",
            "zh-MO",
            "zh-Hant-HK",
            "zh-Hant-TW",
            "ZH-hant",
        ] {
            assert_eq!(
                UiLanguage::from_tag(tag),
                Some(UiLanguage::ChineseTraditional),
                "{tag}"
            );
        }
        for tag in [
            "zh",
            "zh-Hans",
            "zh-CN",
            "zh_CN.GB18030",
            "zh-SG",
            "zh-Hans-HK",
            "zh-MY",
        ] {
            assert_eq!(
                UiLanguage::from_tag(tag),
                Some(UiLanguage::ChineseSimplified),
                "{tag}"
            );
        }
    }

    #[test]
    fn fr17_portuguese_is_brazilian_in_every_region() {
        for tag in ["pt", "pt-BR", "pt_BR.UTF-8", "pt-PT", "pt-AO"] {
            assert_eq!(
                UiLanguage::from_tag(tag),
                Some(UiLanguage::PortugueseBrazil),
                "{tag}"
            );
        }
    }

    #[test]
    fn fr17_other_languages_fall_back_to_english() {
        for tag in ["nl-NL", "pl", "sv_SE.UTF-8", "kok", "C", "POSIX", "", "  "] {
            assert_eq!(UiLanguage::from_tag(tag), None, "{tag}");
            assert_eq!(UiLanguage::from_tag_or_english(tag), UiLanguage::English);
        }
        assert_eq!(UiLanguage::default(), UiLanguage::English);
    }

    #[test]
    fn fr17_the_first_preferred_language_with_a_translation_wins() {
        assert_eq!(
            UiLanguage::first_supported(["nl-NL", "de-DE", "en-US"]),
            UiLanguage::German
        );
        assert_eq!(
            UiLanguage::first_supported(["ko-KR", "en-US"]),
            UiLanguage::Korean
        );
        assert_eq!(
            UiLanguage::first_supported(["pl", "sv"]),
            UiLanguage::English
        );
        assert_eq!(UiLanguage::first_supported([]), UiLanguage::English);
    }

    #[test]
    fn fr17_placeholders_are_found_and_sorted() {
        assert_eq!(placeholders("{1} of {0}"), ["0", "1"]);
        assert_eq!(placeholders("{label} ({seconds} s)"), ["label", "seconds"]);
        assert_eq!(placeholders("{{literal}} {0}"), ["0"]);
        assert!(placeholders("none").is_empty());
    }
}
