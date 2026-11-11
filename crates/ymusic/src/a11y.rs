//! Keyboard reach the way the JVM app's Compose focus works: every list item, card, tab and chip is
//! a real focus stop (Tab reaches it, Enter or Space activates it, the screen reader announces it),
//! and an arrow key moves focus geometrically, to the nearest stop on screen in that direction,
//! preferring one that lines up (Android's FocusFinder rule), so focus enters and leaves lists in
//! screen order. The focused stop is ringed and scrolled into view.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::{Rc, Weak};

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::button::Button;
use gpui_kit::*;

const RING: f32 = 2.;
/// Room kept between a revealed item and the scroll viewport's edge.
const REVEAL_MARGIN: f32 = 12.;
/// FocusFinder's weight of the distance along the arrow against the sideways offset.
const MAJOR_WEIGHT: f32 = 13.;

struct Slot {
    handle: FocusHandle,
    was_focused: Cell<bool>,
    /// Window bounds as last painted, and the frame that painted it.
    bounds: Cell<Bounds<Pixels>>,
    painted: Cell<u64>,
}

/// Every focus stop on screen, for geometric arrow moves.
#[derive(Default)]
pub struct RovingItems {
    slots: RefCell<Vec<Weak<Slot>>>,
    frame: Cell<u64>,
}

impl Global for RovingItems {}

impl RovingItems {
    fn register(cx: &App, slot: &Rc<Slot>) {
        let mut slots = cx.global::<RovingItems>().slots.borrow_mut();
        slots.retain(|slot| slot.strong_count() > 0);
        slots.push(Rc::downgrade(slot));
    }

    /// The stops the latest frame painted, which is what is on screen.
    fn painted(cx: &App) -> Vec<Rc<Slot>> {
        let items = cx.global::<RovingItems>();
        let frame = items.frame.get();
        items.slots.borrow().iter().filter_map(Weak::upgrade).filter(|slot| slot.painted.get() == frame).collect()
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(RovingItems::default());
}

/// Paint this first in the window (before any stop) so stops from earlier frames drop out.
pub fn frame_marker() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, _, cx| {
            let items = cx.global::<RovingItems>();
            items.frame.set(items.frame.get() + 1);
        },
    )
    .absolute()
    .size_0()
}

#[derive(Clone, Copy, PartialEq)]
enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// FocusFinder's ranking of `to` as the next stop from `from`; `None` when it is not that way.
fn rank(from: Bounds<Pixels>, to: Bounds<Pixels>, direction: Direction) -> Option<(bool, f32)> {
    let (fl, fr, ft, fb) =
        (f32::from(from.left()), f32::from(from.right()), f32::from(from.top()), f32::from(from.bottom()));
    let (tl, tr, tt, tb) = (f32::from(to.left()), f32::from(to.right()), f32::from(to.top()), f32::from(to.bottom()));
    let candidate = match direction {
        Direction::Left => (fr > tr || fl >= tr) && fl > tl,
        Direction::Right => (fl < tl || fr <= tl) && fr < tr,
        Direction::Up => (fb > tb || ft >= tb) && ft > tt,
        Direction::Down => (ft < tt || fb <= tt) && fb < tb,
    };
    if !candidate {
        return None;
    }
    let (major, minor, in_beam) = match direction {
        Direction::Left => ((fl - tr).max(0.), ((ft + fb) - (tt + tb)).abs() / 2., tb > ft && tt < fb),
        Direction::Right => ((tl - fr).max(0.), ((ft + fb) - (tt + tb)).abs() / 2., tb > ft && tt < fb),
        Direction::Up => ((ft - tb).max(0.), ((fl + fr) - (tl + tr)).abs() / 2., tr > fl && tl < fr),
        Direction::Down => ((tt - fb).max(0.), ((fl + fr) - (tl + tr)).abs() / 2., tr > fl && tl < fr),
    };
    Some((!in_beam, MAJOR_WEIGHT * major * major + minor * minor))
}

