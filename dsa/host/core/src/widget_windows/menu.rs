//! What the menu of a widget window offers, whichever host draws it: Widget settings, for a
//! widget whose app lets it be set up again, Resize, with the sizes the widget allows, and
//! Remove, as Android's home screen offers them.
//!
//! The user opens it by long-pressing the widget, by right-clicking it, or with the menu key.
//! Choosing Widget settings asks the service to open the widget's setup again, which
//! shows as the app's window; once the user finishes it, the widget shows what they chose.
//! Choosing a size resizes the window, which tells the service and Android the new size as a
//! drag of its edges does. Choosing Remove asks the service to remove the widget; its window
//! closes when the service says the widget is gone, so the settings app and every other
//! watcher learn of it the same way.

use crate::widgets::{clamp_size, Placement, WidgetProvider};

/// A size the Resize menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeName {
    /// The smallest the widget allows.
    Small,
    /// The size the widget is placed at.
    Default,
    /// One and a half times its default.
    Large,
    /// Twice its default.
    ExtraLarge,
}

/// One size of the Resize menu, in dp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeChoice {
    pub name: SizeName,
    pub width_dp: u32,
    pub height_dp: u32,
}

/// What the user chose in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    /// Resize the window to this size, in dp.
    Resize { width_dp: u32, height_dp: u32 },
    /// Remove the widget.
    Remove,
    /// Open the widget's setup again.
    Settings,
}

/// The menu item of Remove. A size is [`FIRST_SIZE`] plus its place in the list.
pub const REMOVE: u32 = 1;
/// The menu item that opens the sizes.
pub const RESIZE: u32 = 2;
/// The menu item of Widget settings.
pub const SETTINGS: u32 = 3;
/// The menu item of the first size.
pub const FIRST_SIZE: u32 = 100;

/// The sizes the Resize menu offers for `provider`, each within what the widget allows. A
/// size equal to one before it is left out, so a widget that cannot be resized offers one.
pub fn size_choices(provider: &WidgetProvider) -> Vec<SizeChoice> {
    let (width, height) = clamp_size(
        provider,
        provider.default_width_dp,
        provider.default_height_dp,
    );
    let asked = [
        (SizeName::Small, 0, 0),
        (SizeName::Default, width, height),
        (SizeName::Large, width * 3 / 2, height * 3 / 2),
        (SizeName::ExtraLarge, width * 2, height * 2),
    ];
    let mut out: Vec<SizeChoice> = Vec::with_capacity(asked.len());
    for (name, w, h) in asked {
        let (width_dp, height_dp) = clamp_size(provider, w, h);
        if out
            .iter()
            .all(|c| (c.width_dp, c.height_dp) != (width_dp, height_dp))
        {
            out.push(SizeChoice {
                name,
                width_dp,
                height_dp,
            });
        }
    }
    out
}

/// What the menu item `command` chose among `sizes`; None for no choice (as when the menu was
/// dismissed) or an unknown one.
pub fn choice_of(command: u32, sizes: &[SizeChoice]) -> Option<MenuChoice> {
    if command == REMOVE {
        return Some(MenuChoice::Remove);
    }
    if command == SETTINGS {
        return Some(MenuChoice::Settings);
    }
    let size = sizes.get(command.checked_sub(FIRST_SIZE)? as usize)?;
    Some(MenuChoice::Resize {
        width_dp: size.width_dp,
        height_dp: size.height_dp,
    })
}

/// Whether `placement` is at the size of `choice`, which the menu marks.
pub fn is_current(choice: &SizeChoice, placement: &Placement) -> bool {
    (choice.width_dp, choice.height_dp) == (placement.width_dp, placement.height_dp)
}

/// The words of the menu in one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuText {
    pub settings: &'static str,
    pub resize: &'static str,
    pub remove: &'static str,
    pub small: &'static str,
    pub default: &'static str,
    pub large: &'static str,
    pub extra_large: &'static str,
    /// What assistive technology calls a widget with no name of its own.
    pub widget: &'static str,
}

/// The menu's words: Korean for a Korean display language, else English.
pub fn menu_text(korean: bool) -> MenuText {
    if korean {
        MenuText {
            settings: "위젯 설정",
            resize: "크기 조정",
            remove: "제거",
            small: "작게",
            default: "기본 크기",
            large: "크게",
            extra_large: "아주 크게",
            widget: "위젯",
        }
    } else {
        MenuText {
            settings: "Widget settings",
            resize: "Resize",
            remove: "Remove",
            small: "Small",
            default: "Default size",
            large: "Large",
            extra_large: "Extra large",
            widget: "Widget",
        }
    }
}

/// The label of a size in the menu, with the size itself, such as "Large (270 × 165)".
pub fn size_label(text: &MenuText, choice: &SizeChoice) -> String {
    let name = match choice.name {
        SizeName::Small => text.small,
        SizeName::Default => text.default,
        SizeName::Large => text.large,
        SizeName::ExtraLarge => text.extra_large,
    };
    format!("{name} ({} × {})", choice.width_dp, choice.height_dp)
}

