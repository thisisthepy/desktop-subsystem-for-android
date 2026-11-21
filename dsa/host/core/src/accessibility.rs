//! The accessibility tree of a task's window, as a host accessibility client reads it.
//!
//! The agent in the guest reads a task's accessibility nodes only while a host client (a
//! screen reader, an inspection tool) wants them, and sends them as a full tree, then as
//! changes. This module holds what every host does with them, without naming any host
//! accessibility API:
//!
//! - [`TreeUpdate`]: one tree or change as it crosses the control channel and the local API,
//!   checked against [`MAX_NODES`] and [`MAX_TEXT_CHARS`] on the way in.
//! - [`Tree`]: the task's nodes. [`Tree::apply`] takes an update whole or not at all, and says
//!   what changed ([`Changes`]) so the host raises the right events.
//! - [`Role`]: what a node is, worked out from its Android class and its states
//!   ([`role_of`]), for the host to map onto its own control types.
//! - [`to_screen`]: a node's bounds on the host's screen.
//! - [`ClientPresence`]: when a window starts asking the guest for its tree, and when it stops.
//! - [`ClientDemand`]: whether a window watches its tree continuously (while an assistive
//!   technology uses it) or reads it once for a client that only reads.
//! - [`AccessibilityTarget`]: whose tree a watch reads, a task's window or a desktop widget's.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use dsa_protocol::control;
use dsa_protocol::host_api::types as wire;

use crate::input::WindowGeometry;
use crate::tasks::Bounds;

/// The most nodes one task's tree may have. Settings' longest screens have a few hundred; a
/// tree past this is a runaway app or a broken agent, and is dropped rather than held.
pub const MAX_NODES: usize = 4000;

/// The most characters any one text of a node keeps. Longer text (a whole document in one
/// view) is cut: a screen reader reads a few sentences of a node, not megabytes.
pub const MAX_TEXT_CHARS: usize = 2000;

/// How long a window keeps the guest reading its tree after the last host client asked for
/// it, while no client listens for events. A screen reader asks again within seconds as the
/// user moves; an inspection tool that has gone stops costing the guest anything after this.
pub const PRESENCE_GRACE: Duration = Duration::from_secs(30);

/// What can be done to a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NodeAction {
    #[default]
    Click,
    LongClick,
    SetText,
    ScrollForward,
    ScrollBackward,
    Focus,
    AccessibilityFocus,
    Select,
    /// Selects characters of an editable node's text, or puts its cursor somewhere.
    SetSelection,
    Expand,
    Collapse,
    /// Sets a slider's value ([`ActionRequest::progress`]).
    SetProgress,
    /// Scrolls the node's container so the node is on the screen.
    ShowOnScreen,
    /// Scrolls a list or a grid to a row and column ([`ActionRequest::position`]).
    ScrollToPosition,
}

/// A list's or a grid's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Collection {
    pub rows: i32,
    pub columns: i32,
    pub hierarchical: bool,
}

/// Where an item is in its list or grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CollectionItem {
    pub row: i32,
    pub row_span: i32,
    pub column: i32,
    pub column_span: i32,
    pub heading: bool,
}

/// The Android window a top-level node is the root of.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeWindow {
    pub title: String,
    pub modal: bool,
    pub popup: bool,
}

/// How a change of a node is announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiveRegion {
    #[default]
    Off,
    Polite,
    Assertive,
}

/// Something a screen reader should say for a task: an announcement or a toast.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Announcement {
    pub text: String,
    pub assertive: bool,
    pub toast: bool,
}

impl NodeAction {
    /// The action's number on both wires, which share it.
    pub fn to_wire(self) -> i32 {
        match self {
            NodeAction::Click => 1,
            NodeAction::LongClick => 2,
            NodeAction::SetText => 3,
            NodeAction::ScrollForward => 4,
            NodeAction::ScrollBackward => 5,
            NodeAction::Focus => 6,
            NodeAction::AccessibilityFocus => 7,
            NodeAction::Select => 8,
            NodeAction::SetSelection => 9,
            NodeAction::Expand => 10,
            NodeAction::Collapse => 11,
            NodeAction::SetProgress => 12,
            NodeAction::ShowOnScreen => 13,
            NodeAction::ScrollToPosition => 14,
        }
    }

    /// The action a wire number names; `None` for one this build does not know.
    pub fn from_wire(value: i32) -> Option<NodeAction> {
        Some(match value {
            1 => NodeAction::Click,
            2 => NodeAction::LongClick,
            3 => NodeAction::SetText,
            4 => NodeAction::ScrollForward,
            5 => NodeAction::ScrollBackward,
            6 => NodeAction::Focus,
            7 => NodeAction::AccessibilityFocus,
            8 => NodeAction::Select,
            9 => NodeAction::SetSelection,
            10 => NodeAction::Expand,
            11 => NodeAction::Collapse,
            12 => NodeAction::SetProgress,
            13 => NodeAction::ShowOnScreen,
            14 => NodeAction::ScrollToPosition,
            _ => return None,
        })
    }
}

impl fmt::Display for NodeAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NodeAction::Click => "click",
            NodeAction::LongClick => "long click",
            NodeAction::SetText => "set text",
            NodeAction::ScrollForward => "scroll forward",
            NodeAction::ScrollBackward => "scroll backward",
            NodeAction::Focus => "focus",
            NodeAction::AccessibilityFocus => "accessibility focus",
            NodeAction::Select => "select",
            NodeAction::SetSelection => "set selection",
            NodeAction::Expand => "expand",
            NodeAction::Collapse => "collapse",
            NodeAction::SetProgress => "set value",
            NodeAction::ShowOnScreen => "show on screen",
            NodeAction::ScrollToPosition => "scroll to position",
        })
    }
}

/// The range of a slider or a progress bar.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NodeRange {
    pub min: f32,
    pub max: f32,
    pub current: f32,
}

/// The states a node has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeStates {
    pub checkable: bool,
    pub checked: bool,
    pub selected: bool,
    pub enabled: bool,
    pub focusable: bool,
    pub focused: bool,
    pub scrollable: bool,
    pub clickable: bool,
    pub long_clickable: bool,
    pub editable: bool,
    pub password: bool,
    pub heading: bool,
    /// The node has the accessibility focus, which a screen reader's cursor follows.
    pub accessibility_focused: bool,
}

