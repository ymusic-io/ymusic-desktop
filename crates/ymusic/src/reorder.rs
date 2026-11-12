//! Drag-to-reorder for a list of rows, as the JVM app's `ReorderDragState`: a press on a row's
//! handle lifts the row, it follows the pointer above the others, rows swap as its center crosses a
//! neighbour, and the list scrolls while the pointer is held near its top or bottom edge.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::ActiveTheme;
use gpui_kit::*;

/// The pointer this near the list's top or bottom edge scrolls it while dragging a row.
const EDGE: f32 = 32.;
const SCROLL_MAX: f32 = 24.;
/// Above the dialogs (`10 + layer`), so a row lifted in one draws over its siblings; under popups.
const LIFTED_PRIORITY: usize = gpui_kit::base::POPUP_PRIORITY - 1;

pub enum ReorderEvent {
    /// The dragged row moved from `from` to `to`.
    Moved { from: usize, to: usize },
    /// The drag ended: the row that started at `from` lies at `to`.
    Dropped { from: usize, to: usize },
}

type OnEvent = Rc<dyn Fn(&ReorderEvent, &mut Window, &mut App)>;

/// One list's reorder state, shared by the view holding it, its rows ([`Self::row`]) and its
/// [`Self::tracker`].
#[derive(Clone)]
pub struct Reorder(Rc<State>);

struct State {
    scroll: ScrollHandle,
    drag: Cell<Option<Drag>>,
    rows: RefCell<Vec<Row>>,
    /// The view drawing the list, redrawn as the drag moves.
    view: Cell<Option<EntityId>>,
    on_event: RefCell<Option<OnEvent>>,
    autoscroll: RefCell<Option<Task<()>>>,
}

#[derive(Clone, Copy)]
struct Drag {
    start: usize,
    index: usize,
    pointer_y: Pixels,
    /// The pointer's distance below the row's top at the press.
    grab: Pixels,
}

#[derive(Default)]
struct Row {
    /// The row's slot in window coordinates, as last laid out.
    slot: Bounds<Pixels>,
}

impl Reorder {
    /// A list scrolled by `scroll`.
    pub fn new(scroll: ScrollHandle) -> Self {
        Self(Rc::new(State {
            scroll,
            drag: Cell::new(None),
            rows: RefCell::default(),
            view: Cell::new(None),
            on_event: RefCell::new(None),
            autoscroll: RefCell::new(None),
        }))
    }

    /// The row drawn lifted (dragged), for its look.
    pub fn lifted(&self) -> Option<usize> {
        self.0.drag.get().map(|drag| drag.index)
    }

    /// Lifts row `index` under a press at `pointer_y` (window coordinates) on its handle.
    pub fn start(&self, index: usize, pointer_y: Pixels, window: &mut Window, cx: &mut App) {
        let Some(top) = self.0.rows.borrow().get(index).map(|row| row.slot.top()) else { return };
        self.0.drag.set(Some(Drag { start: index, index, pointer_y, grab: pointer_y - top }));
        self.start_autoscroll(window, cx);
        self.redraw(cx);
    }

    /// Row `index` of the list: drawn where the drag puts it.
    pub fn row(&self, index: usize, row: impl IntoElement) -> ReorderRow {
        ReorderRow { reorder: self.clone(), index, child: Some(row.into_any_element()) }
    }

