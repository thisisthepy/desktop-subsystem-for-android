//! Each task window's Android UI as an AT-SPI tree: roles, names, states, places, actions,
//! the title bar's buttons, the events a change raises, and that a window nobody reads asks
//! the guest for nothing.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsa_host_core::accessibility::{
    ActionRequest, Node, NodeAction, NodeRange, NodeStates, TreeUpdate,
};
use dsa_host_core::tasks::Bounds;
use dsa_host_linux::accessibility::atspi::{coord, role, role_name, state};
use dsa_host_linux::accessibility::objects::{
    back_name, button_name, interface, prefers_korean, window_facts, Access, Event, Object,
    TitleAction, TitleButton, TitleCommand, WindowEvent, WindowFacts,
};
use dsa_host_linux::accessibility::text::{self, Unit};
use dsa_host_linux::accessibility::watch::{ApiConnect, Shared};
use dsa_host_linux::compositor::title_bar::TitleBar;
use dsa_host_linux::compositor::toplevel::Toplevel;
use dsa_transport::{Connection, Connector, TransportError};

const TASK: i32 = 7;

fn bounds(left: i32, top: i32, right: i32, bottom: i32) -> Bounds {
    Bounds {
        left,
        top,
        right,
        bottom,
    }
}

fn node(id: u32, children: &[u32], class_name: &str, text: &str) -> Node {
    Node {
        id,
        children: children.to_vec(),
        class_name: class_name.into(),
        text: text.into(),
        bounds: bounds(0, 0, 400, 600),
        states: NodeStates {
            enabled: true,
            ..NodeStates::default()
        },
        ..Node::default()
    }
}

fn row(id: u32, title: u32, top: i32) -> Node {
    Node {
        bounds: bounds(0, top, 400, top + 80),
        states: NodeStates {
            enabled: true,
            clickable: true,
            ..NodeStates::default()
        },
        actions: vec![NodeAction::Click],
        ..node(id, &[title], "android.widget.LinearLayout", "")
    }
}

fn title(id: u32, text: &str, top: i32) -> Node {
    Node {
        bounds: bounds(16, top + 10, 384, top + 40),
        ..node(id, &[], "android.widget.TextView", text)
    }
}

fn list(children: &[u32]) -> Node {
    Node {
        states: NodeStates {
            enabled: true,
            scrollable: true,
            ..NodeStates::default()
        },
        actions: vec![NodeAction::ScrollForward, NodeAction::ScrollBackward],
        ..node(2, children, "androidx.recyclerview.widget.RecyclerView", "")
    }
}

/// A Settings screen: a list with two clickable rows, each with a title.
fn settings_screen(epoch: u32) -> TreeUpdate {
    TreeUpdate {
        task_id: TASK,
        epoch,
        full: true,
        roots: vec![1],
        nodes: vec![
            node(1, &[2], "android.widget.FrameLayout", ""),
            list(&[3, 5]),
            row(3, 4, 0),
            title(4, "Network & internet", 0),
            row(5, 6, 80),
            title(6, "Connected devices", 80),
        ],
        removed: vec![],
        focused: None,
    }
}

fn change(epoch: u32, nodes: Vec<Node>) -> TreeUpdate {
    TreeUpdate {
        task_id: TASK,
        epoch,
        full: false,
        roots: vec![1],
        nodes,
        removed: vec![],
        focused: None,
    }
}

/// A Settings window, 400 x 600 pixels at `scale`, that shows the back arrow when `back`.
fn model(scale: u32) -> Toplevel {
    let mut model = Toplevel::new(TASK, "com.android.settings", "Settings", (400, 600));
    model.scale = scale;
    model
}

fn facts_of(model: &Toplevel, back: bool) -> WindowFacts {
    let mut bar = TitleBar::default();
    bar.set_back(back);
    window_facts(model, &bar)
}

fn settings(scale: u32) -> Access {
    let mut access = Access::new(false, Duration::from_secs(30));
    access.sync(TASK, facts_of(&model(scale), false));
    access.apply(TASK, settings_screen(1)).unwrap();
    access
}

fn at(epoch: u32, id: u32) -> Object {
    Object::Node {
        task_id: TASK,
        epoch,
        id,
    }
}