/// One node of a task's tree.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Node {
    /// Unique among the nodes of one epoch, never 0.
    pub id: u32,
    pub children: Vec<u32>,
    pub class_name: String,
    pub text: String,
    pub description: String,
    pub hint: String,
    pub view_id: String,
    /// In pixels of the task's window.
    pub bounds: Bounds,
    pub states: NodeStates,
    pub range: Option<NodeRange>,
    pub actions: Vec<NodeAction>,
    pub state_description: String,
    /// An editable node's selection: where it starts and where it ends (the cursor), in
    /// UTF-16 code units of its text, as Android counts them. Equal for a cursor; the start may
    /// come after the end for a selection made backwards.
    pub selection: Option<(u32, u32)>,
    /// A heading's level, 1 to 9; 0 when the app does not say.
    pub heading_level: u32,
    /// A link, and where it goes when the app says.
    pub link: bool,
    pub url: String,
    pub collection: Option<Collection>,
    pub collection_item: Option<CollectionItem>,
    pub live_region: LiveRegion,
    pub pane_title: String,
    /// Set for a top-level node: the window it is the root of.
    pub window: Option<NodeWindow>,
}

impl Node {
    /// What the node is.
    pub fn role(&self) -> Role {
        if self.link {
            return Role::Link;
        }
        role_of(&self.class_name, &self.states, self.range.is_some())
    }

    /// The name a host client reads for the node: its description, else its text, else, for
    /// an empty editable node, its hint, else the title of the pane or window it is. A
    /// password's text is never its name.
    pub fn name(&self) -> &str {
        if !self.description.is_empty() {
            &self.description
        } else if !self.text.is_empty() && !self.states.editable {
            &self.text
        } else if self.states.editable && !self.hint.is_empty() {
            &self.hint
        } else if !self.pane_title.is_empty() {
            &self.pane_title
        } else {
            self.window
                .as_ref()
                .map_or("", |window| window.title.as_str())
        }
    }

    /// The value of an editable node: its text, hidden for a password.
    pub fn value(&self) -> Option<String> {
        if !self.states.editable {
            return None;
        }
        Some(if self.states.password {
            "\u{2022}".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        })
    }

    pub fn supports(&self, action: NodeAction) -> bool {
        self.actions.contains(&action)
    }
}

/// A task's tree, or what changed in it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TreeUpdate {
    pub task_id: i32,
    pub epoch: u32,
    /// Replaces the whole tree and starts `epoch`.
    pub full: bool,
    pub roots: Vec<u32>,
    /// New or changed nodes, whole.
    pub nodes: Vec<Node>,
    pub removed: Vec<u32>,
    /// The node with the input focus.
    pub focused: Option<u32>,
    /// The node with the accessibility focus.
    pub accessibility_focused: Option<u32>,
}

/// An action a host client asks for on a node.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ActionRequest {
    pub epoch: u32,
    pub node_id: u32,
    pub action: NodeAction,
    /// The new text, for [`NodeAction::SetText`].
    pub text: String,
    pub request_id: u32,
    /// The selection's start and end in UTF-16 code units, for [`NodeAction::SetSelection`].
    pub selection: (u32, u32),
    /// The new value, for [`NodeAction::SetProgress`].
    pub progress: f32,
    /// The row and column, for [`NodeAction::ScrollToPosition`].
    pub position: (i32, i32),
}

/// What came of an [`ActionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    pub request_id: u32,
    /// `Ok` when the node did it, else why not.
    pub outcome: Result<(), String>,
}

/// Whose tree a watch reads: a task's window, or a desktop widget's window. A widget's tree
/// comes from the widget's own views, which the agent hosts, with bounds in pixels of the
/// widget's image; it is otherwise the same tree, watched, changed and acted on alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AccessibilityTarget {
    Task(i32),
    Widget(i32),
}

impl AccessibilityTarget {
    /// The target a message names: its widget when `widget_id` is set (never 0), else its
    /// task.
    pub fn of(task_id: i32, widget_id: i32) -> AccessibilityTarget {
        if widget_id != 0 {
            AccessibilityTarget::Widget(widget_id)
        } else {
            AccessibilityTarget::Task(task_id)
        }
    }

    /// The task ID and the widget ID the wires carry for it, the other being 0.
    pub fn ids(self) -> (i32, i32) {
        match self {
            AccessibilityTarget::Task(task_id) => (task_id, 0),
            AccessibilityTarget::Widget(widget_id) => (0, widget_id),
        }
    }

    /// The watch the control channel carries for it, `on` or off.
    pub fn to_control_watch(self, on: bool) -> control::AccessibilityWatch {
        let (task_id, widget_id) = self.ids();
        control::AccessibilityWatch {
            task_id,
            on,
            widget_id,
        }
    }
}

impl fmt::Display for AccessibilityTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccessibilityTarget::Task(task_id) => write!(f, "task {task_id}"),
            AccessibilityTarget::Widget(widget_id) => write!(f, "widget {widget_id}"),
        }
    }
}

/// What the guest sends about a watched task.
#[derive(Debug, Clone, PartialEq)]
pub enum GuestAccessibility {
    Tree(TreeUpdate),
    ActionResult(ActionResult),
    Announcement(Announcement),
}

/// Follows the focus the trees of one watch carry, so each place a focus move passes on its
/// way to a screen reader (the host service, the window that raises it) logs one line per
/// move with the wall-clock time it passed. The agent logs the same for the event and for
/// the tree it sent, so the lines of one move, read side by side, say how long each hop took.
#[derive(Debug, Default)]
pub struct FocusTrace {
    focused: Option<u32>,
    accessibility_focused: Option<u32>,
}

impl FocusTrace {
    pub fn new() -> FocusTrace {
        FocusTrace::default()
    }

    /// The line to log when `update` moves the input or the accessibility focus, saying that
    /// the move passed `hop` now; `None` when neither moved.
    pub fn moved(
        &mut self,
        update: &TreeUpdate,
        target: AccessibilityTarget,
        hop: &str,
    ) -> Option<String> {
        if update.focused == self.focused
            && update.accessibility_focused == self.accessibility_focused
        {
            return None;
        }
        self.focused = update.focused;
        self.accessibility_focused = update.accessibility_focused;
        Some(focus_trace_line(
            target,
            update.focused,
            update.accessibility_focused,
            hop,
            wall_millis(),
        ))
    }
}

/// One focus trace line: which focus moved where, which hop it passed, and when, in
/// milliseconds since 1970 so the guest's and the host's lines compare.
pub fn focus_trace_line(
    target: AccessibilityTarget,
    focused: Option<u32>,
    accessibility_focused: Option<u32>,
    hop: &str,
    wall_millis: u128,
) -> String {
    let id = |node: Option<u32>| node.map_or_else(|| "none".to_string(), |id| id.to_string());
    format!(
        "accessibility focus trace: {target}: focus {}, accessibility focus {}, {hop} at {wall_millis} ms",
        id(focused),
        id(accessibility_focused)
    )
}

/// Milliseconds since 1970 by the wall clock, for trace lines that compare across machines.
pub fn wall_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

