//! The accessibility tree of a task window: updates, roles, bounds and when the guest is
//! asked for it.

use std::time::{Duration, Instant};

use dsa_host_core::accessibility::{
    from_screen, role_of, to_screen, ActionRequest, ActionResult, ClientPresence, Node, NodeAction,
    NodeRange, NodeStates, PresenceChange, Role, Tree, TreeError, TreeUpdate, MAX_NODES,
    MAX_TEXT_CHARS,
};
use dsa_host_core::compositor::window_sizing::{density_for_dpi, scale_between, to_window_size};
use dsa_host_core::input::WindowGeometry;
use dsa_host_core::tasks::Bounds;
use dsa_protocol::control;

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

/// A Settings screen: a list with two clickable rows, each with a title.
fn settings_screen(epoch: u32) -> TreeUpdate {
    let row = |id: u32, title: u32, top: i32| Node {
        bounds: bounds(0, top, 400, top + 80),
        states: NodeStates {
            enabled: true,
            clickable: true,
            ..NodeStates::default()
        },
        actions: vec![NodeAction::Click],
        ..node(id, &[title], "android.widget.LinearLayout", "")
    };
    let title = |id: u32, text: &str, top: i32| Node {
        bounds: bounds(16, top + 10, 384, top + 40),
        ..node(id, &[], "android.widget.TextView", text)
    };
    TreeUpdate {
        task_id: 7,
        epoch,
        full: true,
        roots: vec![1],
        nodes: vec![
            node(1, &[2], "android.widget.FrameLayout", ""),
            Node {
                states: NodeStates {
                    enabled: true,
                    scrollable: true,
                    ..NodeStates::default()
                },
                ..node(2, &[3, 5], "androidx.recyclerview.widget.RecyclerView", "")
            },
            row(3, 4, 0),
            title(4, "Network & internet", 0),
            row(5, 6, 80),
            title(6, "Connected devices", 80),
        ],
        removed: vec![],
        focused: None,
    }
}

fn change(epoch: u32, nodes: Vec<Node>, removed: Vec<u32>, roots: Vec<u32>) -> TreeUpdate {
    TreeUpdate {
        task_id: 7,
        epoch,
        full: false,
        roots,
        nodes,
        removed,
        focused: None,
    }
}

#[test]
fn fr18_a_full_tree_gives_list_items_with_names_and_roles() {
    let mut tree = Tree::new();
    let changes = tree.apply(settings_screen(1)).unwrap();
    assert!(changes.replaced);
    assert_eq!(tree.len(), 6);
    assert_eq!(tree.roots(), &[1]);
    assert_eq!(tree.node(3).unwrap().role(), Role::ListItem);
    assert_eq!(tree.node(2).unwrap().role(), Role::List);
    assert_eq!(tree.name(3), "Network & internet");
    assert_eq!(tree.name(5), "Connected devices");
    assert_eq!(tree.parent(4), Some(3));
    assert_eq!(tree.parent(1), None);
    assert_eq!(tree.sibling(3, true), Some(5));
    assert_eq!(tree.sibling(5, true), None);
    assert_eq!(tree.sibling(5, false), Some(3));
}

#[test]
fn fr18_a_change_replaces_changed_nodes_and_removes_gone_ones() {
    let mut tree = Tree::new();
    tree.apply(settings_screen(1)).unwrap();
    let renamed = node(4, &[], "android.widget.TextView", "Wi-Fi");
    let list = Node {
        states: NodeStates {
            enabled: true,
            scrollable: true,
            ..NodeStates::default()
        },
        ..node(2, &[3], "androidx.recyclerview.widget.RecyclerView", "")
    };
    let changes = tree
        .apply(change(1, vec![renamed, list], vec![5, 6], vec![1]))
        .unwrap();
    assert!(!changes.replaced);
    assert_eq!(changes.children_changed, vec![2]);
    assert_eq!(changes.changed.len(), 1);
    assert_eq!(changes.changed[0].text, "Network & internet");
    let mut removed = changes.removed.clone();
    removed.sort();
    assert_eq!(removed, vec![5, 6]);
    assert_eq!(tree.len(), 4);
    assert_eq!(tree.name(3), "Wi-Fi");
}