/// Moves focus to the best stop in `direction`; false when there is none.
fn step(direction: Direction, window: &mut Window, cx: &mut App) -> bool {
    let slots = RovingItems::painted(cx);
    let Some(current) = slots.iter().find(|slot| slot.handle.is_focused(window)) else { return false };
    let from = current.bounds.get();
    let best = slots
        .iter()
        .filter(|slot| !Rc::ptr_eq(slot, current))
        .filter_map(|slot| rank(from, slot.bounds.get(), direction).map(|rank| (rank, slot)))
        .min_by(|(left, _), (right, _)| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let Some((_, target)) = best else { return false };
    let handle = target.handle.clone();
    window.focus(&handle, cx);
    true
}

/// Arrow keys move focus geometrically from any stop inside `element`.
fn on_arrows<E: InteractiveElement>(element: E) -> E {
    element.on_key_down(|event: &KeyDownEvent, window, cx| {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.shift {
            return;
        }
        let direction = match event.keystroke.key.as_str() {
            "left" => Direction::Left,
            "right" => Direction::Right,
            "up" => Direction::Up,
            "down" => Direction::Down,
            _ => return,
        };
        if step(direction, window, cx) {
            cx.stop_propagation();
        }
    })
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
            let slot = Rc::new(Slot {
                handle: cx.focus_handle().tab_stop(true),
                was_focused: Cell::new(false),
                bounds: Cell::new(Bounds::default()),
                painted: Cell::new(0),
            });
            RovingItems::register(cx, &slot);
            slots.push(slot);
        }
        slots[index].clone()
    }

    pub fn handle(&self, index: usize, cx: &App) -> FocusHandle {
        self.slot(index, cx).handle.clone()
    }

    /// Records the stop's bounds each frame and reveals it when it gains focus.
    fn tracker(&self, slot: Rc<Slot>) -> impl IntoElement {
        let scroll = self.scroll.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                slot.bounds.set(bounds);
                slot.painted.set(cx.global::<RovingItems>().frame.get());
                let focused = slot.handle.is_focused(window);
                if focused
                    && !slot.was_focused.get()
                    && let Some(scroll) = &scroll
                {
                    reveal(scroll, bounds, window);
                }
                slot.was_focused.set(focused);
            },
        )
        .absolute()
        .size_full()
    }

    /// Makes `element` the list's `index`th stop. It needs an id and a role of its own.
    pub fn item(&self, index: usize, element: Stateful<Div>, cx: &App) -> Stateful<Div> {
        let slot = self.slot(index, cx);
        let ring = cx.theme().ring;
        on_arrows(
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
                .child(self.tracker(slot)),
        )
    }

    /// Makes `button` the list's `index`th stop (a chip, a pill), reachable by the arrows too.
    pub fn button(&self, index: usize, button: Button, cx: &App) -> Div {
        let button = button.track_focus(&self.handle(index, cx));
        self.wrap(index, button, cx)
    }

    /// Makes `element`, already tracking [`Self::handle`]`(index)`, the list's `index`th stop.
    pub fn wrap(&self, index: usize, element: impl IntoElement, cx: &App) -> Div {
        let slot = self.slot(index, cx);
        on_arrows(div().relative().child(element).child(self.tracker(slot)))
    }

    /// Home and End over the stops in `items`.
    pub fn container<E: InteractiveElement>(&self, element: E, items: Range<usize>, cx: &App) -> E {
        let handles: Vec<FocusHandle> = items.map(|index| self.handle(index, cx)).collect();
        element.on_key_down(move |event: &KeyDownEvent, window, cx| {
            if !handles.iter().any(|handle| handle.is_focused(window)) {
                return;
            }
            let target = match event.keystroke.key.as_str() {
                "home" => handles.first(),
                "end" => handles.last(),
                _ => None,
            };
            if let Some(target) = target {
                window.focus(target, cx);
                cx.stop_propagation();
            }
        })
    }
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