#[test]
fn fr18_roles_map_to_atspi_roles() {
    let cases = [
        (
            "android.widget.Button",
            NodeStates::default(),
            role::PUSH_BUTTON,
        ),
        (
            "android.widget.CheckBox",
            NodeStates::default(),
            role::CHECK_BOX,
        ),
        (
            "android.widget.Switch",
            NodeStates::default(),
            role::TOGGLE_BUTTON,
        ),
        (
            "android.widget.RadioButton",
            NodeStates::default(),
            role::RADIO_BUTTON,
        ),
        (
            "android.widget.TextView",
            NodeStates::default(),
            role::LABEL,
        ),
        (
            "android.widget.ImageView",
            NodeStates::default(),
            role::IMAGE,
        ),
        (
            "androidx.recyclerview.widget.RecyclerView",
            NodeStates::default(),
            role::LIST,
        ),
        (
            "android.widget.SeekBar",
            NodeStates::default(),
            role::SLIDER,
        ),
        (
            "android.widget.FrameLayout",
            NodeStates::default(),
            role::PANEL,
        ),
        (
            "android.widget.LinearLayout",
            NodeStates {
                clickable: true,
                ..NodeStates::default()
            },
            role::LIST_ITEM,
        ),
        (
            "android.widget.EditText",
            NodeStates {
                editable: true,
                ..NodeStates::default()
            },
            role::ENTRY,
        ),
        (
            "android.widget.EditText",
            NodeStates {
                editable: true,
                password: true,
                ..NodeStates::default()
            },
            role::PASSWORD_TEXT,
        ),
        (
            "android.widget.TextView",
            NodeStates {
                heading: true,
                ..NodeStates::default()
            },
            role::HEADING,
        ),
    ];
    for (class_name, states, expected) in cases {
        let node = Node {
            id: 1,
            class_name: class_name.into(),
            states,
            ..Node::default()
        };
        assert_eq!(
            dsa_host_linux::accessibility::atspi::role_of(&node),
            expected,
            "{class_name}"
        );
    }
    // AT-SPI's own numbers and names.
    assert_eq!(role::PUSH_BUTTON, 43);
    assert_eq!(role::LIST_ITEM, 32);
    assert_eq!(role::FRAME, 23);
    assert_eq!(role::APPLICATION, 75);
    assert_eq!(role_name(role::LIST_ITEM), "list item");
    assert_eq!(role_name(role::PUSH_BUTTON), "push button");
    assert_eq!(state::FOCUSED, 12);
    assert_eq!(state::CHECKABLE, 41);
}