#[test]
fn fr18_a_change_for_another_epoch_is_refused() {
    let mut tree = Tree::new();
    assert_eq!(
        tree.apply(change(1, vec![], vec![], vec![])),
        Err(TreeError::Epoch {
            tree: None,
            update: 1
        })
    );
    tree.apply(settings_screen(2)).unwrap();
    assert!(matches!(
        tree.apply(change(1, vec![], vec![], vec![1])),
        Err(TreeError::Epoch { .. })
    ));
    assert_eq!(tree.len(), 6);
}

#[test]
fn fr18_a_broken_change_leaves_the_tree_as_it_was() {
    let mut tree = Tree::new();
    tree.apply(settings_screen(1)).unwrap();
    // A child that is not in the tree.
    let dangling = node(3, &[99], "android.widget.LinearLayout", "");
    assert!(matches!(
        tree.apply(change(1, vec![dangling], vec![], vec![1])),
        Err(TreeError::Malformed(_))
    ));
    // A cycle.
    let cycle = node(4, &[1], "android.widget.TextView", "");
    assert!(matches!(
        tree.apply(change(1, vec![cycle], vec![], vec![1])),
        Err(TreeError::Malformed(_))
    ));
    // A node no window holds.
    let orphan = node(50, &[], "android.widget.TextView", "lost");
    assert!(matches!(
        tree.apply(change(1, vec![orphan], vec![], vec![1])),
        Err(TreeError::Malformed(_))
    ));
    // Node 0.
    assert!(matches!(
        tree.apply(change(1, vec![node(0, &[], "", "")], vec![], vec![1])),
        Err(TreeError::Malformed(_))
    ));
    // A focused node that is not there.
    let mut focus = change(1, vec![], vec![], vec![1]);
    focus.focused = Some(77);
    assert!(matches!(tree.apply(focus), Err(TreeError::Malformed(_))));
    assert_eq!(tree.len(), 6);
    assert_eq!(tree.name(3), "Network & internet");
}

#[test]
fn fr18_a_tree_over_the_node_cap_is_dropped() {
    let count = MAX_NODES as u32 + 1;
    let children: Vec<u32> = (2..=count).collect();
    let mut nodes = vec![node(1, &children, "android.widget.FrameLayout", "")];
    nodes.extend((2..=count).map(|id| node(id, &[], "android.widget.TextView", "x")));
    let update = TreeUpdate {
        task_id: 7,
        epoch: 1,
        full: true,
        roots: vec![1],
        nodes,
        removed: vec![],
        focused: None,
    };
    let mut tree = Tree::new();
    assert!(matches!(tree.apply(update), Err(TreeError::Malformed(_))));
    assert!(tree.is_empty());
}

#[test]
fn fr18_an_oversized_tree_from_the_guest_is_refused_and_long_text_cut() {
    let many = control::AccessibilityTree {
        task_id: 7,
        epoch: 1,
        full: true,
        nodes: (1..=MAX_NODES as u32 + 1)
            .map(|id| control::AccessibilityNode {
                id,
                ..control::AccessibilityNode::default()
            })
            .collect(),
        ..control::AccessibilityTree::default()
    };
    assert!(TreeUpdate::from_control(many).is_err());
    let long = control::AccessibilityTree {
        task_id: 7,
        epoch: 1,
        full: true,
        root_ids: vec![1],
        nodes: vec![control::AccessibilityNode {
            id: 1,
            text: "a".repeat(MAX_TEXT_CHARS * 3),
            actions: vec![1, 99],
            ..control::AccessibilityNode::default()
        }],
        ..control::AccessibilityTree::default()
    };
    let update = TreeUpdate::from_control(long).unwrap();
    assert_eq!(update.nodes[0].text.chars().count(), MAX_TEXT_CHARS);
    // An action this build does not know is left out, not an error.
    assert_eq!(update.nodes[0].actions, vec![NodeAction::Click]);
}