impl Announcement {
    pub fn from_control(announcement: control::AccessibilityAnnouncement) -> Announcement {
        Announcement {
            text: capped(announcement.text),
            assertive: announcement.assertive,
            toast: announcement.toast,
        }
    }

    pub fn to_host_api(&self) -> wire::AccessibilityAnnouncement {
        wire::AccessibilityAnnouncement {
            text: self.text.clone(),
            assertive: self.assertive,
            toast: self.toast,
        }
    }

    pub fn from_host_api(announcement: wire::AccessibilityAnnouncement) -> Announcement {
        Announcement {
            text: capped(announcement.text),
            assertive: announcement.assertive,
            toast: announcement.toast,
        }
    }
}

impl LiveRegion {
    fn from_wire(value: i32) -> LiveRegion {
        match value {
            1 => LiveRegion::Polite,
            2 => LiveRegion::Assertive,
            _ => LiveRegion::Off,
        }
    }

    fn to_wire(self) -> i32 {
        match self {
            LiveRegion::Off => 0,
            LiveRegion::Polite => 1,
            LiveRegion::Assertive => 2,
        }
    }
}

/// The place in characters of `text` of an offset in UTF-16 code units, as Android counts a
/// selection; an offset past the end is the end.
pub fn utf16_to_chars(text: &str, at: u32) -> u32 {
    let mut units = 0u32;
    let mut chars = 0u32;
    for c in text.chars() {
        if units >= at {
            break;
        }
        units += c.len_utf16() as u32;
        chars += 1;
    }
    chars
}

/// The place in UTF-16 code units of `text` of an offset in characters.
pub fn chars_to_utf16(text: &str, at: u32) -> u32 {
    text.chars()
        .take(at as usize)
        .map(|c| c.len_utf16() as u32)
        .sum()
}

/// Cuts `text` to [`MAX_TEXT_CHARS`].
fn capped(text: String) -> String {
    if text.chars().count() <= MAX_TEXT_CHARS {
        text
    } else {
        text.chars().take(MAX_TEXT_CHARS).collect()
    }
}

/// Checks the parts of an update every wire shares: no node 0, no node twice, not too many.
fn check_counts(nodes: &[Node], removed: &[u32], roots: &[u32]) -> Result<(), String> {
    if nodes.len() > MAX_NODES || removed.len() > MAX_NODES || roots.len() > MAX_NODES {
        return Err(format!(
            "the tree has {} nodes, more than the {MAX_NODES} a window takes",
            nodes.len().max(removed.len()).max(roots.len())
        ));
    }
    let mut seen = HashSet::with_capacity(nodes.len());
    for node in nodes {
        if node.id == 0 {
            return Err("a node has the ID 0, which names no node".into());
        }
        if !seen.insert(node.id) {
            return Err(format!("node {} appears twice", node.id));
        }
        if node.children.len() > MAX_NODES {
            return Err(format!("node {} has too many children", node.id));
        }
    }
    Ok(())
}

impl TreeUpdate {
    /// Takes a tree from the control channel. An update that breaks the rules is an error
    /// that says why; text that is too long is cut.
    pub fn from_control(tree: control::AccessibilityTree) -> Result<TreeUpdate, String> {
        let nodes: Vec<Node> = tree
            .nodes
            .into_iter()
            .map(|node| {
                let bounds = node.bounds.unwrap_or_default();
                Node {
                    id: node.id,
                    children: node.child_ids,
                    class_name: capped(node.class_name),
                    text: capped(node.text),
                    description: capped(node.content_description),
                    hint: capped(node.hint),
                    view_id: capped(node.view_id),
                    bounds: Bounds {
                        left: bounds.left,
                        top: bounds.top,
                        right: bounds.right,
                        bottom: bounds.bottom,
                    },
                    states: NodeStates {
                        checkable: node.checkable,
                        checked: node.checked,
                        selected: node.selected,
                        enabled: node.enabled,
                        focusable: node.focusable,
                        focused: node.focused,
                        scrollable: node.scrollable,
                        clickable: node.clickable,
                        long_clickable: node.long_clickable,
                        editable: node.editable,
                        password: node.password,
                        heading: node.heading,
                        accessibility_focused: node.accessibility_focused,
                    },
                    range: node.range.map(|range| NodeRange {
                        min: range.min,
                        max: range.max,
                        current: range.current,
                    }),
                    actions: node
                        .actions
                        .into_iter()
                        .filter_map(NodeAction::from_wire)
                        .collect(),
                    state_description: capped(node.state_description),
                    selection: node.has_text_selection.then_some((
                        node.text_selection_start.max(0) as u32,
                        node.text_selection_end.max(0) as u32,
                    )),
                    heading_level: node.heading_level.min(9),
                    link: node.link,
                    url: capped(node.url),
                    collection: node.collection.map(|c| Collection {
                        rows: c.rows,
                        columns: c.columns,
                        hierarchical: c.hierarchical,
                    }),
                    collection_item: node.collection_item.map(|c| CollectionItem {
                        row: c.row,
                        row_span: c.row_span,
                        column: c.column,
                        column_span: c.column_span,
                        heading: c.heading,
                    }),
                    live_region: LiveRegion::from_wire(node.live_region),
                    pane_title: capped(node.pane_title),
                    window: node.window.map(|w| NodeWindow {
                        title: capped(w.title),
                        modal: w.modal,
                        popup: w.popup,
                    }),
                }
            })
            .collect();
        check_counts(&nodes, &tree.removed_ids, &tree.root_ids)?;
        Ok(TreeUpdate {
            task_id: tree.task_id,
            epoch: tree.epoch,
            full: tree.full,
            roots: tree.root_ids,
            nodes,
            removed: tree.removed_ids,
            focused: (tree.focused_id != 0).then_some(tree.focused_id),
            accessibility_focused: (tree.accessibility_focused_id != 0)
                .then_some(tree.accessibility_focused_id),
        })
    }

