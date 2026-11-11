//! Keyboard reach for lists and grids, the way the JVM app's `listKeyboardNavigation` works: every
//! item is a real focus stop (Tab reaches it, Enter or Space activates it, the screen reader
//! announces it), the arrow keys move between items, and an arrow past a list's edge moves on to
//! the next or previous list. The focused item is ringed and scrolled into view.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::{Rc, Weak};

use gpui_kit::component::ActiveTheme;
use gpui_kit::*;

const RING: f32 = 2.;
/// Room kept between a revealed item and the scroll viewport's edge.
const REVEAL_MARGIN: f32 = 12.;

#[derive(Clone, Copy, PartialEq)]
pub enum Layout {
    /// Up/Down move; Left/Right leave the list.
    Vertical,
    /// Left/Right move; Up/Down leave the list.
    Horizontal,
    /// Left/Right move by one, Up/Down by a row of this many.
    Grid(usize),
}

struct Slot {
    handle: FocusHandle,
    was_focused: Cell<bool>,
}

/// Every list's item handles, so an arrow past one list's edge can land in another.
#[derive(Default)]
pub struct RovingItems(RefCell<Vec<Weak<Slot>>>);

impl Global for RovingItems {}

impl RovingItems {
    fn register(cx: &App, slot: &Rc<Slot>) {
        let mut slots = cx.global::<RovingItems>().0.borrow_mut();
        slots.retain(|slot| slot.strong_count() > 0);
        slots.push(Rc::downgrade(slot));
    }

    fn contains(cx: &App, handle: &FocusHandle) -> bool {
        cx.global::<RovingItems>()
            .0
            .borrow()
            .iter()
            .any(|slot| slot.upgrade().is_some_and(|slot| slot.handle == *handle))
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(RovingItems::default());
}

/// The focus handles of one list's items, kept across frames by item position.
pub struct FocusList {
    slots: RefCell<Vec<Rc<Slot>>>,
    scroll: Option<ScrollHandle>,
}

impl FocusList {
    /// `scroll` is the scroll container the items live in, to reveal a focused item.
    pub fn new(scroll: Option<ScrollHandle>) -> Self {
        Self { slots: RefCell::default(), scroll }
    }

    fn slot(&self, index: usize, cx: &App) -> Rc<Slot> {
        let mut slots = self.slots.borrow_mut();
        while slots.len() <= index {
            let slot = Rc::new(Slot { handle: cx.focus_handle().tab_stop(true), was_focused: Cell::new(false) });
            RovingItems::register(cx, &slot);
            slots.push(slot);
        }
        slots[index].clone()
    }

    pub fn handle(&self, index: usize, cx: &App) -> FocusHandle {
        self.slot(index, cx).handle.clone()
    }

