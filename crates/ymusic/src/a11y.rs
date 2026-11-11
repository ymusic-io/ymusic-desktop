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
