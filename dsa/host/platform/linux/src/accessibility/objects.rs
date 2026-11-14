//! The objects a task window shows an AT-SPI client, and everything a client can ask of them,
//! with no D-Bus in it.
//!
//! The application ([`Object::Application`]) has one frame per task window
//! ([`Object::Window`]). A frame's children are, in reading order, the title bar's buttons
//! ([`Object::Button`]: Back while the icon's cell shows the back arrow, then Minimize,
//! Maximize or Restore, and Close, while the window draws its title bar), then the task's
//! top-level accessibility nodes ([`Object::Node`]). A node's object path carries its tree's
//! epoch, so a client holding a node of an earlier tree is told it is gone rather than handed
//! another node that took its ID.
//!
//! Positions are in the window's logical pixels (its surface coordinates) from its top left.
//! A Wayland client is never told where its window is on the screen, so screen coordinates are
//! the window's own, as GTK reports them on Wayland; AT-SPI clients on Wayland expect this.
//!
//! [`Access::apply`] takes the guest's tree and its changes and says which AT-SPI events they
//! raise ([`Event`]); [`Access::sync`] does the same for what the window shows itself (its
//! title, its size, its title bar, whether it is active).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::time::Duration;

use dsa_host_core::accessibility::{
    from_screen, to_screen, ActionRequest, Changes, ClientPresence, Node, NodeAction, NodeRange,
    Tree, TreeError, TreeUpdate,
};
use dsa_host_core::compositor::caption::CaptionButton;
use dsa_host_core::input::WindowGeometry;
use dsa_host_core::tasks::Bounds;

use super::atspi::{self, coord, role, state, StateSet};
use super::text;
use crate::compositor::title_bar::TitleBar;
use crate::compositor::toplevel::Toplevel;
use crate::compositor::window_menu::{menu_text, reads_korean_in};

/// The AT-SPI interfaces the objects implement.
pub mod interface {
    pub const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
    pub const APPLICATION: &str = "org.a11y.atspi.Application";
    pub const COMPONENT: &str = "org.a11y.atspi.Component";
    pub const ACTION: &str = "org.a11y.atspi.Action";
    pub const VALUE: &str = "org.a11y.atspi.Value";
    pub const TEXT: &str = "org.a11y.atspi.Text";
    pub const EDITABLE_TEXT: &str = "org.a11y.atspi.EditableText";
}

/// The application's object path, which AT-SPI fixes.
pub const APPLICATION_PATH: &str = "/org/a11y/atspi/accessible/root";

/// The path AT-SPI names no object by.
pub const NULL_PATH: &str = "/org/a11y/atspi/null";

/// Where the windows' objects are.
const WINDOWS_PATH: &str = "/org/a11y/atspi/accessible/dsa";

/// The most name change events one batch of changes raises; a client reads the rest when it
/// next reads the nodes.
const MAX_NAME_EVENTS: usize = 64;

/// A rectangle in the window's logical pixels: left, top, width, height.
pub type Rect = (i32, i32, i32, i32);

/// A rectangle in the window's pixels: left, top, width, height.
pub type PixelRect = (i32, i32, u32, u32);

/// A button of the title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TitleButton {
    Back,
    Minimize,
    Maximize,
    Close,
}

impl TitleButton {
    fn path_name(self) -> &'static str {
        match self {
            TitleButton::Back => "back",
            TitleButton::Minimize => "minimize",
            TitleButton::Maximize => "maximize",
            TitleButton::Close => "close",
        }
    }

    fn from_path_name(name: &str) -> Option<TitleButton> {
        [
            TitleButton::Back,
            TitleButton::Minimize,
            TitleButton::Maximize,
            TitleButton::Close,
        ]
        .into_iter()
        .find(|button| button.path_name() == name)
    }
}

/// One object of the tree a client reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Object {
    Application,
    Window(i32),
    Button(i32, TitleButton),
    Node { task_id: i32, epoch: u32, id: u32 },
}

impl Object {
    /// The object's D-Bus path.
    pub fn path(&self) -> String {
        match *self {
            Object::Application => APPLICATION_PATH.to_string(),
            Object::Window(task_id) => format!("{WINDOWS_PATH}/t{}", task_id as u32),
            Object::Button(task_id, button) => {
                format!("{WINDOWS_PATH}/t{}/{}", task_id as u32, button.path_name())
            }
            Object::Node { task_id, epoch, id } => {
                format!("{WINDOWS_PATH}/t{}/e{epoch}/n{id}", task_id as u32)
            }
        }
    }

    /// The object a D-Bus path names; `None` for a path that names none.
    pub fn from_path(path: &str) -> Option<Object> {
        if path == APPLICATION_PATH {
            return Some(Object::Application);
        }
        let number = |part: &str, prefix: char| -> Option<u32> {
            let digits = part.strip_prefix(prefix)?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse().ok()
        };
        let rest = path.strip_prefix(WINDOWS_PATH)?.strip_prefix('/')?;
        let mut parts = rest.split('/');
        let task_id = number(parts.next()?, 't')? as i32;
        match (parts.next(), parts.next(), parts.next()) {
            (None, _, _) => Some(Object::Window(task_id)),
            (Some(button), None, _) => {
                TitleButton::from_path_name(button).map(|button| Object::Button(task_id, button))
            }
            (Some(epoch), Some(id), None) => Some(Object::Node {
                task_id,
                epoch: number(epoch, 'e')?,
                id: number(id, 'n')?,
            }),
            _ => None,
        }
    }

