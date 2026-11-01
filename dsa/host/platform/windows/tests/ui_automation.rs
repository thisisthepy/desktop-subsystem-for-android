//! What a task window's UI Automation tree says about each Android accessibility node.

use dsa_host_core::accessibility::{Node, NodeAction, NodeStates, Role};
use dsa_host_windows::ui_automation::{
    control_type, control_type_of, is_content_element, is_control_element, patterns_of,
    scroll_action, Pattern,
};

fn node(class_name: &str, states: NodeStates, actions: &[NodeAction]) -> Node {
    Node {
        id: 1,
        class_name: class_name.into(),
        states,
        actions: actions.to_vec(),
        ..Node::default()
    }
}

#[test]
fn fr18_roles_map_to_ui_automation_control_types() {
    let cases = [
        (Role::Button, control_type::BUTTON),
        (Role::Switch, control_type::BUTTON),
        (Role::CheckBox, control_type::CHECK_BOX),
        (Role::RadioButton, control_type::RADIO_BUTTON),
        (Role::Edit, control_type::EDIT),
        (Role::Text, control_type::TEXT),
        (Role::Heading, control_type::TEXT),
        (Role::Image, control_type::IMAGE),
        (Role::List, control_type::LIST),
        (Role::ListItem, control_type::LIST_ITEM),
        (Role::ScrollView, control_type::PANE),
        (Role::Slider, control_type::SLIDER),
        (Role::ProgressBar, control_type::PROGRESS_BAR),
        (Role::ComboBox, control_type::COMBO_BOX),
        (Role::Tab, control_type::TAB_ITEM),
        (Role::TabList, control_type::TAB),
        (Role::Toolbar, control_type::TOOL_BAR),
        (Role::WebView, control_type::DOCUMENT),
        (Role::Group, control_type::GROUP),
    ];
    for (role, expected) in cases {
        assert_eq!(control_type_of(role), expected, "{role:?}");
    }
    // UI Automation's own numbers.
    assert_eq!(control_type::BUTTON, 50000);
    assert_eq!(control_type::LIST_ITEM, 50007);
    assert_eq!(control_type::PANE, 50033);
}

#[test]
fn fr18_patterns_follow_what_a_node_can_do() {
    let clickable = NodeStates {
        clickable: true,
        enabled: true,
        ..NodeStates::default()
    };
    // A Settings row: Invoke.
    let row = node(
        "android.widget.LinearLayout",
        clickable,
        &[NodeAction::Click],
    );
    assert_eq!(patterns_of(&row), vec![Pattern::Invoke]);
    // A switch toggles rather than invokes.
    let switch = node(
        "android.widget.Switch",
        NodeStates {
            checkable: true,
            ..clickable
        },
        &[NodeAction::Click],
    );
    assert_eq!(patterns_of(&switch), vec![Pattern::Toggle]);
    // An editable field: Value.
    let field = node(
        "android.widget.EditText",
        NodeStates {
            editable: true,
            ..clickable
        },
        &[NodeAction::Click, NodeAction::SetText],
    );
    assert_eq!(patterns_of(&field), vec![Pattern::Invoke, Pattern::Value]);
    // A selectable tab.
    let tab = node(
        "com.google.android.material.tabs.TabLayout$TabView",
        clickable,
        &[NodeAction::Click, NodeAction::Select],
    );
    assert_eq!(patterns_of(&tab), vec![Pattern::SelectionItem]);
    // A list that scrolls.
    let list = node(
        "androidx.recyclerview.widget.RecyclerView",
        NodeStates {
            scrollable: true,
            ..NodeStates::default()
        },
        &[NodeAction::ScrollForward],
    );
    assert_eq!(patterns_of(&list), vec![Pattern::Scroll]);
    // Plain text: none.
    let text = node("android.widget.TextView", NodeStates::default(), &[]);
    assert!(patterns_of(&text).is_empty());
    for pattern in [
        Pattern::Invoke,
        Pattern::Value,
        Pattern::Toggle,
        Pattern::SelectionItem,
        Pattern::Scroll,
    ] {
        assert_eq!(Pattern::from_id(pattern.id()), Some(pattern));
    }
    assert_eq!(Pattern::Invoke.id(), 10000);
    assert_eq!(Pattern::Toggle.id(), 10015);
    assert_eq!(Pattern::from_id(10003), None);
}

#[test]
fn fr18_empty_layouts_are_left_out_of_the_control_view() {
    let frame = node("android.widget.FrameLayout", NodeStates::default(), &[]);
    assert!(!is_control_element(&frame, ""));
    assert!(is_control_element(&frame, "Toolbar"));
    let text = node("android.widget.TextView", NodeStates::default(), &[]);
    assert!(is_control_element(&text, "Wi-Fi"));
    assert!(is_content_element(&text, "Wi-Fi"));
    assert!(!is_content_element(&text, ""));
}

#[test]
fn fr18_a_scroll_amount_names_its_direction() {
    assert_eq!(scroll_action(0), Some(NodeAction::ScrollBackward));
    assert_eq!(scroll_action(1), Some(NodeAction::ScrollBackward));
    assert_eq!(scroll_action(2), None);
    assert_eq!(scroll_action(3), Some(NodeAction::ScrollForward));
    assert_eq!(scroll_action(4), Some(NodeAction::ScrollForward));
}