    /// The update as the local API carries it.
    pub fn to_host_api(&self) -> wire::AccessibilityTree {
        wire::AccessibilityTree {
            task_id: self.task_id,
            epoch: self.epoch,
            full: self.full,
            root_ids: self.roots.clone(),
            nodes: self
                .nodes
                .iter()
                .map(|node| wire::AccessibilityNode {
                    id: node.id,
                    child_ids: node.children.clone(),
                    class_name: node.class_name.clone(),
                    text: node.text.clone(),
                    content_description: node.description.clone(),
                    hint: node.hint.clone(),
                    view_id: node.view_id.clone(),
                    bounds: Some(wire::Rect {
                        left: node.bounds.left,
                        top: node.bounds.top,
                        right: node.bounds.right,
                        bottom: node.bounds.bottom,
                    }),
                    checkable: node.states.checkable,
                    checked: node.states.checked,
                    selected: node.states.selected,
                    enabled: node.states.enabled,
                    focusable: node.states.focusable,
                    focused: node.states.focused,
                    scrollable: node.states.scrollable,
                    clickable: node.states.clickable,
                    long_clickable: node.states.long_clickable,
                    editable: node.states.editable,
                    password: node.states.password,
                    heading: node.states.heading,
                    range: node.range.map(|range| wire::NodeRange {
                        min: range.min,
                        max: range.max,
                        current: range.current,
                    }),
                    actions: node.actions.iter().map(|a| a.to_wire()).collect(),
                    state_description: node.state_description.clone(),
                    accessibility_focused: node.states.accessibility_focused,
                    text_selection_start: node.selection.map_or(0, |s| s.0 as i32),
                    text_selection_end: node.selection.map_or(0, |s| s.1 as i32),
                    has_text_selection: node.selection.is_some(),
                    heading_level: node.heading_level,
                    link: node.link,
                    url: node.url.clone(),
                    collection: node.collection.map(|c| wire::NodeCollection {
                        rows: c.rows,
                        columns: c.columns,
                        hierarchical: c.hierarchical,
                    }),
                    collection_item: node.collection_item.map(|c| wire::NodeCollectionItem {
                        row: c.row,
                        row_span: c.row_span,
                        column: c.column,
                        column_span: c.column_span,
                        heading: c.heading,
                    }),
                    live_region: node.live_region.to_wire(),
                    pane_title: node.pane_title.clone(),
                    window: node.window.as_ref().map(|w| wire::NodeWindow {
                        title: w.title.clone(),
                        modal: w.modal,
                        popup: w.popup,
                    }),
                })
                .collect(),
            removed_ids: self.removed.clone(),
            focused_id: self.focused.unwrap_or(0),
            accessibility_focused_id: self.accessibility_focused.unwrap_or(0),
        }
    }

    /// Takes an update from the local API, checked as one from the control channel is.
    pub fn from_host_api(tree: wire::AccessibilityTree) -> Result<TreeUpdate, String> {
        // The two wires describe the same fields; go through the control form so both are
        // checked the same way.
        let as_control = control::AccessibilityTree {
            task_id: tree.task_id,
            epoch: tree.epoch,
            full: tree.full,
            root_ids: tree.root_ids,
            nodes: tree
                .nodes
                .into_iter()
                .map(|node| control::AccessibilityNode {
                    id: node.id,
                    child_ids: node.child_ids,
                    class_name: node.class_name,
                    text: node.text,
                    content_description: node.content_description,
                    hint: node.hint,
                    view_id: node.view_id,
                    bounds: node.bounds.map(|b| control::Rect {
                        left: b.left,
                        top: b.top,
                        right: b.right,
                        bottom: b.bottom,
                    }),
                    checkable: node.checkable,
                    checked: node.checked,
                    selected: node.selected,
                    enabled: node.enabled,
                    focusable: node.focusable,
                    focused: node.focused,
                    scrollable: node.scrollable,
                    clickable: node.clickable,
                    long_clickable: node.long_clickable,
                    editable: node.editable,
                    password: node.password,
                    heading: node.heading,
                    range: node.range.map(|r| control::NodeRange {
                        min: r.min,
                        max: r.max,
                        current: r.current,
                    }),
                    actions: node.actions,
                    state_description: node.state_description,
                    accessibility_focused: node.accessibility_focused,
                    text_selection_start: node.text_selection_start,
                    text_selection_end: node.text_selection_end,
                    has_text_selection: node.has_text_selection,
                    heading_level: node.heading_level,
                    link: node.link,
                    url: node.url,
                    collection: node.collection.map(|c| control::NodeCollection {
                        rows: c.rows,
                        columns: c.columns,
                        hierarchical: c.hierarchical,
                    }),
                    collection_item: node.collection_item.map(|c| control::NodeCollectionItem {
                        row: c.row,
                        row_span: c.row_span,
                        column: c.column,
                        column_span: c.column_span,
                        heading: c.heading,
                    }),
                    live_region: node.live_region,
                    pane_title: node.pane_title,
                    window: node.window.map(|w| control::NodeWindow {
                        title: w.title,
                        modal: w.modal,
                        popup: w.popup,
                    }),
                })
                .collect(),
            removed_ids: tree.removed_ids,
            focused_id: tree.focused_id,
            accessibility_focused_id: tree.accessibility_focused_id,
            widget_id: 0,
        };
        TreeUpdate::from_control(as_control)
    }
}

impl ActionRequest {
    /// The request as the control channel carries it, for `task_id`.
    pub fn to_control(&self, task_id: i32) -> control::AccessibilityAction {
        self.to_control_for(AccessibilityTarget::Task(task_id))
    }

    /// The request as the control channel carries it, for a node of `target`'s tree.
    pub fn to_control_for(&self, target: AccessibilityTarget) -> control::AccessibilityAction {
        let (task_id, widget_id) = target.ids();
        control::AccessibilityAction {
            task_id,
            epoch: self.epoch,
            node_id: self.node_id,
            action: self.action.to_wire(),
            text: self.text.clone(),
            request_id: self.request_id,
            selection_start: self.selection.0 as i32,
            selection_end: self.selection.1 as i32,
            progress: self.progress,
            row: self.position.0,
            column: self.position.1,
            widget_id,
        }
    }

    pub fn to_host_api(&self) -> wire::AccessibilityAction {
        wire::AccessibilityAction {
            epoch: self.epoch,
            node_id: self.node_id,
            action: self.action.to_wire(),
            text: self.text.clone(),
            request_id: self.request_id,
            selection_start: self.selection.0 as i32,
            selection_end: self.selection.1 as i32,
            progress: self.progress,
            row: self.position.0,
            column: self.position.1,
        }
    }

    /// Takes a request from the local API; an error says why it is not one.
    pub fn from_host_api(action: wire::AccessibilityAction) -> Result<ActionRequest, String> {
        let kind = NodeAction::from_wire(action.action)
            .ok_or_else(|| format!("{} names no action", action.action))?;
        Ok(ActionRequest {
            epoch: action.epoch,
            node_id: action.node_id,
            action: kind,
            text: capped(action.text),
            request_id: action.request_id,
            selection: (
                action.selection_start.max(0) as u32,
                action.selection_end.max(0) as u32,
            ),
            progress: action.progress,
            position: (action.row, action.column),
        })
    }
}

impl ActionResult {
    pub fn from_control(result: control::AccessibilityActionResult) -> ActionResult {
        ActionResult {
            request_id: result.request_id,
            outcome: if result.done {
                Ok(())
            } else if result.error.is_empty() {
                Err("Android did not do it".into())
            } else {
                Err(capped(result.error))
            },
        }
    }