    /// The task whose window the object belongs to.
    pub fn task_id(&self) -> Option<i32> {
        match *self {
            Object::Application => None,
            Object::Window(task_id) | Object::Button(task_id, _) => Some(task_id),
            Object::Node { task_id, .. } => Some(task_id),
        }
    }
}

/// What a title bar button does, as a click on it would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleAction {
    /// Sends the task Back.
    Back,
    Minimize,
    Maximize,
    /// Restores a maximized window.
    Restore,
    Close,
}

/// A title bar button's action for the window of `task_id`, which the compositor's main
/// thread carries out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleCommand {
    pub task_id: i32,
    pub action: TitleAction,
}

/// What an action of an object does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    /// The node's accessibility action in Android.
    Node(NodeAction),
    /// The title bar button's click.
    Title(TitleAction),
}

/// One action an object offers through AT-SPI's Action interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    /// Its name, which clients match on.
    pub name: &'static str,
    /// Its name in the user's language, which a screen reader speaks.
    pub localized: &'static str,
    pub work: Work,
}

/// The node actions a client can call, in the order they are offered: the first is the
/// default action, which a screen reader calls when the user activates the object.
const NODE_ACTIONS: [(NodeAction, &str, &str, &str); 6] = [
    (NodeAction::Click, "click", "Click", "클릭"),
    (NodeAction::Select, "select", "Select", "선택"),
    (
        NodeAction::LongClick,
        "long press",
        "Long press",
        "길게 누르기",
    ),
    (
        NodeAction::ScrollForward,
        "scroll forward",
        "Scroll forward",
        "앞으로 스크롤",
    ),
    (
        NodeAction::ScrollBackward,
        "scroll backward",
        "Scroll backward",
        "뒤로 스크롤",
    ),
    (NodeAction::Focus, "focus", "Focus", "포커스"),
];

/// The actions a node offers: what it says it supports, the default first.
pub fn node_actions(node: &Node, korean: bool) -> Vec<Action> {
    NODE_ACTIONS
        .iter()
        .filter(|(action, ..)| node.supports(*action))
        .map(|&(action, name, english, korean_name)| Action {
            name,
            localized: if korean { korean_name } else { english },
            work: Work::Node(action),
        })
        .collect()
}

/// The back button's name in the user's language.
pub fn back_name(korean: bool) -> &'static str {
    if korean {
        "뒤로"
    } else {
        "Back"
    }
}

/// A title bar button's name in the user's language: the window menu's words for the caption
/// buttons, and Restore for the maximize button of a maximized window.
pub fn button_name(button: TitleButton, maximized: bool, korean: bool) -> &'static str {
    let words = menu_text(korean);
    match button {
        TitleButton::Back => back_name(korean),
        TitleButton::Minimize => words.minimize,
        TitleButton::Maximize if maximized => words.restore,
        TitleButton::Maximize => words.maximize,
        TitleButton::Close => words.close,
    }
}

/// What a click on a title bar button does.
pub fn button_action(button: TitleButton, maximized: bool) -> TitleAction {
    match button {
        TitleButton::Back => TitleAction::Back,
        TitleButton::Minimize => TitleAction::Minimize,
        TitleButton::Maximize if maximized => TitleAction::Restore,
        TitleButton::Maximize => TitleAction::Maximize,
        TitleButton::Close => TitleAction::Close,
    }
}

/// Whether the user's first preferred language is Korean: the first entry of `LANGUAGE`, the
/// list of languages gettext reads messages in by preference, else the locale gettext would
/// choose (`LC_ALL`, `LC_MESSAGES`, `LANG`), as `var` reads them.
pub fn prefers_korean(var: impl Fn(&str) -> Option<String>) -> bool {
    let first = var("LANGUAGE").and_then(|list| {
        list.split(':')
            .find(|language| !language.is_empty())
            .map(str::to_string)
    });
    match first {
        Some(language) => language.starts_with("ko"),
        None => reads_korean_in(var),
    }
}

/// Where the title bar's cells are, in the window's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptionCells {
    /// The icon's cell while it shows the back arrow.
    pub back: Option<PixelRect>,
    pub minimize: PixelRect,
    pub maximize: PixelRect,
    pub close: PixelRect,
}

/// What a task window shows a client of itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowFacts {
    pub title: String,
    /// The window's scale: pixels per logical pixel.
    pub scale: u32,
    /// The window's size in pixels.
    pub shown: (u32, u32),
    /// The task's size in Android's pixels.
    pub task: (u32, u32),
    /// How large the window shows each of the task's pixels (window pixels per task pixels):
    /// 1:1 unless the task keeps its density on a monitor of another scale. A node's bounds
    /// are in the task's pixels, and the window shows them at this scale.
    pub frame_scale: (u32, u32),
    /// Whether the window is the active one.
    pub active: bool,
    pub maximized: bool,
    pub resizable: bool,
    /// The title bar's buttons, while the window draws its title bar.
    pub caption: Option<CaptionCells>,
}

/// What a window shows of itself now, from its model and its title bar.
pub fn window_facts(model: &Toplevel, title_bar: &TitleBar) -> WindowFacts {
    let caption = model.has_caption().then(|| {
        let layout = model.caption_layout();
        CaptionCells {
            back: if title_bar.back {
                layout.icon_cell()
            } else {
                None
            },
            minimize: layout.button_rect(CaptionButton::Minimize),
            maximize: layout.button_rect(CaptionButton::Maximize),
            close: layout.button_rect(CaptionButton::Close),
        }
    });
    WindowFacts {
        title: model.title.clone(),
        scale: model.scale.max(1),
        shown: model.shown,
        task: model.task,
        frame_scale: model.frame_scale,
        active: model.states.activated,
        maximized: model.states.maximized,
        resizable: model.resizable,
        caption,
    }
}