#[test]
fn fr18_the_tree_crosses_the_local_api_unchanged() {
    let mut update = settings_screen(3);
    update.nodes[3].range = Some(NodeRange {
        min: 0.0,
        max: 10.0,
        current: 4.0,
    });
    update.focused = Some(4);
    let back = TreeUpdate::from_host_api(update.to_host_api()).unwrap();
    assert_eq!(back, update);
    let request = ActionRequest {
        epoch: 3,
        node_id: 5,
        action: NodeAction::SetText,
        text: "hello".into(),
        request_id: 9,
    };
    assert_eq!(
        ActionRequest::from_host_api(request.to_host_api()).unwrap(),
        request
    );
    let control = request.to_control(7);
    assert_eq!(control.task_id, 7);
    assert_eq!(control.action, control::NodeAction::SetText as i32);
    let refused = ActionResult {
        request_id: 9,
        outcome: Err("the node is gone".into()),
    };
    assert_eq!(ActionResult::from_host_api(refused.to_host_api()), refused);
}

#[test]
fn fr18_focus_and_hits_follow_the_tree() {
    let mut tree = Tree::new();
    let mut screen = settings_screen(1);
    screen.focused = Some(3);
    let changes = tree.apply(screen).unwrap();
    assert_eq!(changes.focus, Some(3));
    let mut moved = change(1, vec![], vec![], vec![1]);
    moved.focused = Some(5);
    assert_eq!(tree.apply(moved).unwrap().focus, Some(5));
    // The second row's title, inside the second row.
    assert_eq!(tree.hit(100, 100), Some(6));
    assert_eq!(tree.hit(390, 150), Some(5));
    assert_eq!(tree.hit(500, 10), None);
    let cleared = tree.clear();
    assert!(cleared.replaced);
    assert!(tree.is_empty());
}

#[test]
fn fr18_roles_follow_the_android_class_and_states() {
    let plain = NodeStates::default();
    let clickable = NodeStates {
        clickable: true,
        ..plain
    };
    let cases = [
        ("android.widget.Button", plain, Role::Button),
        (
            "com.google.android.material.button.MaterialButton",
            plain,
            Role::Button,
        ),
        ("android.widget.CheckBox", plain, Role::CheckBox),
        (
            "androidx.appcompat.widget.AppCompatCheckBox",
            plain,
            Role::CheckBox,
        ),
        ("android.widget.Switch", plain, Role::Switch),
        (
            "com.google.android.material.materialswitch.MaterialSwitch",
            plain,
            Role::Switch,
        ),
        ("android.widget.RadioButton", plain, Role::RadioButton),
        ("android.widget.EditText", plain, Role::Edit),
        ("android.widget.TextView", plain, Role::Text),
        ("android.widget.TextView", clickable, Role::Button),
        ("android.widget.ImageView", plain, Role::Image),
        ("android.widget.ImageButton", plain, Role::Button),
        ("android.widget.SeekBar", plain, Role::Slider),
        ("android.widget.ProgressBar", plain, Role::ProgressBar),
        ("android.widget.Spinner", plain, Role::ComboBox),
        ("android.widget.ListView", plain, Role::List),
        (
            "androidx.recyclerview.widget.RecyclerView",
            plain,
            Role::List,
        ),
        ("android.widget.GridView", plain, Role::Grid),
        ("android.widget.ScrollView", plain, Role::ScrollView),
        ("android.widget.LinearLayout", plain, Role::Group),
        ("android.widget.LinearLayout", clickable, Role::ListItem),
        ("android.webkit.WebView", plain, Role::WebView),
        ("android.widget.Toolbar", plain, Role::Toolbar),
        ("", plain, Role::Unknown),
        ("", clickable, Role::Button),
    ];
    for (class_name, states, role) in cases {
        assert_eq!(role_of(class_name, &states, false), role, "{class_name}");
    }
    let heading = NodeStates {
        heading: true,
        ..plain
    };
    assert_eq!(
        role_of("android.widget.TextView", &heading, false),
        Role::Heading
    );
    let editable = NodeStates {
        editable: true,
        ..plain
    };
    assert_eq!(role_of("com.example.Field", &editable, false), Role::Edit);
}