    /// Makes `element` the list's `index`th item. It needs an id and a role of its own.
    pub fn item(&self, index: usize, element: Stateful<Div>, cx: &App) -> Stateful<Div> {
        let slot = self.slot(index, cx);
        let ring = cx.theme().ring;
        let scroll = self.scroll.clone();
        let watched = slot.clone();
        element
            .track_focus(&slot.handle)
            .relative()
            .focus_visible(move |style| {
                style.shadow(vec![BoxShadow {
                    color: ring,
                    offset: point(px(0.), px(0.)),
                    blur_radius: px(0.),
                    spread_radius: px(RING),
                    inset: true,
                }])
            })
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let focused = watched.handle.is_focused(window);
                        if focused
                            && !watched.was_focused.get()
                            && let Some(scroll) = &scroll
                        {
                            reveal(scroll, bounds, window);
                        }
                        watched.was_focused.set(focused);
                    },
                )
                .absolute()
                .size_full(),
            )
    }

    /// Arrow keys over the items in `items`, laid out as `layout`.
    pub fn container<E: InteractiveElement>(&self, element: E, items: Range<usize>, layout: Layout, cx: &App) -> E {
        let handles: Vec<FocusHandle> = items.map(|index| self.handle(index, cx)).collect();
        element.on_key_down(move |event: &KeyDownEvent, window, cx| {
            let modifiers = &event.keystroke.modifiers;
            if modifiers.platform || modifiers.control || modifiers.alt || modifiers.shift {
                return;
            }
            if handles.is_empty() {
                return;
            }
            let Some(current) = handles.iter().position(|handle| handle.is_focused(window)) else { return };
            let step: isize = match (layout, event.keystroke.key.as_str()) {
                (Layout::Vertical, "up") | (Layout::Horizontal | Layout::Grid(_), "left") => -1,
                (Layout::Vertical, "down") | (Layout::Horizontal | Layout::Grid(_), "right") => 1,
                (Layout::Grid(columns), "up") => -(columns.max(1) as isize),
                (Layout::Grid(columns), "down") => columns.max(1) as isize,
                (Layout::Vertical, "left") | (Layout::Horizontal, "up") => {
                    if leave(&handles, false, window, cx) {
                        cx.stop_propagation();
                    }
                    return;
                }
                (Layout::Vertical, "right") | (Layout::Horizontal, "down") => {
                    if leave(&handles, true, window, cx) {
                        cx.stop_propagation();
                    }
                    return;
                }
                (_, "home") => -(current as isize),
                (_, "end") => (handles.len() - 1 - current) as isize,
                _ => return,
            };
            let target = current as isize + step;
            if (0..handles.len() as isize).contains(&target) {
                window.focus(&handles[target as usize], cx);
                cx.stop_propagation();
            } else if leave(&handles, step > 0, window, cx) {
                cx.stop_propagation();
            }
        })
    }
}

/// Moves focus along the tab order to the nearest item of another list; stays put if there is none.
fn leave(own: &[FocusHandle], forward: bool, window: &mut Window, cx: &mut App) -> bool {
    let Some(start) = window.focused(cx) else { return false };
    let mut visited = vec![start.clone()];
    loop {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
        let Some(focused) = window.focused(cx) else { break };
        if visited.contains(&focused) {
            break;
        }
        if !own.contains(&focused) && RovingItems::contains(cx, &focused) {
            return true;
        }
        visited.push(focused);
    }
    window.focus(&start, cx);
    false
}

/// Scrolls `scroll` the least that shows `item` (both in window coordinates).
fn reveal(scroll: &ScrollHandle, item: Bounds<Pixels>, window: &mut Window) {
    let viewport = scroll.bounds();
    let offset = scroll.offset();
    let max = scroll.max_offset();
    let margin = px(REVEAL_MARGIN);
    let mut y = offset.y;
    if item.bottom() + margin > viewport.bottom() {
        y -= item.bottom() + margin - viewport.bottom();
    }
    if item.top() - margin < viewport.top() {
        y += viewport.top() - (item.top() - margin);
    }
    let mut x = offset.x;
    if item.right() + margin > viewport.right() {
        x -= item.right() + margin - viewport.right();
    }
    if item.left() - margin < viewport.left() {
        x += viewport.left() - (item.left() - margin);
    }
    let y = y.clamp(-max.y, px(0.));
    let x = x.clamp(-max.x, px(0.));
    if (x, y) != (offset.x, offset.y) {
        scroll.set_offset(point(x, y));
        // Runs during paint, where a refresh is ignored.
        window.on_next_frame(|window, _| window.refresh());
    }
}

/// A single control drawn as a div: a tab stop with the focus ring. It needs an id and a role.
pub trait KeyboardFocusable {
    fn keyboard_focusable(self, ring: Hsla) -> Self;
}

impl KeyboardFocusable for Stateful<Div> {
    fn keyboard_focusable(self, ring: Hsla) -> Self {
        self.tab_index(0).focus_visible(move |style| {
            style.shadow(vec![BoxShadow {
                color: ring,
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(RING),
                inset: true,
            }])
        })
    }
}

pub fn focusable(element: Stateful<Div>, cx: &App) -> Stateful<Div> {
    element.keyboard_focusable(cx.theme().ring)
}