/// A rectangle of the window's pixels in its logical pixels, grown to whole logical pixels.
fn logical(left: f64, top: f64, width: f64, height: f64, scale: u32) -> Rect {
    let scale = f64::from(scale.max(1));
    let x0 = (left / scale).floor();
    let y0 = (top / scale).floor();
    let x1 = ((left + width) / scale).ceil();
    let y1 = ((top + height) / scale).ceil();
    (x0 as i32, y0 as i32, (x1 - x0) as i32, (y1 - y0) as i32)
}

/// One window's objects and its tree.
pub struct WindowAccess {
    pub facts: WindowFacts,
    pub tree: Tree,
    /// When the window wants its task's tree from the guest.
    pub presence: ClientPresence,
    /// The running watch of the task's tree, by its number.
    pub(crate) watch: Option<u64>,
    /// Where the running watch takes actions.
    pub(crate) actions: Option<Sender<ActionRequest>>,
    next_request: u32,
}

impl WindowAccess {
    fn new(facts: WindowFacts, grace: Duration) -> WindowAccess {
        WindowAccess {
            facts,
            tree: Tree::new(),
            presence: ClientPresence::new(grace),
            watch: None,
            actions: None,
            next_request: 0,
        }
    }

    /// The title bar's buttons in order.
    pub fn buttons(&self) -> Vec<TitleButton> {
        buttons_of(&self.facts)
    }

    fn cell(&self, button: TitleButton) -> Option<PixelRect> {
        cell_of(&self.facts, button)
    }

    fn frame_rect(&self) -> Rect {
        let (width, height) = self.facts.shown;
        logical(
            0.0,
            0.0,
            f64::from(width),
            f64::from(height),
            self.facts.scale,
        )
    }

    fn button_rect(&self, button: TitleButton) -> Option<Rect> {
        let (left, top, width, height) = self.cell(button)?;
        Some(logical(
            f64::from(left),
            f64::from(top),
            f64::from(width),
            f64::from(height),
            self.facts.scale,
        ))
    }

    /// What node bounds and points are converted with: the window shows the task's frames
    /// at its frame scale.
    fn geometry(&self) -> WindowGeometry {
        WindowGeometry {
            client: self.facts.shown,
            task: self.facts.task,
            scale: self.facts.frame_scale,
        }
    }

    fn node_rect(&self, bounds: Bounds) -> Rect {
        let rect = to_screen(bounds, self.geometry(), (0, 0));
        logical(
            rect.left,
            rect.top,
            rect.width,
            rect.height,
            self.facts.scale,
        )
    }

    fn node(&self, epoch: u32, id: u32) -> Option<&Node> {
        if self.tree.epoch() != Some(epoch) {
            return None;
        }
        self.tree.node(id)
    }

    fn node_object(&self, task_id: i32, id: u32) -> Option<Object> {
        Some(Object::Node {
            task_id,
            epoch: self.tree.epoch()?,
            id,
        })
    }

    /// The frame's children: the title bar's buttons, then the task's top-level nodes.
    fn children(&self, task_id: i32) -> Vec<Object> {
        let mut children: Vec<Object> = self
            .buttons()
            .into_iter()
            .map(|button| Object::Button(task_id, button))
            .collect();
        children.extend(
            self.tree
                .roots()
                .iter()
                .filter_map(|id| self.node_object(task_id, *id)),
        );
        children
    }
}

/// The title bar's buttons of a window that shows `facts`, in order.
fn buttons_of(facts: &WindowFacts) -> Vec<TitleButton> {
    let Some(cells) = facts.caption else {
        return Vec::new();
    };
    let mut buttons = Vec::with_capacity(4);
    if cells.back.is_some() {
        buttons.push(TitleButton::Back);
    }
    buttons.extend([
        TitleButton::Minimize,
        TitleButton::Maximize,
        TitleButton::Close,
    ]);
    buttons
}

/// What a window event says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEvent {
    Create,
    Destroy,
    Activate,
    Deactivate,
}

/// An AT-SPI event, for the clients that listen.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `object:children-changed:add`.
    ChildAdded {
        parent: Object,
        index: i32,
        child: Object,
    },
    /// `object:children-changed:remove`.
    ChildRemoved {
        parent: Object,
        index: i32,
        child: Object,
    },
    /// `object:state-changed:<state>`.
    State {
        object: Object,
        state: &'static str,
        on: bool,
    },
    /// `object:property-change:accessible-name`.
    Name { object: Object, name: String },
    /// `object:property-change:accessible-description`.
    Description { object: Object, description: String },
    /// `object:property-change:accessible-value`.
    Value { object: Object, value: f64 },
    /// `object:bounds-changed`.
    Bounds { object: Object, rect: Rect },
    /// `object:text-changed:delete`.
    TextRemoved {
        object: Object,
        start: i32,
        text: String,
    },
    /// `object:text-changed:insert`.
    TextInserted {
        object: Object,
        start: i32,
        text: String,
    },
    /// `window:<kind>`, from the window's frame.
    Window {
        task_id: i32,
        kind: WindowEvent,
        title: String,
    },
}