#[test]
fn fr18_object_paths_name_their_window_and_epoch() {
    let objects = [
        Object::Application,
        Object::Window(TASK),
        Object::Button(TASK, TitleButton::Back),
        Object::Button(TASK, TitleButton::Close),
        at(3, 42),
        Object::Node {
            task_id: -1,
            epoch: 0,
            id: 1,
        },
    ];
    for object in objects {
        let path = object.path();
        assert!(
            path.split('/').skip(1).all(|part| !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')),
            "{path} is a D-Bus object path"
        );
        assert_eq!(Object::from_path(&path), Some(object), "{path}");
    }
    assert_eq!(
        Object::Application.path(),
        "/org/a11y/atspi/accessible/root"
    );
    assert_eq!(
        Object::from_path("/org/a11y/atspi/accessible/dsa/t7/e1"),
        None
    );
    assert_eq!(
        Object::from_path("/org/a11y/atspi/accessible/dsa/t7/nothing"),
        None
    );
    assert_eq!(Object::from_path("/elsewhere"), None);
}

#[test]
fn fr18_a_settings_screen_reads_as_list_items_with_names_and_places() {
    let access = settings(1);
    let frame = Object::Window(TASK);
    assert_eq!(access.children(Object::Application), vec![frame]);
    assert_eq!(access.role(frame), role::FRAME);
    assert_eq!(access.name(frame), "Settings");
    assert_eq!(
        access.children(frame),
        vec![
            Object::Button(TASK, TitleButton::Minimize),
            Object::Button(TASK, TitleButton::Maximize),
            Object::Button(TASK, TitleButton::Close),
            at(1, 1),
        ]
    );
    assert_eq!(access.children(at(1, 2)), vec![at(1, 3), at(1, 5)]);
    assert_eq!(access.role(at(1, 2)), role::LIST);
    assert_eq!(access.parent(at(1, 1)), Some(frame));
    assert_eq!(access.parent(at(1, 5)), Some(at(1, 2)));
    assert_eq!(access.index_in_parent(at(1, 5)), 1);
    assert_eq!(access.index_in_parent(at(1, 1)), 3);
    let first = at(1, 3);
    assert_eq!(access.role(first), role::LIST_ITEM);
    assert_eq!(access.name(first), "Network & internet");
    assert_eq!(access.name(at(1, 5)), "Connected devices");
    assert_eq!(access.extents(first, coord::SCREEN), Some((0, 0, 400, 80)));
    assert_eq!(
        access.extents(at(1, 5), coord::WINDOW),
        Some((0, 80, 400, 80))
    );
    // From the row's top left, its title is 16 in and 10 down.
    assert_eq!(
        access.extents(at(1, 4), coord::PARENT),
        Some((16, 10, 368, 30))
    );
    let states = access.states(first);
    for expected in [
        state::ENABLED,
        state::SENSITIVE,
        state::VISIBLE,
        state::SHOWING,
    ] {
        assert!(states.contains(expected), "state {expected}");
    }
    assert!(!states.contains(state::FOCUSED));
    assert!(access.interfaces(first).contains(&interface::ACTION));
    assert!(access.interfaces(first).contains(&interface::COMPONENT));
    assert_eq!(access.text(at(1, 4)).as_deref(), Some("Network & internet"));
    assert!(access.interfaces(at(1, 4)).contains(&interface::TEXT));
    // A node of an earlier tree is gone, never another node.
    assert!(!access.exists(at(0, 3)));
    assert!(access.states(at(0, 3)).contains(state::DEFUNCT));
    assert_eq!(access.name(at(0, 3)), "");
}

#[test]
fn fr18_places_are_in_the_windows_logical_pixels() {
    let access = settings(2);
    let frame = Object::Window(TASK);
    assert_eq!(access.extents(frame, coord::SCREEN), Some((0, 0, 200, 300)));
    assert_eq!(
        access.extents(at(1, 3), coord::SCREEN),
        Some((0, 0, 200, 40))
    );
    assert_eq!(
        access.extents(at(1, 4), coord::WINDOW),
        Some((8, 5, 184, 15))
    );
    // A point on the second row's title finds it; one on Close finds the button.
    assert_eq!(
        access.at_point(frame, 10, 50, coord::WINDOW),
        Some(at(1, 6))
    );
    let close = access
        .extents(Object::Button(TASK, TitleButton::Close), coord::WINDOW)
        .unwrap();
    assert_eq!(
        access.at_point(frame, close.0 + 2, close.1 + 2, coord::WINDOW),
        Some(Object::Button(TASK, TitleButton::Close))
    );
    // Inside a row only its own nodes are found.
    assert_eq!(
        access.at_point(at(1, 5), 10, 50, coord::WINDOW),
        Some(at(1, 6))
    );
    assert_eq!(access.at_point(at(1, 3), 10, 50, coord::WINDOW), None);
    assert!(access.contains(at(1, 5), 10, 50, coord::SCREEN));
    assert!(!access.contains(at(1, 3), 10, 50, coord::SCREEN));
}

/// Settings kept at the density of a monitor at 100% (130 dpi) and shown on one at 125%
/// (164 dpi): its window shows each of the task's 400 by 600 pixels at 164 per 130.
fn settings_at_125_percent_kept_at_100() -> (Toplevel, Access) {
    use dsa_host_core::compositor::window_sizing::{
        density_for_dpi, scale_between, to_window_size,
    };
    let mut model = model(1);
    model.frame_scale = scale_between(density_for_dpi(120), density_for_dpi(96));
    model.shown = to_window_size(model.task, model.frame_scale);
    let mut access = Access::new(false, Duration::from_secs(30));
    access.sync(TASK, facts_of(&model, false));
    access.apply(TASK, settings_screen(1)).unwrap();
    (model, access)
}

#[test]
fn fr18_on_a_monitor_of_another_scale_places_are_where_the_window_shows_each_node() {
    let (model, access) = settings_at_125_percent_kept_at_100();
    assert_eq!(model.frame_scale, (82, 65));
    assert_eq!(model.shown, (505, 757));
    // The second row is 80 task pixels down and 80 high: 100.9 to 201.8 window pixels, grown
    // to whole pixels.
    assert_eq!(
        access.extents(at(1, 5), coord::WINDOW),
        Some((0, 100, 505, 102))
    );
    // Its title, 16 in and 90 down in the task, is 20.2 in and 113.5 down in the window.
    assert_eq!(
        access.extents(at(1, 6), coord::WINDOW),
        Some((20, 113, 465, 39))
    );
    // A point on the title as the window shows it finds the title.
    assert_eq!(
        access.at_point(Object::Window(TASK), 30, 120, coord::WINDOW),
        Some(at(1, 6))
    );
    // 101 window pixels down is 80.5 task pixels down: the second row above its title,
    // where at 1:1 it would have been on the title.
    assert_eq!(
        access.at_point(Object::Window(TASK), 30, 101, coord::WINDOW),
        Some(at(1, 5))
    );
    // 95 window pixels down is 75.7 task pixels down: still the first row, whose bottom the
    // task has at 80, where at 1:1 it would have been the second row's title.
    assert_eq!(
        access.at_point(Object::Window(TASK), 30, 95, coord::WINDOW),
        Some(at(1, 3))
    );
}

#[test]
fn fr18_a_new_scale_moves_every_node_on_the_screen() {
    let (mut model, mut access) = settings_at_125_percent_kept_at_100();
    // Back on a 100% monitor, the window shows the task 1:1 again.
    model.frame_scale = (1, 1);
    model.shown = model.task;
    let events = access.sync(TASK, facts_of(&model, false));
    let moved: Vec<(Object, (i32, i32, i32, i32))> = events
        .iter()
        .filter_map(|event| match event {
            Event::Bounds { object, rect } if matches!(object, Object::Node { .. }) => {
                Some((*object, *rect))
            }
            _ => None,
        })
        .collect();
    for id in 1..=6 {
        assert!(
            moved.iter().any(|(object, _)| *object == at(1, id)),
            "node {id} was not said to move: {moved:?}"
        );
    }
    assert!(moved.contains(&(at(1, 5), (0, 80, 400, 80))));
}

#[test]
fn fr18_actions_become_the_nodes_accessibility_actions() {
    let mut access = settings(1);
    let (actions, requests) = mpsc::channel();
    access.attach_actions(TASK, actions);
    // A row's default action presses it, as a tap would.
    let offered = access.actions(at(1, 3));
    assert_eq!(offered[0].name, "click");
    assert_eq!(access.perform(at(1, 3), 0), Ok(None));
    assert_eq!(
        requests.try_recv(),
        Ok(ActionRequest {
            epoch: 1,
            node_id: 3,
            action: NodeAction::Click,
            text: String::new(),
            request_id: 1,
        })
    );
    // The list scrolls forward and backward.
    let names: Vec<&str> = access.actions(at(1, 2)).iter().map(|a| a.name).collect();
    assert_eq!(names, ["scroll forward", "scroll backward"]);
    access.perform(at(1, 2), 1).unwrap();
    let scrolled = requests.try_recv().unwrap();
    assert_eq!(
        (scrolled.node_id, scrolled.action),
        (2, NodeAction::ScrollBackward)
    );
    // A label offers nothing; a node of an earlier tree is refused.
    assert!(access.actions(at(1, 4)).is_empty());
    assert!(access.perform(at(1, 4), 0).is_err());
    assert!(access.perform(at(0, 3), 0).is_err());
    // Focus: the accessibility focus where the node takes no input focus.
    assert_eq!(
        access.focus_action(at(1, 3)),
        Some(NodeAction::AccessibilityFocus)
    );
    access
        .request(at(1, 3), NodeAction::AccessibilityFocus, String::new())
        .unwrap();
    assert_eq!(
        requests.try_recv().unwrap().action,
        NodeAction::AccessibilityFocus
    );
    assert!(requests.try_recv().is_err());
}

#[test]
fn fr18_fields_take_new_text_and_sliders_new_values() {
    let mut access = Access::new(false, Duration::from_secs(30));
    access.sync(TASK, facts_of(&model(1), false));
    let field = Node {
        states: NodeStates {
            enabled: true,
            editable: true,
            focusable: true,
            ..NodeStates::default()
        },
        actions: vec![NodeAction::SetText, NodeAction::Focus],
        hint: "Search settings".into(),
        ..node(2, &[], "android.widget.EditText", "wi")
    };
    let slider = Node {
        range: Some(NodeRange {
            min: 0.0,
            max: 100.0,
            current: 40.0,
        }),
        actions: vec![NodeAction::ScrollForward, NodeAction::ScrollBackward],
        state_description: "40 percent".into(),
        ..node(3, &[], "android.widget.SeekBar", "")
    };
    access
        .apply(
            TASK,
            TreeUpdate {
                task_id: TASK,
                epoch: 1,
                full: true,
                roots: vec![1],
                nodes: vec![
                    node(1, &[2, 3], "android.widget.LinearLayout", ""),
                    field,
                    slider,
                ],
                removed: vec![],
                focused: Some(2),
            },
        )
        .unwrap();
    let (actions, requests) = mpsc::channel();
    access.attach_actions(TASK, actions);
    let field = at(1, 2);
    assert_eq!(access.role(field), role::ENTRY);
    assert!(access.interfaces(field).contains(&interface::EDITABLE_TEXT));
    assert!(access.states(field).contains(state::EDITABLE));
    assert!(access.states(field).contains(state::FOCUSED));
    assert_eq!(access.text(field).as_deref(), Some("wi"));
    assert_eq!(access.help_text(field), "Search settings");
    assert_eq!(access.focus_action(field), Some(NodeAction::Focus));
    access
        .request(field, NodeAction::SetText, text::insert("wi", 2, "-fi"))
        .unwrap();
    let typed = requests.try_recv().unwrap();
    assert_eq!(
        (typed.action, typed.text.as_str()),
        (NodeAction::SetText, "wi-fi")
    );
    // A slider moves a step toward the value asked for.
    let slider = at(1, 3);
    assert_eq!(access.role(slider), role::SLIDER);
    assert!(access.interfaces(slider).contains(&interface::VALUE));
    assert_eq!(access.range(slider).map(|r| r.current), Some(40.0));
    assert_eq!(access.description(slider), "40 percent");
    assert_eq!(
        access.value_action(slider, 60.0),
        Some(NodeAction::ScrollForward)
    );
    assert_eq!(
        access.value_action(slider, 10.0),
        Some(NodeAction::ScrollBackward)
    );
    assert_eq!(access.value_action(slider, 40.0), None);
}

#[test]
fn fr18_changes_raise_children_name_state_and_bounds_events() {
    let mut access = settings(1);
    // The first row's title changes: the title and the row it names are renamed.
    let events = access
        .apply(TASK, change(1, vec![title(4, "Internet", 0)]))
        .unwrap();
    assert!(events.contains(&Event::Name {
        object: at(1, 4),
        name: "Internet".into()
    }));
    assert!(events.contains(&Event::Name {
        object: at(1, 3),
        name: "Internet".into()
    }));
    // A third row comes into the list.
    let events = access
        .apply(
            TASK,
            change(
                1,
                vec![list(&[3, 5, 8]), row(8, 9, 160), title(9, "Apps", 160)],
            ),
        )
        .unwrap();
    assert!(events.contains(&Event::ChildAdded {
        parent: at(1, 2),
        index: 2,
        child: at(1, 8),
    }));
    // It moves and is turned off.
    let mut moved = row(8, 9, 240);
    moved.states.enabled = false;
    let events = access.apply(TASK, change(1, vec![moved])).unwrap();
    assert!(events.contains(&Event::Bounds {
        object: at(1, 8),
        rect: (0, 240, 400, 80),
    }));
    assert!(events.contains(&Event::State {
        object: at(1, 8),
        state: "enabled",
        on: false,
    }));
    // It goes again.
    let mut gone = change(1, vec![list(&[3, 5])]);
    gone.removed = vec![8, 9];
    let events = access.apply(TASK, gone).unwrap();
    assert!(events.contains(&Event::ChildRemoved {
        parent: at(1, 2),
        index: 2,
        child: at(1, 8),
    }));
    // A new screen replaces the whole tree, after the title bar's three buttons.
    let events = access.apply(TASK, settings_screen(2)).unwrap();
    assert_eq!(
        &events[..2],
        &[
            Event::ChildRemoved {
                parent: Object::Window(TASK),
                index: 3,
                child: at(1, 1),
            },
            Event::ChildAdded {
                parent: Object::Window(TASK),
                index: 3,
                child: at(2, 1),
            },
        ]
    );
    // When Android stops reading the task, the nodes leave the window.
    assert_eq!(
        access.clear(TASK),
        vec![Event::ChildRemoved {
            parent: Object::Window(TASK),
            index: 3,
            child: at(2, 1),
        }]
    );
    assert_eq!(access.children(Object::Window(TASK)).len(), 3);
}

#[test]
fn fr18_typing_raises_text_events_and_focus_follows_the_active_window() {
    let mut access = Access::new(false, Duration::from_secs(30));
    let mut window = model(1);
    window.states.activated = true;
    let events = access.sync(TASK, facts_of(&window, false));
    assert_eq!(
        events,
        vec![
            Event::ChildAdded {
                parent: Object::Application,
                index: 0,
                child: Object::Window(TASK),
            },
            Event::Window {
                task_id: TASK,
                kind: WindowEvent::Create,
                title: "Settings".into(),
            },
            Event::Window {
                task_id: TASK,
                kind: WindowEvent::Activate,
                title: "Settings".into(),
            },
            Event::State {
                object: Object::Window(TASK),
                state: "active",
                on: true,
            },
        ]
    );
    let field = |text: &str| Node {
        states: NodeStates {
            enabled: true,
            editable: true,
            focusable: true,
            ..NodeStates::default()
        },
        actions: vec![NodeAction::SetText],
        ..node(1, &[], "android.widget.EditText", text)
    };
    let mut first = TreeUpdate {
        task_id: TASK,
        epoch: 1,
        full: true,
        roots: vec![1],
        nodes: vec![field("wi")],
        removed: vec![],
        focused: Some(1),
    };
    let events = access.apply(TASK, first.clone()).unwrap();
    assert!(events.contains(&Event::State {
        object: at(1, 1),
        state: "focused",
        on: true,
    }));
    first.full = false;
    first.nodes = vec![field("wifi")];
    let events = access.apply(TASK, first).unwrap();
    assert!(events.contains(&Event::TextInserted {
        object: at(1, 1),
        start: 2,
        text: "fi".into(),
    }));
    // Another window becomes active, then this one again: the focus is told again.
    window.states.activated = false;
    let events = access.sync(TASK, facts_of(&window, false));
    assert!(events.contains(&Event::Window {
        task_id: TASK,
        kind: WindowEvent::Deactivate,
        title: "Settings".into(),
    }));
    window.states.activated = true;
    let events = access.sync(TASK, facts_of(&window, false));
    assert!(events.contains(&Event::State {
        object: at(1, 1),
        state: "focused",
        on: true,
    }));
    // The window closes.
    let events = access.close(TASK);
    assert_eq!(
        events[0],
        Event::ChildRemoved {
            parent: Object::Application,
            index: 0,
            child: Object::Window(TASK),
        }
    );
    assert!(access.children(Object::Application).is_empty());
}

#[test]
fn fr18_title_bar_buttons_are_named_push_buttons_that_click() {
    let mut access = Access::new(false, Duration::from_secs(30));
    let mut window = model(1);
    access.sync(TASK, facts_of(&window, false));
    let frame = Object::Window(TASK);
    let minimize = Object::Button(TASK, TitleButton::Minimize);
    let maximize = Object::Button(TASK, TitleButton::Maximize);
    let close = Object::Button(TASK, TitleButton::Close);
    let back = Object::Button(TASK, TitleButton::Back);
    for (button, name) in [
        (minimize, "Minimize"),
        (maximize, "Maximize"),
        (close, "Close"),
    ] {
        assert_eq!(access.role(button), role::PUSH_BUTTON);
        assert_eq!(access.name(button), name);
        assert!(access.interfaces(button).contains(&interface::ACTION));
    }
    // Each sits where the title bar draws it.
    let layout = window.caption_layout();
    let (left, top, width, height) =
        layout.button_rect(dsa_host_core::compositor::caption::CaptionButton::Close);
    assert_eq!(
        access.extents(close, coord::WINDOW),
        Some((left, top, width as i32, height as i32))
    );
    assert_eq!(
        access.perform(close, 0),
        Ok(Some(TitleCommand {
            task_id: TASK,
            action: TitleAction::Close,
        }))
    );
    assert!(!access.exists(back));
    // The task can go back: Back comes first, where the icon's cell is.
    let events = access.sync(TASK, facts_of(&window, true));
    assert_eq!(
        events,
        vec![Event::ChildAdded {
            parent: frame,
            index: 0,
            child: back,
        }]
    );
    assert_eq!(access.children(frame)[0], back);
    assert_eq!(access.name(back), "Back");
    let (left, top, width, height) = layout.icon_cell().unwrap();
    assert_eq!(
        access.extents(back, coord::WINDOW),
        Some((left, top, width as i32, height as i32))
    );
    assert_eq!(
        access.perform(back, 0),
        Ok(Some(TitleCommand {
            task_id: TASK,
            action: TitleAction::Back,
        }))
    );
    // A maximized window's maximize button restores it.
    window.states.maximized = true;
    let events = access.sync(TASK, facts_of(&window, true));
    assert!(events.contains(&Event::Name {
        object: maximize,
        name: "Restore".into(),
    }));
    assert_eq!(
        access.perform(maximize, 0),
        Ok(Some(TitleCommand {
            task_id: TASK,
            action: TitleAction::Restore,
        }))
    );
    // At the first screen again, Back goes.
    let events = access.sync(TASK, facts_of(&window, false));
    assert!(events.contains(&Event::ChildRemoved {
        parent: frame,
        index: 0,
        child: back,
    }));
    // A full screen window draws no title bar, so it has no buttons.
    let mut facts = facts_of(&window, false);
    facts.caption = None;
    access.sync(TASK, facts);
    assert!(access.children(frame).is_empty());
}

#[test]
fn fr22_the_buttons_are_named_in_the_first_preferred_language() {
    assert_eq!(back_name(true), "뒤로");
    assert_eq!(back_name(false), "Back");
    assert_eq!(button_name(TitleButton::Close, false, true), "닫기");
    assert_eq!(button_name(TitleButton::Maximize, true, false), "Restore");
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    };
    // LANGUAGE lists the languages by preference; its first decides.
    assert!(prefers_korean(env(&[
        ("LANGUAGE", "ko:en"),
        ("LANG", "en_US.UTF-8")
    ])));
    assert!(!prefers_korean(env(&[
        ("LANGUAGE", "en_US:ko"),
        ("LANG", "ko_KR.UTF-8")
    ])));
    // Without it, the locale does.
    assert!(prefers_korean(env(&[("LANG", "ko_KR.UTF-8")])));
    assert!(!prefers_korean(env(&[("LANG", "en_US.UTF-8")])));
    let mut korean = Access::new(true, Duration::from_secs(30));
    korean.sync(TASK, facts_of(&model(1), true));
    assert_eq!(korean.name(Object::Button(TASK, TitleButton::Back)), "뒤로");
    assert_eq!(
        korean.name(Object::Button(TASK, TitleButton::Minimize)),
        "최소화"
    );
}