/// Debug builds: gpui's own warnings (an unannounced focus, a node without a role) go to stderr.
pub fn install_debug_log() {
    struct Stderr;
    impl log::Log for Stderr {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Info
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                eprintln!("[{} {}] {}", record.level(), record.target(), record.args());
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: Stderr = Stderr;
    if cfg!(debug_assertions) && log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// Debug: `YMUSIC_DEV_A11Y_DUMP=<file>` writes the accessibility tree there every second while an
/// assistive client is connected.
pub fn dev_dump_tree<V: 'static>(window: &mut Window, cx: &mut Context<V>) {
    let Some(path) = std::env::var_os("YMUSIC_DEV_A11Y_DUMP").filter(|_| cfg!(debug_assertions)) else { return };
    let executor = cx.background_executor().clone();
    cx.spawn_in(window, async move |_, cx| {
        loop {
            executor.timer(std::time::Duration::from_secs(1)).await;
            let dump = cx.update(|window, _| (window.is_a11y_active(), window.debug_a11y_tree_json()));
            let Ok((active, json)) = dump else { return };
            let text = format!("active={active}\nappkit_focus={}\n{}", appkit_focus(), json.unwrap_or_default());
            if let Err(error) = std::fs::write(&path, text) {
                eprintln!("YMUSIC_DEV_A11Y_DUMP: {error}");
                return;
            }
        }
    })
    .detach();
}

/// What AppKit answers when VoiceOver asks the app for its focused element.
#[cfg(target_os = "macos")]
fn appkit_focus() -> String {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};

    fn text(object: Option<Retained<AnyObject>>) -> String {
        let Some(object) = object else { return "nil".into() };
        // SAFETY: an NSString (or nil), read on the main thread.
        unsafe {
            let utf8: *const std::ffi::c_char = msg_send![&*object, UTF8String];
            if utf8.is_null() { "?".into() } else { std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned() }
        }
    }
    // SAFETY: NSApplication / NSAccessibility queries on the main thread.
    unsafe {
        let Some(app_class) = AnyClass::get(c"NSApplication") else { return "no NSApplication".into() };
        let app: Retained<AnyObject> = msg_send![app_class, sharedApplication];
        let key_window: Option<Retained<AnyObject>> = msg_send![&*app, keyWindow];
        let responder = match &key_window {
            Some(window) => {
                let responder: Option<Retained<AnyObject>> = msg_send![&**window, firstResponder];
                responder.map(|responder| {
                    let class: Retained<AnyObject> = msg_send![&*responder, className];
                    text(Some(class))
                })
            }
            None => None,
        };
        let focused: Option<Retained<AnyObject>> = msg_send![&*app, accessibilityFocusedUIElement];
        let (role, label) = match &focused {
            Some(element) => {
                let role: Option<Retained<AnyObject>> = msg_send![&**element, accessibilityRole];
                let label: Option<Retained<AnyObject>> = msg_send![&**element, accessibilityTitle];
                (text(role), text(label))
            }
            None => ("nil".into(), "nil".into()),
        };
        let content = match &key_window {
            Some(window) => {
                let view: Option<Retained<AnyObject>> = msg_send![&**window, contentView];
                view.map(|view| {
                    let class: Retained<AnyObject> = msg_send![&*view, className];
                    let subviews: Retained<AnyObject> = msg_send![&*view, subviews];
                    let count: usize = msg_send![&*subviews, count];
                    let focused: Option<Retained<AnyObject>> = msg_send![&*view, accessibilityFocusedUIElement];
                    let focused_role = focused.map(|element| {
                        let role: Option<Retained<AnyObject>> = msg_send![&*element, accessibilityRole];
                        text(role)
                    });
                    format!(
                        "{}({count} subviews, ax_focus={})",
                        text(Some(class)),
                        focused_role.unwrap_or_else(|| "nil".into())
                    )
                })
            }
            None => None,
        };
        format!(
            "content_view={} key_window={} first_responder={} focused_role={role} focused_label={label}",
            content.unwrap_or_else(|| "nil".into()),
            key_window.is_some(),
            responder.unwrap_or_else(|| "nil".into())
        )
    }
}

#[cfg(not(target_os = "macos"))]
fn appkit_focus() -> String {
    String::new()
}