    pub fn to_host_api(&self) -> wire::AccessibilityActionResult {
        wire::AccessibilityActionResult {
            request_id: self.request_id,
            done: self.outcome.is_ok(),
            error: self.outcome.clone().err().unwrap_or_default(),
        }
    }

    pub fn from_host_api(result: wire::AccessibilityActionResult) -> ActionResult {
        ActionResult::from_control(control::AccessibilityActionResult {
            task_id: 0,
            request_id: result.request_id,
            done: result.done,
            error: result.error,
            widget_id: 0,
        })
    }
}

/// Why an update was not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeError {
    /// A change for another epoch than the tree's, or before any full tree. The guest is asked
    /// for a full tree again.
    Epoch { tree: Option<u32>, update: u32 },
    /// The update leaves a tree that is not one; the detail says how.
    Malformed(String),
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeError::Epoch { tree: None, update } => write!(
                f,
                "a change for epoch {update} arrived before any full tree"
            ),
            TreeError::Epoch {
                tree: Some(tree),
                update,
            } => write!(
                f,
                "a change for epoch {update} arrived while the tree is at epoch {tree}"
            ),
            TreeError::Malformed(why) => f.write_str(why),
        }
    }
}

/// What an update changed, for the host's events.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Changes {
    /// The whole tree was replaced.
    pub replaced: bool,
    /// Whether the top-level nodes changed.
    pub roots_changed: bool,
    /// Nodes whose children changed, so their subtree is new to a client.
    pub children_changed: Vec<u32>,
    /// Nodes that came into the tree.
    pub added: Vec<u32>,
    /// Nodes that left it.
    pub removed: Vec<u32>,
    /// Nodes still in the tree whose own properties changed, with what they were.
    pub changed: Vec<Node>,
    /// The focus a host client follows ([`Tree::client_focus`]) moved to this node.
    pub focus: Option<u32>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        !self.replaced
            && !self.roots_changed
            && self.children_changed.is_empty()
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
            && self.focus.is_none()
    }
}

/// A task's accessibility tree as the host holds it.
#[derive(Debug, Clone, Default)]
pub struct Tree {
    epoch: Option<u32>,
    nodes: HashMap<u32, Node>,
    roots: Vec<u32>,
    parents: HashMap<u32, u32>,
    focused: Option<u32>,
    accessibility_focused: Option<u32>,
    client_focus: Option<u32>,
}

impl Tree {
    pub fn new() -> Tree {
        Tree::default()
    }