#[test]
fn fr18_text_reads_by_character_word_and_line() {
    let line = "Wi-Fi is on. Tap to change";
    assert_eq!(text::length(line), 26);
    assert_eq!(text::slice(line, 0, 5), "Wi-Fi");
    assert_eq!(text::slice(line, 13, -1), "Tap to change");
    assert_eq!(text::character_at(line, 0), 'W' as i32);
    assert_eq!(text::unit_at(line, 1, Unit::Char), ("i".into(), 1, 2));
    assert_eq!(text::unit_at(line, 2, Unit::Word), ("Wi-Fi ".into(), 0, 6));
    assert_eq!(
        text::unit_at(line, 3, Unit::Sentence),
        ("Wi-Fi is on. ".into(), 0, 13)
    );
    assert_eq!(text::unit_after(line, 0, Unit::Word), ("is ".into(), 6, 9));
    assert_eq!(
        text::unit_before(line, 7, Unit::Word),
        ("Wi-Fi ".into(), 0, 6)
    );
    assert_eq!(
        text::unit_at("one\ntwo", 5, Unit::Line),
        ("two".into(), 4, 7)
    );
    assert_eq!(text::unit_at(line, 99, Unit::Word), (String::new(), 26, 26));
    // Korean counts characters, not bytes.
    assert_eq!(text::length("설정"), 2);
    assert_eq!(
        text::unit_at("설정 앱", 1, Unit::Word),
        ("설정 ".into(), 0, 3)
    );
    assert_eq!(
        text::difference("wi", "wifi"),
        Some((2, String::new(), "fi".into()))
    );
    assert_eq!(text::difference("wifi", "wifi"), None);
    assert_eq!(text::delete("wi-fi", 2, 3), "wifi");
    assert_eq!(Unit::from_granularity(1), Unit::Word);
    assert_eq!(Unit::from_boundary(5), Unit::Line);
}