    /// Follows the pointer while a row is dragged; `on_event` hears each move and the drop. Draw it
    /// once per frame beside a list of `len` rows.
    pub fn tracker(
        &self,
        len: usize,
        on_event: impl Fn(&ReorderEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        self.0.rows.borrow_mut().resize_with(len, Row::default);
        *self.0.on_event.borrow_mut() = Some(Rc::new(on_event));
        let reorder = self.clone();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                reorder.0.view.set(Some(window.current_view()));
                if reorder.0.drag.get().is_none() {
                    return;
                }
                let moved = reorder.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    let Some(mut drag) = moved.0.drag.get() else { return };
                    if event.pressed_button != Some(MouseButton::Left) {
                        moved.end(window, cx);
                    } else {
                        drag.pointer_y = event.position.y;
                        moved.0.drag.set(Some(drag));
                        moved.follow(window, cx);
                        moved.redraw(cx);
                    }
                    cx.stop_propagation();
                });
                let released = reorder.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture || released.0.drag.get().is_none() {
                        return;
                    }
                    released.end(window, cx);
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_0()
    }

    /// Moves the dragged row to the slot its center is over.
    fn follow(&self, window: &mut Window, cx: &mut App) {
        let Some(mut drag) = self.0.drag.get() else { return };
        let target = {
            let rows = self.0.rows.borrow();
            let Some(dragged) = rows.get(drag.index) else { return };
            let center = dragged_top(&drag, &rows, self.0.scroll.bounds()) + dragged.slot.size.height / 2.;
            rows.iter().position(|row| center >= row.slot.top() && center < row.slot.bottom())
        };
        let Some(to) = target.filter(|to| *to != drag.index) else { return };
        let from = drag.index;
        {
            let mut rows = self.0.rows.borrow_mut();
            let row = rows.remove(from);
            rows.insert(to, row);
        }
        drag.index = to;
        self.0.drag.set(Some(drag));
        self.emit(&ReorderEvent::Moved { from, to }, window, cx);
    }

    fn end(&self, window: &mut Window, cx: &mut App) {
        let Some(drag) = self.0.drag.take() else { return };
        self.0.autoscroll.borrow_mut().take();
        self.emit(&ReorderEvent::Dropped { from: drag.start, to: drag.index }, window, cx);
        self.redraw(cx);
    }

    fn emit(&self, event: &ReorderEvent, window: &mut Window, cx: &mut App) {
        let on_event = self.0.on_event.borrow().clone();
        if let Some(on_event) = on_event {
            on_event(event, window, cx);
        }
    }

    fn redraw(&self, cx: &mut App) {
        if let Some(view) = self.0.view.get() {
            cx.notify(view);
        }
    }

    /// Scrolls the list while the dragged row is held near its top or bottom edge, until the drag ends.
    fn start_autoscroll(&self, window: &Window, cx: &App) {
        let state = Rc::downgrade(&self.0);
        let executor = cx.background_executor().clone();
        let task = window.spawn(cx, async move |cx| {
            loop {
                executor.timer(Duration::from_millis(16)).await;
                let Some(state) = state.upgrade() else { return };
                let reorder = Reorder(state);
                if !matches!(cx.update(|window, cx| reorder.autoscroll_step(window, cx)), Ok(true)) {
                    return;
                }
            }
        });
        *self.0.autoscroll.borrow_mut() = Some(task);
    }

    fn autoscroll_step(&self, window: &mut Window, cx: &mut App) -> bool {
        let Some(drag) = self.0.drag.get() else { return false };
        let scroll = &self.0.scroll;
        let viewport = scroll.bounds();
        let pointer = f32::from(drag.pointer_y);
        let (top, bottom) = (f32::from(viewport.top()), f32::from(viewport.bottom()));
        let overhang = if pointer > bottom - EDGE {
            pointer - (bottom - EDGE)
        } else if pointer < top + EDGE {
            pointer - (top + EDGE)
        } else {
            return true;
        };
        let step = (overhang * 0.5).clamp(-SCROLL_MAX, SCROLL_MAX);
        let offset = scroll.offset();
        let scrolled = (offset.y - px(step)).clamp(-scroll.max_offset().y, px(0.));
        if scrolled == offset.y {
            return true;
        }
        scroll.set_offset(point(offset.x, scrolled));
        for row in self.0.rows.borrow_mut().iter_mut() {
            row.slot.origin.y += scrolled - offset.y;
        }
        self.follow(window, cx);
        self.redraw(cx);
        true
    }
}

/// The dragged row's top: under the pointer, kept within the rows and the viewport.
fn dragged_top(drag: &Drag, rows: &[Row], viewport: Bounds<Pixels>) -> Pixels {
    let top = drag.pointer_y - drag.grab;
    let (Some(first), Some(last), Some(dragged)) = (rows.first(), rows.last(), rows.get(drag.index)) else {
        return top;
    };
    let highest = first.slot.top().max(viewport.top());
    let lowest = last.slot.bottom().min(viewport.bottom()) - dragged.slot.size.height;
    if lowest < highest { top } else { top.clamp(highest, lowest) }
}

/// A lifted row's look: raised on the popover ground with a shadow.
pub fn lift<E: Styled>(row: E, cx: &App) -> E {
    row.bg(cx.theme().popover).shadow(vec![BoxShadow {
        color: black().opacity(0.25),
        offset: point(px(0.), px(2.)),
        blur_radius: px(8.),
        spread_radius: px(0.),
        inset: false,
    }])
}

/// A row of a [`Reorder`] list; the dragged one draws over its siblings, clipped to the list.
pub struct ReorderRow {
    reorder: Reorder,
    index: usize,
    child: Option<AnyElement>,
}

impl IntoElement for ReorderRow {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for ReorderRow {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = self.child.as_mut().expect("a row is laid out once");
        (child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let state = &self.reorder.0;
        let drag = state.drag.get().filter(|drag| drag.index == self.index);
        let offset = {
            let mut rows = state.rows.borrow_mut();
            if let Some(row) = rows.get_mut(self.index) {
                row.slot = bounds;
            }
            match drag {
                Some(drag) => dragged_top(&drag, &rows, state.scroll.bounds()) - bounds.origin.y,
                None => px(0.),
            }
        };
        let Some(mut child) = self.child.take() else { return };
        if drag.is_some() {
            let origin = window.element_offset() + point(px(0.), offset);
            window.defer_draw(child, origin, LIFTED_PRIORITY, Some(window.content_mask()));
        } else {
            child.prepaint(window, cx);
            self.child = Some(child);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = self.child.as_mut() {
            child.paint(window, cx);
        }
    }
}