    /// The epoch the node IDs belong to; `None` before the first full tree.
    pub fn epoch(&self) -> Option<u32> {
        self.epoch
    }

    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn roots(&self) -> &[u32] {
        &self.roots
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node's parent; `None` for a top-level node or one not in the tree.
    pub fn parent(&self, id: u32) -> Option<u32> {
        self.parents.get(&id).copied()
    }

    /// The node with the input focus.
    pub fn focused(&self) -> Option<u32> {
        self.focused
    }

    /// The node with the accessibility focus, which Android's screen reader cursor follows.
    pub fn accessibility_focused(&self) -> Option<u32> {
        self.accessibility_focused
    }

    /// The node a host client treats as focused: whichever of the input focus and the
    /// accessibility focus moved last, or the one there is. A host has one focus where
    /// Android has two, and the one that moved is the one the user just went to.
    pub fn client_focus(&self) -> Option<u32> {
        self.client_focus
    }

    /// The list or grid an item is in: the nearest of its ancestors, within the few levels a
    /// list's rows sit at, that says its size.
    pub fn collection_of(&self, id: u32) -> Option<u32> {
        let mut at = self.parent(id);
        for _ in 0..3 {
            let node = self.nodes.get(&at?)?;
            if node.collection.is_some() {
                return Some(node.id);
            }
            at = self.parent(node.id);
        }
        None
    }

    /// An item's place in its list or grid, counted from 1, and how many items there are: a
    /// column of rows counts rows, a row of columns counts columns, a grid counts cells row by
    /// row.
    pub fn place_in_set(&self, id: u32) -> Option<(i32, i32)> {
        let item = self.nodes.get(&id)?.collection_item?;
        let collection = self.nodes.get(&self.collection_of(id)?)?.collection?;
        let (rows, columns) = (collection.rows, collection.columns);
        let (place, size) = if columns <= 1 {
            (item.row + 1, rows)
        } else if rows <= 1 {
            (item.column + 1, columns)
        } else {
            (item.row * columns + item.column + 1, rows * columns)
        };
        (size > 0 && place > 0).then_some((place, size))
    }

    /// The items of a list or grid that are in the tree (on the screen): its children, or
    /// theirs for a list that wraps its rows.
    pub fn items_of(&self, id: u32) -> Vec<u32> {
        let Some(node) = self.nodes.get(&id) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for child in &node.children {
            let Some(child) = self.nodes.get(child) else {
                continue;
            };
            if child.collection_item.is_some() {
                items.push(child.id);
            } else {
                items.extend(child.children.iter().copied().filter(|grandchild| {
                    self.nodes
                        .get(grandchild)
                        .is_some_and(|node| node.collection_item.is_some())
                }));
            }
        }
        items
    }

    /// The item of a list or grid at `row` and `column`, while it is in the tree.
    pub fn item_at(&self, id: u32, row: i32, column: i32) -> Option<u32> {
        self.items_of(id).into_iter().find(|item| {
            self.nodes
                .get(item)
                .and_then(|node| node.collection_item)
                .is_some_and(|place| place.row == row && place.column == column)
        })
    }

    /// The name a host client reads for a node. A node with no name of its own that the user
    /// acts on as one (a list row, a card, a button made of a layout) takes the names of the
    /// nodes inside it, as Android's own screen reader does: a Settings row reads as its
    /// title and summary.
    pub fn name(&self, id: u32) -> String {
        let Some(node) = self.nodes.get(&id) else {
            return String::new();
        };
        let own = node.name();
        if !own.is_empty() || !(node.states.clickable || node.states.long_clickable) {
            return own.to_string();
        }
        let mut parts = Vec::new();
        let mut stack: Vec<u32> = node.children.iter().rev().copied().collect();
        let mut visited = 0;
        while let Some(child) = stack.pop() {
            visited += 1;
            if visited > MAX_NODES || parts.len() >= 4 {
                break;
            }
            let Some(child) = self.nodes.get(&child) else {
                continue;
            };
            let name = child.name();
            if !name.is_empty() {
                parts.push(name.to_string());
            } else {
                stack.extend(child.children.iter().rev().copied());
            }
        }
        let mut name = parts.join(", ");
        if name.chars().count() > MAX_TEXT_CHARS {
            name = name.chars().take(MAX_TEXT_CHARS).collect();
        }
        name
    }

    /// The nodes that share the node's parent (the top-level nodes for a top-level node), in
    /// order.
    pub fn siblings(&self, id: u32) -> &[u32] {
        match self.parent(id) {
            Some(parent) => self
                .nodes
                .get(&parent)
                .map(|node| node.children.as_slice())
                .unwrap_or(&[]),
            None => &self.roots,
        }
    }

    /// The node after (`forward`) or before this one among its siblings.
    pub fn sibling(&self, id: u32, forward: bool) -> Option<u32> {
        let siblings = self.siblings(id);
        let at = siblings.iter().position(|sibling| *sibling == id)?;
        if forward {
            siblings.get(at + 1).copied()
        } else {
            at.checked_sub(1)
                .and_then(|before| siblings.get(before).copied())
        }
    }

    /// The deepest node whose bounds hold the point, in pixels of the task's window, looking
    /// at the last-drawn (last) child first, as the guest draws them.
    pub fn hit(&self, x: i32, y: i32) -> Option<u32> {
        let inside = |id: &u32| {
            self.nodes.get(id).is_some_and(|node| {
                let b = node.bounds;
                x >= b.left && x < b.right && y >= b.top && y < b.bottom
            })
        };
        let mut found = self.roots.iter().rev().find(|id| inside(id)).copied()?;
        // Bounded by the node count, so a tree that slipped a cycle in cannot loop forever.
        for _ in 0..self.nodes.len() {
            let next = self.nodes[&found]
                .children
                .iter()
                .rev()
                .find(|id| inside(id))
                .copied();
            match next {
                Some(child) => found = child,
                None => break,
            }
        }
        Some(found)
    }

    /// Forgets every node, as when the host stops asking for the tree.
    pub fn clear(&mut self) -> Changes {
        let removed: Vec<u32> = self.nodes.keys().copied().collect();
        *self = Tree::default();
        Changes {
            replaced: true,
            roots_changed: true,
            removed,
            ..Changes::default()
        }
    }

    /// Applies `update`. A full tree replaces everything; a change applies to the epoch it
    /// names. Either the whole update is taken or, with an error, none of it: an update that
    /// would leave the tree without a node it names, with a node twice, with a cycle, or with
    /// more than [`MAX_NODES`] nodes changes nothing.
    pub fn apply(&mut self, update: TreeUpdate) -> Result<Changes, TreeError> {
        check_counts(&update.nodes, &update.removed, &update.roots)
            .map_err(TreeError::Malformed)?;
        if !update.full && self.epoch != Some(update.epoch) {
            return Err(TreeError::Epoch {
                tree: self.epoch,
                update: update.epoch,
            });
        }
        let mut nodes = if update.full {
            HashMap::with_capacity(update.nodes.len())
        } else {
            self.nodes.clone()
        };
        let mut changes = Changes {
            replaced: update.full,
            ..Changes::default()
        };
        for id in &update.removed {
            if nodes.remove(id).is_some() {
                changes.removed.push(*id);
            }
        }
        for node in update.nodes {
            match nodes.get(&node.id) {
                None => changes.added.push(node.id),
                Some(old) => {
                    if old.children != node.children {
                        changes.children_changed.push(node.id);
                    }
                    let mut same = old.clone();
                    same.children = node.children.clone();
                    if same != node {
                        changes.changed.push(old.clone());
                    }
                }
            }
            nodes.insert(node.id, node);
        }
        if nodes.len() > MAX_NODES {
            return Err(TreeError::Malformed(format!(
                "the tree would have {} nodes, more than the {MAX_NODES} a window takes",
                nodes.len()
            )));
        }
        let parents = link(&nodes, &update.roots)?;
        for focused in [update.focused, update.accessibility_focused]
            .into_iter()
            .flatten()
        {
            if !nodes.contains_key(&focused) {
                return Err(TreeError::Malformed(format!(
                    "the focused node {focused} is not in the tree"
                )));
            }
        }
        changes.roots_changed = update.full || update.roots != self.roots;
        let client_focus =
            self.next_client_focus(update.full, update.focused, update.accessibility_focused);
        if client_focus != self.client_focus || (update.full && client_focus.is_some()) {
            changes.focus = client_focus;
        }
        // Removed nodes are gone, and a node removed and sent again in one update is new.
        changes.removed.retain(|id| !nodes.contains_key(id));
        self.epoch = Some(update.epoch);
        self.nodes = nodes;
        self.roots = update.roots;
        self.parents = parents;
        self.focused = update.focused;
        self.accessibility_focused = update.accessibility_focused;
        self.client_focus = client_focus;
        Ok(changes)
    }

    /// The client focus once `update` is applied: the focus that moved (the input focus when
    /// both did), else the one that still holds the node it held, else whichever there is.
    fn next_client_focus(
        &self,
        full: bool,
        input: Option<u32>,
        access: Option<u32>,
    ) -> Option<u32> {
        let input_moved = input.is_some() && (full || input != self.focused);
        let access_moved = access.is_some() && (full || access != self.accessibility_focused);
        if input_moved {
            return input;
        }
        if access_moved {
            return access;
        }
        let kept = (!full)
            .then_some(self.client_focus)
            .flatten()
            .filter(|id| Some(*id) == input || Some(*id) == access);
        kept.or(input).or(access)
    }
}

/// Every node's parent, checking that the nodes form a forest from `roots` that holds every
/// node exactly once.
fn link(nodes: &HashMap<u32, Node>, roots: &[u32]) -> Result<HashMap<u32, u32>, TreeError> {
    let mut parents = HashMap::with_capacity(nodes.len());
    let mut seen = HashSet::with_capacity(nodes.len());
    let mut stack: Vec<u32> = Vec::with_capacity(nodes.len());
    for root in roots {
        if !nodes.contains_key(root) {
            return Err(TreeError::Malformed(format!(
                "the top-level node {root} is not in the tree"
            )));
        }
        if !seen.insert(*root) {
            return Err(TreeError::Malformed(format!(
                "node {root} appears twice in the tree"
            )));
        }
        stack.push(*root);
    }
    while let Some(id) = stack.pop() {
        for child in &nodes[&id].children {
            if !nodes.contains_key(child) {
                return Err(TreeError::Malformed(format!(
                    "node {id} names child {child}, which is not in the tree"
                )));
            }
            if !seen.insert(*child) {
                return Err(TreeError::Malformed(format!(
                    "node {child} appears twice in the tree, or in a cycle"
                )));
            }
            parents.insert(*child, id);
            stack.push(*child);
        }
    }
    if seen.len() != nodes.len() {
        return Err(TreeError::Malformed(format!(
            "{} nodes are in no window's tree",
            nodes.len() - seen.len()
        )));
    }
    Ok(parents)
}

/// What a node is, in terms every host's accessibility API has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Button,
    CheckBox,
    RadioButton,
    Switch,
    Edit,
    Text,
    Heading,
    Image,
    List,
    ListItem,
    Grid,
    ScrollView,
    Slider,
    ProgressBar,
    ComboBox,
    Tab,
    TabList,
    Menu,
    MenuItem,
    Toolbar,
    Link,
    WebView,
    /// A layout that only groups other nodes.
    Group,
    /// A view no rule recognises.
    Unknown,
}