#[test]
fn fr18_names_values_and_passwords() {
    let mut edit = node(1, &[], "android.widget.EditText", "secret");
    edit.states.editable = true;
    edit.hint = "Password".into();
    assert_eq!(edit.name(), "Password");
    assert_eq!(edit.value().as_deref(), Some("secret"));
    edit.states.password = true;
    assert_eq!(edit.value().unwrap().chars().count(), 6);
    assert!(!edit.value().unwrap().contains("secret"));
    let mut described = node(2, &[], "android.widget.ImageView", "");
    described.description = "Search".into();
    assert_eq!(described.name(), "Search");
    assert_eq!(described.value(), None);
}

#[test]
fn fr18_bounds_go_to_the_screen_through_the_window() {
    // A 1:1 window whose client area starts at (100, 50).
    let window = WindowGeometry::one_to_one((400, 600), (400, 600));
    let rect = to_screen(bounds(10, 20, 110, 70), window, (100, 50));
    assert_eq!(
        (rect.left, rect.top, rect.width, rect.height),
        (110.0, 70.0, 100.0, 50.0)
    );
    // A task smaller than its window for a moment: frames are shown 1:1, never stretched,
    // and so are the bounds.
    let grown = WindowGeometry::one_to_one((800, 1200), (400, 600));
    let rect = to_screen(bounds(10, 20, 110, 70), grown, (0, 0));
    assert_eq!(
        (rect.left, rect.top, rect.width, rect.height),
        (10.0, 20.0, 100.0, 50.0)
    );
    // Cut to the client area.
    let rect = to_screen(bounds(-50, 550, 450, 700), window, (0, 0));
    assert_eq!(
        (rect.left, rect.top, rect.width, rect.height),
        (0.0, 550.0, 400.0, 50.0)
    );
    // And back.
    assert_eq!(from_screen((110.0, 70.0), grown, (100, 50)), (10, 20));
}

/// A task that keeps the density of a main monitor at 100% (130 dpi), shown on a monitor at
/// 125% (164 dpi): its window shows each task pixel at 164 window pixels per 130.
fn kept_at_100_percent_on_125_percent(task: (u32, u32)) -> WindowGeometry {
    let scale = scale_between(density_for_dpi(120), density_for_dpi(96));
    WindowGeometry {
        client: to_window_size(task, scale),
        task,
        scale,
    }
}

#[test]
fn fr18_on_a_monitor_of_another_scale_bounds_are_where_the_window_shows_the_node() {
    let window = kept_at_100_percent_on_125_percent((1300, 900));
    assert_eq!(window.scale, (82, 65));
    // A button 130 by 65 task pixels at (650, 325) is shown 164 by 82 window pixels at
    // (820, 410), not where the task has it.
    let rect = to_screen(bounds(650, 325, 780, 390), window, (100, 50));
    assert_eq!(
        (rect.left, rect.top, rect.width, rect.height),
        (920.0, 460.0, 164.0, 82.0)
    );
    // The whole task covers the whole client area, and no more.
    let rect = to_screen(bounds(0, 0, 1300, 900), window, (0, 0));
    assert_eq!(
        (rect.left, rect.top, rect.width, rect.height),
        (
            0.0,
            0.0,
            f64::from(window.client.0),
            f64::from(window.client.1)
        )
    );
    // A point on the button's shown middle hits the button's middle in the task.
    assert_eq!(
        from_screen((100.0 + 902.0, 50.0 + 451.0), window, (100, 50)),
        (715, 357)
    );
    // Every edge maps back to the pixel it came from.
    for (x, y) in [(0, 0), (650, 325), (1299, 899)] {
        let shown = to_screen(bounds(x, y, x + 1, y + 1), window, (0, 0));
        assert_eq!(
            from_screen((shown.left + 0.5, shown.top + 0.5), window, (0, 0)),
            (x, y)
        );
    }
}