/// The children-changed events from `before` to `after` under `parent`, the first at
/// `offset`: the children gone (last first), then the new ones.
fn child_events<T: Copy + PartialEq>(
    parent: Object,
    offset: i32,
    before: &[T],
    after: &[T],
    object: impl Fn(T) -> Option<Object>,
    events: &mut Vec<Event>,
) {
    for (index, child) in before.iter().enumerate().rev() {
        if !after.contains(child) {
            if let Some(child) = object(*child) {
                events.push(Event::ChildRemoved {
                    parent,
                    index: offset + index as i32,
                    child,
                });
            }
        }
    }
    for (index, child) in after.iter().enumerate() {
        if !before.contains(child) {
            if let Some(child) = object(*child) {
                events.push(Event::ChildAdded {
                    parent,
                    index: offset + index as i32,
                    child,
                });
            }
        }
    }
}

/// Every window's objects, as the AT-SPI provider of the compositor answers for them.
pub struct Access {
    windows: BTreeMap<i32, WindowAccess>,
    korean: bool,
    grace: Duration,
}

impl Access {
    /// No windows yet. Names are Korean when `korean`; a window stops asking the guest for its
    /// tree `grace` after a client last asked for it while none listens.
    pub fn new(korean: bool, grace: Duration) -> Access {
        Access {
            windows: BTreeMap::new(),
            korean,
            grace,
        }
    }

    pub fn korean(&self) -> bool {
        self.korean
    }

    pub fn window(&self, task_id: i32) -> Option<&WindowAccess> {
        self.windows.get(&task_id)
    }

    pub(crate) fn window_mut(&mut self, task_id: i32) -> Option<&mut WindowAccess> {
        self.windows.get_mut(&task_id)
    }

    pub fn task_ids(&self) -> Vec<i32> {
        self.windows.keys().copied().collect()
    }

    /// The node an object names, while it is in the tree of its epoch.
    fn node(&self, object: Object) -> Option<(&WindowAccess, &Node)> {
        let Object::Node { task_id, epoch, id } = object else {
            return None;
        };
        let window = self.windows.get(&task_id)?;
        Some((window, window.node(epoch, id)?))
    }

    /// Whether the object is there to be read.
    pub fn exists(&self, object: Object) -> bool {
        match object {
            Object::Application => true,
            Object::Window(task_id) => self.windows.contains_key(&task_id),
            Object::Button(task_id, button) => self
                .windows
                .get(&task_id)
                .is_some_and(|window| window.buttons().contains(&button)),
            Object::Node { .. } => self.node(object).is_some(),
        }
    }

    /// The task whose tree a call on `object` asks for: a call that reads into a window (its
    /// children, a point in it, or any of its buttons and nodes). Reading a window's name or
    /// role, as a window switcher does, asks for nothing, so the guest reads no tree for it.
    pub fn wants_tree(object: Object, member: &str, property: Option<&str>) -> Option<i32> {
        match object {
            Object::Application => None,
            Object::Window(task_id) => {
                let reads_in = matches!(
                    member,
                    "GetChildren" | "GetChildAtIndex" | "GetAccessibleAtPoint" | "GetAll"
                ) || property == Some("ChildCount");
                reads_in.then_some(task_id)
            }
            Object::Button(task_id, _) | Object::Node { task_id, .. } => Some(task_id),
        }
    }

