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

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use dsa_protocol::control;
use dsa_protocol::host_api::types as wire;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeAction {
    Click,
    LongClick,
    SetText,
    ScrollForward,
    ScrollBackward,
    Focus,
    AccessibilityFocus,
    Select,
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
}

impl Node {
    /// What the node is.
    pub fn role(&self) -> Role {
        role_of(&self.class_name, &self.states, self.range.is_some())
    }

    /// The name a host client reads for the node: its description, else its text, else, for
    /// an empty editable node, its hint. A password's text is never its name.
    pub fn name(&self) -> &str {
        if !self.description.is_empty() {
            &self.description
        } else if !self.text.is_empty() && !self.states.editable {
            &self.text
        } else if self.states.editable && !self.hint.is_empty() {
            &self.hint
        } else {
            ""
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
}

/// An action a host client asks for on a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRequest {
    pub epoch: u32,
    pub node_id: u32,
    pub action: NodeAction,
    /// The new text, for [`NodeAction::SetText`].
    pub text: String,
    pub request_id: u32,
}

/// What came of an [`ActionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    pub request_id: u32,
    /// `Ok` when the node did it, else why not.
    pub outcome: Result<(), String>,
}

/// What the guest sends about a watched task.
#[derive(Debug, Clone, PartialEq)]
pub enum GuestAccessibility {
    Tree(TreeUpdate),
    ActionResult(ActionResult),
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
                })
                .collect(),
            removed_ids: self.removed.clone(),
            focused_id: self.focused.unwrap_or(0),
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
                })
                .collect(),
            removed_ids: tree.removed_ids,
            focused_id: tree.focused_id,
        };
        TreeUpdate::from_control(as_control)
    }
}

impl ActionRequest {
    /// The request as the control channel carries it, for `task_id`.
    pub fn to_control(&self, task_id: i32) -> control::AccessibilityAction {
        control::AccessibilityAction {
            task_id,
            epoch: self.epoch,
            node_id: self.node_id,
            action: self.action.to_wire(),
            text: self.text.clone(),
            request_id: self.request_id,
        }
    }

    pub fn to_host_api(&self) -> wire::AccessibilityAction {
        wire::AccessibilityAction {
            epoch: self.epoch,
            node_id: self.node_id,
            action: self.action.to_wire(),
            text: self.text.clone(),
            request_id: self.request_id,
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
    /// The input focus moved to this node.
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

    pub fn focused(&self) -> Option<u32> {
        self.focused
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
        if let Some(focused) = update.focused {
            if !nodes.contains_key(&focused) {
                return Err(TreeError::Malformed(format!(
                    "the focused node {focused} is not in the tree"
                )));
            }
        }
        changes.roots_changed = update.full || update.roots != self.roots;
        if update.focused != self.focused || (update.full && update.focused.is_some()) {
            changes.focus = update.focused;
        }
        // Removed nodes are gone, and a node removed and sent again in one update is new.
        changes.removed.retain(|id| !nodes.contains_key(id));
        self.epoch = Some(update.epoch);
        self.nodes = nodes;
        self.roots = update.roots;
        self.parents = parents;
        self.focused = update.focused;
        Ok(changes)
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

/// A node's `bounds`, in pixels of the task, on the host's screen: the window shows the task
/// (`task_size`, in guest pixels) in its client area (`client_size`, in host pixels, which is
/// the task's size unless a frame is shown stretched), whose top left is at `client_origin` on
/// the screen. The result is cut to the client area, since nothing outside it is seen.
pub fn to_screen(
    bounds: Bounds,
    task_size: (u32, u32),
    client_size: (u32, u32),
    client_origin: (i32, i32),
) -> ScreenRect {
    let scale = |client: u32, task: u32| {
        if task == 0 || client == 0 {
            1.0
        } else {
            client as f64 / task as f64
        }
    };
    let (sx, sy) = (
        scale(client_size.0, task_size.0),
        scale(client_size.1, task_size.1),
    );
    let clamp = |value: f64, limit: u32| {
        if limit == 0 {
            value.max(0.0)
        } else {
            value.clamp(0.0, limit as f64)
        }
    };
    let left = clamp(bounds.left as f64 * sx, client_size.0);
    let top = clamp(bounds.top as f64 * sy, client_size.1);
    let right = clamp(bounds.right as f64 * sx, client_size.0);
    let bottom = clamp(bounds.bottom as f64 * sy, client_size.1);
    ScreenRect {
        left: client_origin.0 as f64 + left,
        top: client_origin.1 as f64 + top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

/// A point on the host's screen in pixels of the task, the inverse of [`to_screen`].
pub fn from_screen(
    point: (f64, f64),
    task_size: (u32, u32),
    client_size: (u32, u32),
    client_origin: (i32, i32),
) -> (i32, i32) {
    let scale = |client: u32, task: u32| {
        if task == 0 || client == 0 {
            1.0
        } else {
            task as f64 / client as f64
        }
    };
    (
        ((point.0 - client_origin.0 as f64) * scale(client_size.0, task_size.0)).floor() as i32,
        ((point.1 - client_origin.1 as f64) * scale(client_size.1, task_size.1)).floor() as i32,
    )
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

/// A watch on one task's tree, from the session: what the guest sends about the task, until
/// the watch is dropped. Dropping the last watch of a task tells the guest to stop reading it.
pub struct AccessibilityWatch {
    /// The trees and action results the guest sends for the task.
    pub updates: Receiver<GuestAccessibility>,
    on_end: Option<Box<dyn FnOnce() + Send>>,
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