#[test]
fn fr18_on_a_monitor_above_and_left_of_the_main_one_a_hit_test_lands_on_the_node_shown() {
    // The 125% monitor is left of and above the main one: its client area starts at
    // negative screen coordinates.
    let window = kept_at_100_percent_on_125_percent((810, 1661));
    let origin = (-2900, -880);
    let mut tree = Tree::new();
    let mut search = node(2, &[], "android.widget.Button", "Search");
    search.bounds = bounds(650, 1300, 780, 1365);
    let mut root = node(1, &[2], "android.widget.FrameLayout", "");
    root.bounds = bounds(0, 0, 810, 1661);
    tree.apply(TreeUpdate {
        task_id: 6,
        epoch: 1,
        full: true,
        nodes: vec![root, search],
        removed: vec![],
        roots: vec![1],
        focused: None,
    })
    .unwrap();
    // The button is shown at 820 by 1640 window pixels into the client area.
    let rect = to_screen(bounds(650, 1300, 780, 1365), window, origin);
    assert_eq!((rect.left, rect.top), (-2900.0 + 820.0, -880.0 + 1640.0));
    assert_eq!((rect.width, rect.height), (164.0, 82.0));
    // A point just inside its shown corner is on the button; one just above it is not, though
    // at 1:1 it would have been far below the button's top.
    let inside = from_screen((rect.left + 1.0, rect.top + 1.0), window, origin);
    assert_eq!(tree.hit(inside.0, inside.1), Some(2));
    let above = from_screen((rect.left + 1.0, rect.top - 2.0), window, origin);
    assert_eq!(tree.hit(above.0, above.1), Some(1));
    // At the task's own coordinates (1:1) the shown corner would have missed the button.
    let one_to_one = WindowGeometry::one_to_one(window.client, window.task);
    let wrong = from_screen((rect.left + 1.0, rect.top + 1.0), one_to_one, origin);
    assert_eq!(tree.hit(wrong.0, wrong.1), None);
}

#[test]
fn fr18_the_guest_is_asked_while_a_client_asks_and_stops_after_the_grace() {
    let start = Instant::now();
    let grace = Duration::from_secs(30);
    let mut presence = ClientPresence::new(grace);
    assert!(!presence.active());
    // Nothing is asked of the guest until a client asks.
    assert_eq!(presence.poll(start, false), None);
    assert_eq!(presence.asked(start), Some(PresenceChange::Start));
    assert_eq!(presence.asked(start + Duration::from_secs(1)), None);
    assert_eq!(presence.poll(start + Duration::from_secs(20), false), None);
    assert_eq!(
        presence.poll(start + Duration::from_secs(31), false),
        Some(PresenceChange::Stop)
    );
    assert!(!presence.active());
    // A client listening for events keeps it.
    assert_eq!(
        presence.asked(start + Duration::from_secs(40)),
        Some(PresenceChange::Start)
    );
    assert_eq!(presence.poll(start + Duration::from_secs(200), true), None);
    assert_eq!(presence.poll(start + Duration::from_secs(220), false), None);
    assert_eq!(
        presence.poll(start + Duration::from_secs(231), false),
        Some(PresenceChange::Stop)
    );
    // A window that closes stops at once.
    presence.asked(start + Duration::from_secs(300));
    assert_eq!(presence.end(), Some(PresenceChange::Stop));
    assert_eq!(presence.end(), None);
}