/// What assistive technology calls the menu of a widget: the widget's own name, or "Widget".
pub fn menu_name(text: &MenuText, provider: &WidgetProvider) -> String {
    if provider.label.trim().is_empty() {
        text.widget.to_string()
    } else {
        provider.label.clone()
    }
}

/// One entry of a widget's menu, as a host's own menu shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// An item the user chooses, `command` naming it for [`choice_of`]. A size is `checked`
    /// when the widget is at it now.
    Item {
        command: u32,
        label: String,
        checked: bool,
    },
    /// Resize, which opens the sizes.
    Sizes {
        label: String,
        sizes: Vec<Entry>,
    },
    Separator,
}

/// The entries of a widget's menu, in order: Widget settings when the widget can be set up
/// again, Resize with its sizes (the current one checked), a separator, and Remove.
pub fn entries(text: &MenuText, provider: &WidgetProvider, placement: &Placement) -> Vec<Entry> {
    let sizes: Vec<Entry> = size_choices(provider)
        .iter()
        .zip(FIRST_SIZE..)
        .map(|(size, command)| Entry::Item {
            command,
            label: size_label(text, size),
            checked: is_current(size, placement),
        })
        .collect();
    let mut out = Vec::with_capacity(4);
    if provider.reconfigurable {
        out.push(Entry::Item {
            command: SETTINGS,
            label: text.settings.to_string(),
            checked: false,
        });
    }
    if !sizes.is_empty() {
        out.push(Entry::Sizes {
            label: text.resize.to_string(),
            sizes,
        });
        out.push(Entry::Separator);
    }
    out.push(Entry::Item {
        command: REMOVE,
        label: text.remove.to_string(),
        checked: false,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> WidgetProvider {
        WidgetProvider {
            provider: "com.example.clock/.ClockWidget".into(),
            label: "Clock".into(),
            min_width_dp: 110,
            min_height_dp: 40,
            default_width_dp: 180,
            default_height_dp: 110,
            resize_mode: 3,
            ..WidgetProvider::default()
        }
    }

    fn at(width_dp: u32, height_dp: u32) -> Placement {
        Placement {
            x: 0,
            y: 0,
            width_dp,
            height_dp,
            resized: false,
        }
    }

    #[test]
    fn fr12_right_clicking_a_widget_offers_resize_and_remove() {
        let text = menu_text(false);
        let menu = entries(&text, &clock(), &at(180, 110));
        assert!(matches!(&menu[0], Entry::Sizes { label, .. } if label == "Resize"));
        assert_eq!(menu[1], Entry::Separator);
        assert_eq!(
            menu[2],
            Entry::Item {
                command: REMOVE,
                label: "Remove".into(),
                checked: false
            }
        );
        let Entry::Sizes { sizes, .. } = &menu[0] else {
            unreachable!()
        };
        // The current size is checked, and each size chooses itself.
        let checked: Vec<&Entry> = sizes
            .iter()
            .filter(|entry| matches!(entry, Entry::Item { checked: true, .. }))
            .collect();
        assert_eq!(checked.len(), 1);
        let Entry::Item { command, label, .. } = checked[0] else {
            unreachable!()
        };
        assert_eq!(label, "Default size (180 × 110)");
        let choices = size_choices(&clock());
        assert_eq!(
            choice_of(*command, &choices),
            Some(MenuChoice::Resize {
                width_dp: 180,
                height_dp: 110
            })
        );
        assert_eq!(choice_of(REMOVE, &choices), Some(MenuChoice::Remove));
        assert_eq!(choice_of(9999, &choices), None);
    }

    #[test]
    fn fr12_the_widget_menu_offers_settings_only_for_a_widget_that_can_be_set_up_again() {
        let text = menu_text(true);
        let plain = entries(&text, &clock(), &at(180, 110));
        assert!(!plain.iter().any(|entry| matches!(
            entry,
            Entry::Item {
                command: SETTINGS,
                ..
            }
        )));
        let settable = WidgetProvider {
            reconfigurable: true,
            ..clock()
        };
        let menu = entries(&text, &settable, &at(180, 110));
        assert_eq!(
            menu[0],
            Entry::Item {
                command: SETTINGS,
                label: "위젯 설정".into(),
                checked: false
            }
        );
        assert_eq!(
            choice_of(SETTINGS, &size_choices(&settable)),
            Some(MenuChoice::Settings)
        );
    }

    #[test]
    fn fr12_a_widget_that_cannot_be_resized_offers_one_size() {
        let fixed = WidgetProvider {
            resize_mode: 0,
            ..clock()
        };
        assert_eq!(size_choices(&fixed).len(), 1);
        assert_eq!(
            menu_name(&menu_text(false), &WidgetProvider::default()),
            "Widget"
        );
        assert_eq!(menu_name(&menu_text(false), &clock()), "Clock");
    }
}