/// The part of a class name after its last dot (`Button` for `android.widget.Button`).
fn simple_name(class_name: &str) -> &str {
    class_name.rsplit(['.', '$']).next().unwrap_or(class_name)
}

/// Works out a node's role from its Android class and states. The class is matched by its
/// simple name, so the platform's widgets and their AndroidX and Material counterparts
/// (`androidx.appcompat.widget.AppCompatCheckBox`, `com.google.android.material.button.
/// MaterialButton`) map alike.
pub fn role_of(class_name: &str, states: &NodeStates, has_range: bool) -> Role {
    let name = simple_name(class_name);
    let has = |part: &str| name.contains(part);
    if states.heading {
        return Role::Heading;
    }
    if has("Switch") || has("ToggleButton") {
        return Role::Switch;
    }
    if has("CheckBox") || has("CheckedTextView") {
        return Role::CheckBox;
    }
    if has("RadioButton") {
        return Role::RadioButton;
    }
    if has("EditText") || has("SearchView") || has("AutoCompleteTextView") || states.editable {
        return Role::Edit;
    }
    if has("SeekBar") || has("Slider") || has("RatingBar") {
        return Role::Slider;
    }
    if has("ProgressBar") || has("ProgressIndicator") {
        return if has_range && states.focusable {
            Role::Slider
        } else {
            Role::ProgressBar
        };
    }
    if has("Spinner") {
        return Role::ComboBox;
    }
    if has("Button") || has("FloatingActionButton") || has("Chip") {
        return Role::Button;
    }
    if has("WebView") {
        return Role::WebView;
    }
    if has("TabWidget") || has("TabLayout") {
        return Role::TabList;
    }
    if has("TabView") || name == "Tab" {
        return Role::Tab;
    }
    if has("GridView") {
        return Role::Grid;
    }
    if has("ListView") || has("RecyclerView") {
        return Role::List;
    }
    if has("ScrollView") || has("ViewPager") || has("NestedScrollView") {
        return Role::ScrollView;
    }
    if has("Toolbar") || has("ActionBar") {
        return Role::Toolbar;
    }
    if has("MenuView") || has("PopupMenu") {
        return Role::Menu;
    }
    if has("MenuItem") {
        return Role::MenuItem;
    }
    if has("ImageView") {
        return if states.clickable {
            Role::Button
        } else {
            Role::Image
        };
    }
    if has("TextView") || has("Text") {
        return if states.clickable || states.long_clickable {
            Role::Button
        } else {
            Role::Text
        };
    }
    if states.scrollable {
        return Role::ScrollView;
    }
    if has("Layout") || name == "View" || name == "ViewGroup" {
        // A clickable layout is a list row or a card the user taps as one.
        return if states.clickable {
            Role::ListItem
        } else {
            Role::Group
        };
    }
    if states.clickable {
        Role::Button
    } else {
        Role::Unknown
    }
}

/// Where a node is on the host's screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

/// A node's `bounds`, in pixels of the task, on the host's screen. The window shows the task
/// from the top left of its client area, whose top left is at `client_origin` on the screen,
/// at the window's scale (`geometry.scale`): 1:1 for a task drawn at its window's density,
/// and larger or smaller for a task that keeps its density on a monitor of another scale, so
/// the bounds are where the window shows the node, not where the task has it. Frames are
/// never stretched to a client area of another size (`geometry.client` leads or trails the
/// task for a moment while the guest catches up). The result is cut to the client area, since
/// nothing outside it is seen.
pub fn to_screen(
    bounds: Bounds,
    geometry: WindowGeometry,
    client_origin: (i32, i32),
) -> ScreenRect {
    let clamp = |value: f64, limit: u32| {
        if limit == 0 {
            value.max(0.0)
        } else {
            value.clamp(0.0, f64::from(limit))
        }
    };
    let (left, top, right, bottom) = geometry.to_window_bounds(bounds);
    let left = clamp(left, geometry.client.0);
    let top = clamp(top, geometry.client.1);
    let right = clamp(right, geometry.client.0);
    let bottom = clamp(bottom, geometry.client.1);
    ScreenRect {
        left: f64::from(client_origin.0) + left,
        top: f64::from(client_origin.1) + top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

/// A point on the host's screen in pixels of the task, the inverse of [`to_screen`]: the
/// task pixel the window shows there, at the window's scale. A point on a monitor left of or
/// above the main one has negative coordinates, and so may the client area's origin.
pub fn from_screen(
    point: (f64, f64),
    geometry: WindowGeometry,
    client_origin: (i32, i32),
) -> (i32, i32) {
    let (x, y) = geometry.to_task_point((
        point.0 - f64::from(client_origin.0),
        point.1 - f64::from(client_origin.1),
    ));
    (x.floor() as i32, y.floor() as i32)
}

/// What a window should do about the guest's reading of its tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceChange {
    /// Ask the guest for the tree.
    Start,
    /// Tell the guest to stop reading it.
    Stop,
}

/// When a window wants its task's tree: from the first time a host client asks for the
/// window's accessibility tree until no client has asked for [`PRESENCE_GRACE`] and none
/// listens for events. A window nobody reads costs the guest nothing.
#[derive(Debug, Clone)]
pub struct ClientPresence {
    grace: Duration,
    last_asked: Option<Instant>,
    active: bool,
}

impl Default for ClientPresence {
    fn default() -> Self {
        ClientPresence::new(PRESENCE_GRACE)
    }
}

impl ClientPresence {
    pub fn new(grace: Duration) -> ClientPresence {
        ClientPresence {
            grace,
            last_asked: None,
            active: false,
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// A client asked for the window's tree at `now`.
    pub fn asked(&mut self, now: Instant) -> Option<PresenceChange> {
        self.last_asked = Some(now);
        if self.active {
            None
        } else {
            self.active = true;
            Some(PresenceChange::Start)
        }
    }

    /// Checks at `now` whether to stop. `listening` is whether any host client listens for
    /// accessibility events, which keeps the tree while it does.
    pub fn poll(&mut self, now: Instant, listening: bool) -> Option<PresenceChange> {
        if !self.active {
            return None;
        }
        if listening {
            self.last_asked = Some(now);
            return None;
        }
        let quiet = self
            .last_asked
            .map(|at| now.saturating_duration_since(at))
            .unwrap_or(Duration::MAX);
        if quiet >= self.grace {
            self.active = false;
            self.last_asked = None;
            Some(PresenceChange::Stop)
        } else {
            None
        }
    }

    /// The window closed, or the tree could not be had: stop without waiting.
    pub fn end(&mut self) -> Option<PresenceChange> {
        self.last_asked = None;
        std::mem::replace(&mut self.active, false).then_some(PresenceChange::Stop)
    }
}

/// The least time between two one-off reads of a window's tree for a client that is not an
/// assistive technology ([`ClientDemand`]). A client that walks the tree over and over (an
/// inspection tool, a feature of the desktop that looks at every window) then costs the app
/// one read every few seconds at most, not one for every change.
pub const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);

/// How a window's tree is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Demand {
    /// Watch the tree for as long as an assistive technology uses the window: every change
    /// comes as it happens.
    Continuous,
    /// Read the tree once, for a client that only reads, and stop.
    Snapshot,
}

/// Why a window's watch started or stopped, for the host's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemandReason {
    /// A screen reader, or another client that follows the focus, reads the window.
    AssistiveTechnology,
    /// A client that is no assistive technology read the window, so the tree is read once.
    Read,
    /// A client acted on a node, so the tree is read once and the action is sent.
    Action,
    /// The assistive technology that kept the watch has gone.
    AssistiveTechnologyGone,
    /// No client has asked for a while and none listens.
    Quiet,
}