/// A service that is never there, which counts how often it is dialled.
struct Counting(Arc<AtomicUsize>);

impl Connector for Counting {
    fn connect(&mut self, _: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(TransportError::Closed)
    }
}

fn wait_until(limit: Duration, done: impl Fn() -> bool) -> bool {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

#[test]
fn fr18_no_client_no_requests() {
    let dials = Arc::new(AtomicUsize::new(0));
    let connect: ApiConnect = {
        let dials = Arc::clone(&dials);
        Arc::new(move || Box::new(Counting(Arc::clone(&dials))) as Box<dyn Connector>)
    };
    let shared = Shared::new(
        Access::new(false, Duration::from_millis(300)),
        Some(connect),
        Arc::new(|_: TitleCommand| {}),
        Arc::new(|_: &str| {}),
    );
    shared.sync(TASK, facts_of(&model(1), false));
    // A window switcher reads the windows' names and roles: that asks for no tree.
    assert_eq!(
        Access::wants_tree(Object::Application, "GetChildren", None),
        None
    );
    assert_eq!(
        Access::wants_tree(Object::Window(TASK), "Get", Some("Name")),
        None
    );
    assert_eq!(
        Access::wants_tree(Object::Window(TASK), "GetRole", None),
        None
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(dials.load(Ordering::SeqCst), 0, "nobody read the window");
    // A client reads into the window: the service is asked for the task's tree.
    let task = Access::wants_tree(Object::Window(TASK), "GetChildren", None).unwrap();
    shared.asked(task);
    assert!(wait_until(Duration::from_secs(3), || dials
        .load(Ordering::SeqCst)
        >= 1));
    // Nobody asks again and nobody listens: after the grace the watch ends and stops
    // dialling.
    std::thread::sleep(Duration::from_millis(1500));
    let settled = dials.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(dials.load(Ordering::SeqCst), settled, "the watch ended");
    // A window with no service to dial never watches.
    let none = Shared::new(
        Access::new(false, Duration::from_millis(300)),
        None,
        Arc::new(|_: TitleCommand| {}),
        Arc::new(|_: &str| {}),
    );
    none.sync(TASK, facts_of(&model(1), false));
    none.asked(TASK);
}

#[test]
fn fr18_a_listening_client_keeps_the_tree() {
    let dials = Arc::new(AtomicUsize::new(0));
    let connect: ApiConnect = {
        let dials = Arc::clone(&dials);
        Arc::new(move || Box::new(Counting(Arc::clone(&dials))) as Box<dyn Connector>)
    };
    let shared = Shared::new(
        Access::new(false, Duration::from_millis(200)),
        Some(connect),
        Arc::new(|_: TitleCommand| {}),
        Arc::new(|_: &str| {}),
    );
    shared.sync(TASK, facts_of(&model(1), false));
    shared.set_listeners(true);
    assert!(shared.listening());
    shared.asked(TASK);
    // Past the grace the watch still runs, so it dials again after its retry.
    std::thread::sleep(Duration::from_millis(2500));
    assert!(dials.load(Ordering::SeqCst) >= 2, "the watch kept trying");
    // Accessibility goes off: every watch ends.
    shared.set_listeners(false);
    shared.end_watches();
    std::thread::sleep(Duration::from_millis(300));
    let settled = dials.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(dials.load(Ordering::SeqCst), settled);
}
