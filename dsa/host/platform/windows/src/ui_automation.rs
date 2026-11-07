//! What a task window's UI Automation tree says about each Android accessibility node: its
//! control type, the patterns it supports, and whether a client lists it.
//!
//! The UI Automation provider of the app window host ([`crate::app_window::accessibility`])
//! asks these questions; the answers are plain values, so they are tested on every host. The
//! numbers are UI Automation's own (UIAutomationClient.h): control type IDs from 50000,
//! pattern IDs from 10000.

use dsa_host_core::accessibility::{Node, NodeAction, Role};

/// UI Automation control type IDs.
pub mod control_type {
    pub const BUTTON: i32 = 50000;
    pub const CHECK_BOX: i32 = 50002;
    pub const COMBO_BOX: i32 = 50003;
    pub const EDIT: i32 = 50004;
    pub const HYPERLINK: i32 = 50005;
    pub const IMAGE: i32 = 50006;
    pub const LIST_ITEM: i32 = 50007;
    pub const LIST: i32 = 50008;
    pub const MENU: i32 = 50009;
    pub const MENU_ITEM: i32 = 50011;
    pub const PROGRESS_BAR: i32 = 50012;
    pub const RADIO_BUTTON: i32 = 50013;
    pub const SLIDER: i32 = 50015;
    pub const TAB: i32 = 50018;
    pub const TAB_ITEM: i32 = 50019;
    pub const TEXT: i32 = 50020;
    pub const TOOL_BAR: i32 = 50021;
    pub const GROUP: i32 = 50026;
    pub const DATA_GRID: i32 = 50028;
    pub const DOCUMENT: i32 = 50030;
    pub const PANE: i32 = 50033;
    pub const SEPARATOR: i32 = 50038;
}

/// The UI Automation control patterns a node can support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pattern {
    Invoke,
    Value,
    Toggle,
    SelectionItem,
    Scroll,
}

impl Pattern {
    /// The pattern's UI Automation ID.
    pub fn id(self) -> i32 {
        match self {
            Pattern::Invoke => 10000,
            Pattern::Value => 10002,
            Pattern::Scroll => 10004,
            Pattern::SelectionItem => 10010,
            Pattern::Toggle => 10015,
        }
    }

    pub fn from_id(id: i32) -> Option<Pattern> {
        [
            Pattern::Invoke,
            Pattern::Value,
            Pattern::Toggle,
            Pattern::SelectionItem,
            Pattern::Scroll,
        ]
        .into_iter()
        .find(|pattern| pattern.id() == id)
    }
}

/// The UI Automation control type of a role. A switch is a button with the Toggle pattern, as
/// UI Automation describes toggle switches; a scrolling container is a pane.
pub fn control_type_of(role: Role) -> i32 {
    use control_type::*;
    match role {
        Role::Button | Role::Switch => BUTTON,
        Role::CheckBox => CHECK_BOX,
        Role::RadioButton => RADIO_BUTTON,
        Role::Edit => EDIT,
        Role::Text | Role::Heading => TEXT,
        Role::Image => IMAGE,
        Role::List => LIST,
        Role::ListItem => LIST_ITEM,
        Role::Grid => DATA_GRID,
        Role::ScrollView => PANE,
        Role::Slider => SLIDER,
        Role::ProgressBar => PROGRESS_BAR,
        Role::ComboBox => COMBO_BOX,
        Role::Tab => TAB_ITEM,
        Role::TabList => TAB,
        Role::Menu => MENU,
        Role::MenuItem => MENU_ITEM,
        Role::Toolbar => TOOL_BAR,
        Role::Link => HYPERLINK,
        Role::WebView => DOCUMENT,
        Role::Group => GROUP,
        Role::Unknown => PANE,
    }
}

/// The patterns a node supports, from what it can do and what it is.
pub fn patterns_of(node: &Node) -> Vec<Pattern> {
    let role = node.role();
    let mut patterns = Vec::new();
    let toggles = node.states.checkable || role == Role::Switch || role == Role::CheckBox;
    let selects =
        node.supports(NodeAction::Select) || matches!(role, Role::Tab | Role::RadioButton);
    if node.supports(NodeAction::Click)
        && !toggles
        && !matches!(role, Role::RadioButton | Role::Tab)
    {
        patterns.push(Pattern::Invoke);
    }
    if toggles && role != Role::RadioButton {
        patterns.push(Pattern::Toggle);
    }
    if selects {
        patterns.push(Pattern::SelectionItem);
    }
    if node.states.editable {
        patterns.push(Pattern::Value);
    }
    if node.states.scrollable
        && (node.supports(NodeAction::ScrollForward) || node.supports(NodeAction::ScrollBackward))
    {
        patterns.push(Pattern::Scroll);
    }
    patterns
}

/// Whether UI Automation's control view lists the node: everything but a layout that only
/// groups others and that the user cannot act on, which would make a screen reader stop on
/// every row's empty frame.
pub fn is_control_element(node: &Node, name: &str) -> bool {
    let role = node.role();
    if matches!(role, Role::Group | Role::Unknown) {
        return !name.is_empty() || node.states.focusable || node.states.clickable;
    }
    true
}

/// Whether the content view lists the node: a control that carries something to read or do.
pub fn is_content_element(node: &Node, name: &str) -> bool {
    is_control_element(node, name) && (!name.is_empty() || !patterns_of(node).is_empty())
}

/// The action a scroll of `amount` asks for: UI Automation's ScrollAmount, where 0 and 1
/// scroll back and 3 and 4 forward (LargeDecrement, SmallDecrement, NoAmount, LargeIncrement,
/// SmallIncrement).
pub fn scroll_action(amount: i32) -> Option<NodeAction> {
    match amount {
        0 | 1 => Some(NodeAction::ScrollBackward),
        3 | 4 => Some(NodeAction::ScrollForward),
        _ => None,
    }
}