impl fmt::Display for DemandReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DemandReason::AssistiveTechnology => {
                "a screen reader or another client that follows the focus reads the window"
            }
            DemandReason::Read => {
                "a UI Automation client that is no screen reader read the window; its tree is \
                 read once"
            }
            DemandReason::Action => "a client acted on a node; the tree is read once",
            DemandReason::AssistiveTechnologyGone => {
                "no screen reader or other client that follows the focus is active any more"
            }
            DemandReason::Quiet => "no client asked for the tree for a while and none listens",
        })
    }
}

/// When a window reads its task's tree, and how: continuously only while an assistive
/// technology (a screen reader, or a client that follows the focus) uses the window, and
/// otherwise once per request, at most every [`SNAPSHOT_INTERVAL`] for reads. Many clients
/// that are no screen reader look at every window as it opens; a continuous watch for each
/// would read every app's tree on every change, and each node read takes the app's UI
/// thread for a moment.
#[derive(Debug, Clone)]
pub struct ClientDemand {
    presence: ClientPresence,
    snapshot_interval: Duration,
    last_snapshot: Option<Instant>,
}

impl Default for ClientDemand {
    fn default() -> Self {
        ClientDemand::new(PRESENCE_GRACE, SNAPSHOT_INTERVAL)
    }
}

impl ClientDemand {
    pub fn new(grace: Duration, snapshot_interval: Duration) -> ClientDemand {
        ClientDemand {
            presence: ClientPresence::new(grace),
            snapshot_interval,
            last_snapshot: None,
        }
    }

    /// Whether the continuous watch runs.
    pub fn continuous(&self) -> bool {
        self.presence.active()
    }

    /// A client asked for the window's tree at `now`. `assistive` is whether an assistive
    /// technology is active; `reading` whether a one-off read runs already.
    pub fn asked(
        &mut self,
        now: Instant,
        assistive: bool,
        reading: bool,
    ) -> Option<(Demand, DemandReason)> {
        if assistive {
            return self
                .presence
                .asked(now)
                .map(|_| (Demand::Continuous, DemandReason::AssistiveTechnology));
        }
        if self.presence.active() || reading {
            return None;
        }
        let due = match self.last_snapshot {
            None => true,
            Some(at) => now.saturating_duration_since(at) >= self.snapshot_interval,
        };
        if !due {
            return None;
        }
        self.last_snapshot = Some(now);
        Some((Demand::Snapshot, DemandReason::Read))
    }

    /// A client asked for an action at `now`: it needs a watch to reach Android, whenever
    /// the last read was.
    pub fn acted(&mut self, now: Instant, reading: bool) -> Option<(Demand, DemandReason)> {
        if self.presence.active() || reading {
            return None;
        }
        self.last_snapshot = Some(now);
        Some((Demand::Snapshot, DemandReason::Action))
    }

    /// Checks at `now` whether the continuous watch stops: once no assistive technology is
    /// active, or once no client has asked for a while and none listens (`listening`).
    pub fn poll(
        &mut self,
        now: Instant,
        listening: bool,
        assistive: bool,
    ) -> Option<(PresenceChange, DemandReason)> {
        if self.presence.active() && !assistive {
            return self
                .presence
                .end()
                .map(|change| (change, DemandReason::AssistiveTechnologyGone));
        }
        self.presence
            .poll(now, listening)
            .map(|change| (change, DemandReason::Quiet))
    }

    /// The window closed, or the watch could not be had.
    pub fn end(&mut self) -> Option<PresenceChange> {
        self.presence.end()
    }
}

/// A watch on one task's tree, from the session: what the guest sends about the task, until
/// the watch is dropped. Dropping the last watch of a task tells the guest to stop reading it.
pub struct AccessibilityWatch {
    /// The trees and action results the guest sends for the task.
    pub updates: Receiver<GuestAccessibility>,
    on_end: Option<Box<dyn FnOnce() + Send>>,
    on_listening: Option<Box<dyn Fn(bool) + Send>>,
}

impl AccessibilityWatch {
    /// A watch that receives on `updates` and runs `on_end` once it is dropped.
    pub fn new(
        updates: Receiver<GuestAccessibility>,
        on_end: impl FnOnce() + Send + 'static,
    ) -> AccessibilityWatch {
        AccessibilityWatch {
            updates,
            on_end: Some(Box::new(on_end)),
            on_listening: None,
        }
    }

    /// The watch, telling `on_listening` each time its client says whether a screen reader
    /// listens ([`AccessibilityWatch::set_listening`]).
    pub fn with_listening(
        mut self,
        on_listening: impl Fn(bool) + Send + 'static,
    ) -> AccessibilityWatch {
        self.on_listening = Some(Box::new(on_listening));
        self
    }

    /// Whether a screen reader (or another client that follows the focus) listens for this
    /// watch's events, so Android sends focus at once. A watch starts as not listening.
    pub fn set_listening(&self, listening: bool) {
        if let Some(on_listening) = &self.on_listening {
            on_listening(listening);
        }
    }
}

impl Drop for AccessibilityWatch {
    fn drop(&mut self) {
        if let Some(on_end) = self.on_end.take() {
            on_end();
        }
    }
}

impl fmt::Debug for AccessibilityWatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessibilityWatch")
    }
}