    /// The object's children, in order.
    pub fn children(&self, object: Object) -> Vec<Object> {
        match object {
            Object::Application => self.windows.keys().map(|id| Object::Window(*id)).collect(),
            Object::Window(task_id) => self
                .windows
                .get(&task_id)
                .map(|window| window.children(task_id))
                .unwrap_or_default(),
            Object::Button(..) => Vec::new(),
            Object::Node { task_id, epoch, .. } => self
                .node(object)
                .map(|(_, node)| {
                    node.children
                        .iter()
                        .map(|id| Object::Node {
                            task_id,
                            epoch,
                            id: *id,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The object's parent; `None` for the application, whose parent is the desktop.
    pub fn parent(&self, object: Object) -> Option<Object> {
        match object {
            Object::Application => None,
            Object::Window(_) => Some(Object::Application),
            Object::Button(task_id, _) => Some(Object::Window(task_id)),
            Object::Node { task_id, epoch, id } => {
                let (window, _) = self.node(object)?;
                Some(match window.tree.parent(id) {
                    Some(parent) => Object::Node {
                        task_id,
                        epoch,
                        id: parent,
                    },
                    None => Object::Window(task_id),
                })
            }
        }
    }

    /// Where the object stands among its parent's children; -1 when it is not there.
    pub fn index_in_parent(&self, object: Object) -> i32 {
        match object {
            Object::Application => -1,
            _ => self
                .parent(object)
                .and_then(|parent| {
                    self.children(parent)
                        .iter()
                        .position(|child| *child == object)
                })
                .map_or(-1, |at| at as i32),
        }
    }

    /// The object's AT-SPI role.
    pub fn role(&self, object: Object) -> u32 {
        match object {
            Object::Application => role::APPLICATION,
            Object::Window(_) => role::FRAME,
            Object::Button(..) => role::PUSH_BUTTON,
            Object::Node { .. } => self
                .node(object)
                .map_or(role::PANEL, |(_, node)| atspi::role_of(node)),
        }
    }

    /// The name a client reads for the object.
    pub fn name(&self, object: Object) -> String {
        match object {
            Object::Application => "Desktop Subsystem for Android".into(),
            Object::Window(task_id) => self
                .windows
                .get(&task_id)
                .map(|window| window.facts.title.clone())
                .unwrap_or_default(),
            Object::Button(task_id, button) => {
                let maximized = self
                    .windows
                    .get(&task_id)
                    .is_some_and(|window| window.facts.maximized);
                button_name(button, maximized, self.korean).to_string()
            }
            Object::Node { id, .. } => self
                .node(object)
                .map(|(window, _)| window.tree.name(id))
                .unwrap_or_default(),
        }
    }

    /// What a client reads after the name: a node's state in words ("On", "50 percent").
    pub fn description(&self, object: Object) -> String {
        self.node(object)
            .map(|(_, node)| node.state_description.clone())
            .unwrap_or_default()
    }

    /// A node's hint, as the help text of an empty field.
    pub fn help_text(&self, object: Object) -> String {
        self.node(object)
            .map(|(_, node)| node.hint.clone())
            .unwrap_or_default()
    }

    /// An ID a test or an automation tool finds the object by: a node's view ID, a button's
    /// name in English.
    pub fn accessible_id(&self, object: Object) -> String {
        match object {
            Object::Button(_, button) => button.path_name().to_string(),
            Object::Node { .. } => self
                .node(object)
                .map(|(_, node)| node.view_id.clone())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// The object's states. A node that is gone is defunct, as AT-SPI says of an object that
    /// no longer exists.
    pub fn states(&self, object: Object) -> StateSet {
        let shown = StateSet::new()
            .with(state::VISIBLE, true)
            .with(state::SHOWING, true)
            .with(state::ENABLED, true)
            .with(state::SENSITIVE, true);
        match object {
            Object::Application => shown,
            Object::Window(task_id) => match self.windows.get(&task_id) {
                Some(window) => shown
                    .with(state::ACTIVE, window.facts.active)
                    .with(state::RESIZABLE, window.facts.resizable),
                None => StateSet::new().with(state::DEFUNCT, true),
            },
            Object::Button(..) if self.exists(object) => shown,
            Object::Button(..) => StateSet::new().with(state::DEFUNCT, true),
            Object::Node { id, .. } => match self.node(object) {
                Some((window, node)) => {
                    let (_, _, width, height) = window.node_rect(node.bounds);
                    atspi::states_of(
                        node,
                        window.tree.focused() == Some(id),
                        width > 0 && height > 0,
                    )
                }
                None => StateSet::new().with(state::DEFUNCT, true),
            },
        }
    }

    /// The object's attributes: what the node is in Android.
    pub fn attributes(&self, object: Object) -> Vec<(String, String)> {
        let mut attributes = vec![("toolkit".to_string(), "Android".to_string())];
        if let Some((_, node)) = self.node(object) {
            attributes.push(("class".into(), node.class_name.clone()));
            if !node.view_id.is_empty() {
                attributes.push(("id".into(), node.view_id.clone()));
            }
        }
        attributes
    }

    /// The AT-SPI interfaces the object implements.
    pub fn interfaces(&self, object: Object) -> Vec<&'static str> {
        let mut interfaces = vec![interface::ACCESSIBLE];
        match object {
            Object::Application => interfaces.push(interface::APPLICATION),
            Object::Window(_) => interfaces.push(interface::COMPONENT),
            Object::Button(..) => interfaces.extend([interface::COMPONENT, interface::ACTION]),
            Object::Node { .. } => {
                interfaces.push(interface::COMPONENT);
                if let Some((_, node)) = self.node(object) {
                    if !node_actions(node, false).is_empty() {
                        interfaces.push(interface::ACTION);
                    }
                    if node.range.is_some() {
                        interfaces.push(interface::VALUE);
                    }
                    if self.text(object).is_some() {
                        interfaces.push(interface::TEXT);
                    }
                    if node.states.editable && node.supports(NodeAction::SetText) {
                        interfaces.push(interface::EDITABLE_TEXT);
                    }
                }
            }
        }
        interfaces
    }

    /// The object's rectangle in the window, in AT-SPI's `coord_type`: screen and window
    /// coordinates are both the window's (see the module's notes), parent coordinates are
    /// from the parent's top left. `None` for the application, which has no place.
    pub fn extents(&self, object: Object, coord_type: u32) -> Option<Rect> {
        let rect = self.window_rect(object)?;
        if coord_type != coord::PARENT {
            return Some(rect);
        }
        let origin = self
            .parent(object)
            .and_then(|parent| self.window_rect(parent))
            .map_or((0, 0), |(x, y, _, _)| (x, y));
        Some((rect.0 - origin.0, rect.1 - origin.1, rect.2, rect.3))
    }

    fn window_rect(&self, object: Object) -> Option<Rect> {
        match object {
            Object::Application => None,
            Object::Window(task_id) => Some(self.windows.get(&task_id)?.frame_rect()),
            Object::Button(task_id, button) => self.windows.get(&task_id)?.button_rect(button),
            Object::Node { .. } => {
                let (window, node) = self.node(object)?;
                Some(window.node_rect(node.bounds))
            }
        }
    }

    /// A point in `coord_type` in the window's logical pixels.
    fn to_window(&self, object: Object, x: i32, y: i32, coord_type: u32) -> (i32, i32) {
        if coord_type != coord::PARENT {
            return (x, y);
        }
        let origin = self
            .parent(object)
            .and_then(|parent| self.window_rect(parent))
            .map_or((0, 0), |(px, py, _, _)| (px, py));
        (x + origin.0, y + origin.1)
    }

    /// Whether the point is in the object.
    pub fn contains(&self, object: Object, x: i32, y: i32, coord_type: u32) -> bool {
        let Some((left, top, width, height)) = self.extents(object, coord_type) else {
            return false;
        };
        x >= left && x < left + width && y >= top && y < top + height
    }

    /// The deepest object inside `object` at the point: a title bar button over the app's
    /// top, else the deepest node there. `None` when nothing of the object is there.
    pub fn at_point(&self, object: Object, x: i32, y: i32, coord_type: u32) -> Option<Object> {
        let task_id = object.task_id()?;
        let window = self.windows.get(&task_id)?;
        let (wx, wy) = self.to_window(object, x, y, coord_type);
        if object == Object::Window(task_id) {
            // The buttons are drawn over the app, so they are what a point on them hits.
            for button in window.buttons() {
                if let Some((left, top, width, height)) = window.button_rect(button) {
                    if wx >= left && wx < left + width && wy >= top && wy < top + height {
                        return Some(Object::Button(task_id, button));
                    }
                }
            }
        }
        // The middle of the logical pixel, in the window's pixels, is the task pixel the
        // window shows there at its frame scale.
        let scale = f64::from(window.facts.scale.max(1));
        let (tx, ty) = from_screen(
            ((f64::from(wx) + 0.5) * scale, (f64::from(wy) + 0.5) * scale),
            window.geometry(),
            (0, 0),
        );
        let hit = window.tree.hit(tx, ty)?;
        let found = window.node_object(task_id, hit)?;
        match object {
            Object::Window(_) => Some(found),
            Object::Node { id, .. } => {
                // Only a node inside this one.
                let mut at = window.tree.parent(hit);
                while let Some(ancestor) = at {
                    if ancestor == id {
                        return Some(found);
                    }
                    at = window.tree.parent(ancestor);
                }
                None
            }
            _ => None,
        }
    }

    /// The actions the object offers, the default first.
    pub fn actions(&self, object: Object) -> Vec<Action> {
        match object {
            Object::Button(task_id, button) if self.exists(object) => {
                let maximized = self
                    .windows
                    .get(&task_id)
                    .is_some_and(|window| window.facts.maximized);
                vec![Action {
                    name: "click",
                    localized: if self.korean { "클릭" } else { "Click" },
                    work: Work::Title(button_action(button, maximized)),
                }]
            }
            Object::Node { .. } => self
                .node(object)
                .map(|(_, node)| node_actions(node, self.korean))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// The text AT-SPI's Text interface reads of a node: an editable node's value (a
    /// password as dots), else its text. `None` for a node with no text.
    pub fn text(&self, object: Object) -> Option<String> {
        let (_, node) = self.node(object)?;
        if node.states.editable {
            return node.value();
        }
        (!node.text.is_empty()).then(|| node.text.clone())
    }

    /// A slider's or a progress bar's range.
    pub fn range(&self, object: Object) -> Option<NodeRange> {
        self.node(object)?.1.range
    }

    /// How a client's focus call reaches the node: the input focus where the node takes it,
    /// else the accessibility focus.
    pub fn focus_action(&self, object: Object) -> Option<NodeAction> {
        let (_, node) = self.node(object)?;
        Some(if node.supports(NodeAction::Focus) {
            NodeAction::Focus
        } else {
            NodeAction::AccessibilityFocus
        })
    }

    /// The action a new value for a slider asks for: Android moves a slider a step forward or
    /// backward.
    pub fn value_action(&self, object: Object, value: f64) -> Option<NodeAction> {
        let (_, node) = self.node(object)?;
        let current = f64::from(node.range?.current);
        let action = if value > current {
            NodeAction::ScrollForward
        } else if value < current {
            NodeAction::ScrollBackward
        } else {
            return None;
        };
        node.supports(action).then_some(action)
    }

    /// Asks Android to do `action` on the node through the running watch. An error that says
    /// why when the node is gone, does not do that, or no watch runs.
    pub fn request(
        &mut self,
        object: Object,
        action: NodeAction,
        text: String,
    ) -> Result<(), String> {
        let Object::Node { task_id, epoch, id } = object else {
            return Err("only an Android node takes an accessibility action".into());
        };
        let window = self
            .windows
            .get_mut(&task_id)
            .ok_or("the window has closed")?;
        let node = window.node(epoch, id).ok_or("the node is gone")?;
        // Android takes the accessibility focus on any node.
        if action != NodeAction::AccessibilityFocus && !node.supports(action) {
            return Err(format!("the node does not take {action}"));
        }
        window.next_request = window.next_request.wrapping_add(1);
        let request = ActionRequest {
            epoch,
            node_id: id,
            action,
            text,
            request_id: window.next_request,
        };
        match &window.actions {
            Some(actions) if actions.send(request).is_ok() => Ok(()),
            _ => Err("the window's tree is not being read from Android".into()),
        }
    }

    /// What action `index` of the object does: a node's action goes to Android through the
    /// watch, a button's comes back as the command for the compositor to carry out.
    pub fn perform(&mut self, object: Object, index: i32) -> Result<Option<TitleCommand>, String> {
        let actions = self.actions(object);
        let action = usize::try_from(index)
            .ok()
            .and_then(|at| actions.get(at))
            .ok_or_else(|| format!("the object has no action {index}"))?;
        match action.work {
            Work::Node(action) => self.request(object, action, String::new()).map(|()| None),
            Work::Title(action) => Ok(Some(TitleCommand {
                task_id: object.task_id().unwrap_or_default(),
                action,
            })),
        }
    }

    /// Lets the watch of the task's tree take actions on `actions`.
    pub fn attach_actions(&mut self, task_id: i32, actions: Sender<ActionRequest>) {
        if let Some(window) = self.windows.get_mut(&task_id) {
            window.actions = Some(actions);
        }
    }

    /// A window opened, or what it shows changed. Returns the events that raises.
    pub fn sync(&mut self, task_id: i32, facts: WindowFacts) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(window) = self.windows.get_mut(&task_id) else {
            let active = facts.active;
            let title = facts.title.clone();
            self.windows
                .insert(task_id, WindowAccess::new(facts, self.grace));
            let index = self
                .windows
                .keys()
                .position(|id| *id == task_id)
                .unwrap_or_default() as i32;
            events.push(Event::ChildAdded {
                parent: Object::Application,
                index,
                child: Object::Window(task_id),
            });
            events.push(Event::Window {
                task_id,
                kind: WindowEvent::Create,
                title: title.clone(),
            });
            if active {
                events.push(Event::Window {
                    task_id,
                    kind: WindowEvent::Activate,
                    title,
                });
                events.push(Event::State {
                    object: Object::Window(task_id),
                    state: "active",
                    on: true,
                });
            }
            return events;
        };
        let before = std::mem::replace(&mut window.facts, facts);
        let after = &window.facts;
        let frame = Object::Window(task_id);
        if before.title != after.title {
            events.push(Event::Name {
                object: frame,
                name: after.title.clone(),
            });
        }
        let (old, new) = (buttons_of(&before), buttons_of(after));
        child_events(
            frame,
            0,
            &old,
            &new,
            |button| Some(Object::Button(task_id, button)),
            &mut events,
        );
        if before.maximized != after.maximized && new.contains(&TitleButton::Maximize) {
            events.push(Event::Name {
                object: Object::Button(task_id, TitleButton::Maximize),
                name: button_name(TitleButton::Maximize, after.maximized, self.korean).into(),
            });
        }
        if before.shown != after.shown || before.scale != after.scale {
            events.push(Event::Bounds {
                object: frame,
                rect: window.frame_rect(),
            });
        }
        for button in &new {
            let moved = old.contains(button)
                && (before.scale != after.scale
                    || cell_of(&before, *button) != cell_of(after, *button));
            if moved {
                if let Some(rect) = window.button_rect(*button) {
                    events.push(Event::Bounds {
                        object: Object::Button(task_id, *button),
                        rect,
                    });
                }
            }
        }
        // A new scale moves every node on the screen without the task changing.
        if before.scale != after.scale || before.frame_scale != after.frame_scale {
            let mut ids: Vec<u32> = window.tree.roots().to_vec();
            let mut at = 0;
            while at < ids.len() && ids.len() <= window.tree.len() {
                if let Some(node) = window.tree.node(ids[at]) {
                    ids.extend(node.children.iter().copied());
                }
                at += 1;
            }
            for id in ids.into_iter().take(window.tree.len()) {
                let (Some(object), Some(node)) =
                    (window.node_object(task_id, id), window.tree.node(id))
                else {
                    continue;
                };
                events.push(Event::Bounds {
                    object,
                    rect: window.node_rect(node.bounds),
                });
            }
        }
        if before.active != after.active {
            events.push(Event::Window {
                task_id,
                kind: if after.active {
                    WindowEvent::Activate
                } else {
                    WindowEvent::Deactivate
                },
                title: after.title.clone(),
            });
            events.push(Event::State {
                object: frame,
                state: "active",
                on: after.active,
            });
            if after.active {
                if let Some(focus) = window
                    .tree
                    .focused()
                    .and_then(|id| window.node_object(task_id, id))
                {
                    events.push(Event::State {
                        object: focus,
                        state: "focused",
                        on: true,
                    });
                }
            }
        }
        events
    }

    /// A window closed. Returns the events that raises.
    pub fn close(&mut self, task_id: i32) -> Vec<Event> {
        let Some(index) = self.windows.keys().position(|id| *id == task_id) else {
            return Vec::new();
        };
        let window = self.windows.remove(&task_id);
        vec![
            Event::ChildRemoved {
                parent: Object::Application,
                index: index as i32,
                child: Object::Window(task_id),
            },
            Event::Window {
                task_id,
                kind: WindowEvent::Destroy,
                title: window.map(|window| window.facts.title).unwrap_or_default(),
            },
        ]
    }

    /// Takes the guest's tree or change for the task's window. Returns the events it raises;
    /// an update that cannot be taken changes nothing.
    pub fn apply(&mut self, task_id: i32, update: TreeUpdate) -> Result<Vec<Event>, TreeError> {
        let Some(window) = self.windows.get_mut(&task_id) else {
            return Ok(Vec::new());
        };
        let before = Before::of(&window.tree, &update);
        let changes = window.tree.apply(update)?;
        Ok(events_of(task_id, window, &before, &changes))
    }

    /// Forgets the task's tree, as when the guest stops reading it. Returns the events that
    /// raises.
    pub fn clear(&mut self, task_id: i32) -> Vec<Event> {
        let Some(window) = self.windows.get_mut(&task_id) else {
            return Vec::new();
        };
        let offset = window.buttons().len() as i32;
        let mut events = Vec::new();
        if let Some(epoch) = window.tree.epoch() {
            child_events(
                Object::Window(task_id),
                offset,
                window.tree.roots(),
                &[],
                |id| Some(Object::Node { task_id, epoch, id }),
                &mut events,
            );
        }
        window.tree.clear();
        events
    }
}

fn cell_of(facts: &WindowFacts, button: TitleButton) -> Option<PixelRect> {
    let cells = facts.caption?;
    match button {
        TitleButton::Back => cells.back,
        TitleButton::Minimize => Some(cells.minimize),
        TitleButton::Maximize => Some(cells.maximize),
        TitleButton::Close => Some(cells.close),
    }
}

/// What a tree was before an update, as its events need it.
struct Before {
    epoch: Option<u32>,
    roots: Vec<u32>,
    focused: Option<u32>,
    /// The children of each node the update sends again.
    children: HashMap<u32, Vec<u32>>,
    /// Every node's name.
    names: HashMap<u32, String>,
}

impl Before {
    fn of(tree: &Tree, update: &TreeUpdate) -> Before {
        let children = update
            .nodes
            .iter()
            .filter_map(|node| Some((node.id, tree.node(node.id)?.children.clone())))
            .collect();
        let mut names = HashMap::with_capacity(tree.len());
        let mut stack: Vec<u32> = tree.roots().to_vec();
        while let Some(id) = stack.pop() {
            if let Some(node) = tree.node(id) {
                names.insert(id, tree.name(id));
                stack.extend(node.children.iter().copied());
            }
        }
        Before {
            epoch: tree.epoch(),
            roots: tree.roots().to_vec(),
            focused: tree.focused(),
            children,
            names,
        }
    }
}

/// The events of one update the window's tree took.
fn events_of(
    task_id: i32,
    window: &WindowAccess,
    before: &Before,
    changes: &Changes,
) -> Vec<Event> {
    let mut events = Vec::new();
    let tree = &window.tree;
    let Some(epoch) = tree.epoch() else {
        return events;
    };
    let frame = Object::Window(task_id);
    let offset = window.buttons().len() as i32;
    let node = |id: u32| Object::Node { task_id, epoch, id };
    if changes.replaced {
        // Every node of the old tree is gone with its epoch; the new one is all new.
        if let Some(old_epoch) = before.epoch {
            child_events(
                frame,
                offset,
                &before.roots,
                &[],
                |id| {
                    Some(Object::Node {
                        task_id,
                        epoch: old_epoch,
                        id,
                    })
                },
                &mut events,
            );
        }
        child_events(
            frame,
            offset,
            &[],
            tree.roots(),
            |id| Some(node(id)),
            &mut events,
        );
    } else {
        if changes.roots_changed {
            child_events(
                frame,
                offset,
                &before.roots,
                tree.roots(),
                |id| Some(node(id)),
                &mut events,
            );
        }
        for id in &changes.children_changed {
            let (Some(old), Some(new)) = (before.children.get(id), tree.node(*id)) else {
                continue;
            };
            child_events(
                node(*id),
                0,
                old,
                &new.children,
                |child| Some(node(child)),
                &mut events,
            );
        }
        for old in &changes.changed {
            let Some(new) = tree.node(old.id) else {
                continue;
            };
            let object = node(new.id);
            let state = |name: &'static str, on: bool| Event::State {
                object,
                state: name,
                on,
            };
            if old.states.checked != new.states.checked {
                events.push(state("checked", new.states.checked));
                if new.role() == dsa_host_core::accessibility::Role::Switch {
                    events.push(state("pressed", new.states.checked));
                }
            }
            if old.states.selected != new.states.selected {
                events.push(state("selected", new.states.selected));
            }
            if old.states.enabled != new.states.enabled {
                events.push(state("enabled", new.states.enabled));
                events.push(state("sensitive", new.states.enabled));
            }
            if old.state_description != new.state_description {
                events.push(Event::Description {
                    object,
                    description: new.state_description.clone(),
                });
            }
            if new.states.editable {
                let (was, is) = (
                    old.value().unwrap_or_default(),
                    new.value().unwrap_or_default(),
                );
                if let Some((start, removed, inserted)) = text::difference(&was, &is) {
                    if !removed.is_empty() {
                        events.push(Event::TextRemoved {
                            object,
                            start,
                            text: removed,
                        });
                    }
                    if !inserted.is_empty() {
                        events.push(Event::TextInserted {
                            object,
                            start,
                            text: inserted,
                        });
                    }
                }
            }
            if let (Some(was), Some(is)) = (old.range, new.range) {
                if was.current != is.current {
                    events.push(Event::Value {
                        object,
                        value: f64::from(is.current),
                    });
                }
            }
            if old.bounds != new.bounds {
                events.push(Event::Bounds {
                    object,
                    rect: window.node_rect(new.bounds),
                });
            }
        }
        // A node's name may come from the nodes inside it (a list row reads as its title), so
        // a change can rename the nodes above it: look up from each changed node.
        let mut named: HashSet<u32> = HashSet::new();
        let starts = changes
            .changed
            .iter()
            .map(|old| old.id)
            .chain(changes.children_changed.iter().copied())
            .chain(changes.added.iter().copied());
        'names: for start in starts {
            let mut at = Some(start);
            while let Some(id) = at {
                if named.len() >= MAX_NAME_EVENTS {
                    break 'names;
                }
                let Some(was) = before.names.get(&id) else {
                    // A node new in this update: its name is read with it.
                    at = tree.parent(id);
                    continue;
                };
                let name = tree.name(id);
                if &name != was && named.insert(id) {
                    events.push(Event::Name {
                        object: node(id),
                        name,
                    });
                }
                at = tree.parent(id);
            }
        }
    }
    if let Some(focus) = changes.focus {
        if window.facts.active {
            if let Some(old) = before
                .focused
                .filter(|old| *old != focus && !changes.replaced)
            {
                events.push(Event::State {
                    object: node(old),
                    state: "focused",
                    on: false,
                });
            }
            events.push(Event::State {
                object: node(focus),
                state: "focused",
                on: true,
            });
        }
    }
    events
}
