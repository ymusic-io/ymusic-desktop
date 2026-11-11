//! The JVM app's `ActionIcon`: an icon-only action, its glyph centered in a fixed circular hit target.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{Icon, Sizable};
use gpui_kit::*;

/// The standard glyph, with 8 around it in the standard hit target.
const ICON: f32 = 24.;
pub const HIT: f32 = 40.;

/// A ghost button: `icon` at the standard size in the standard hit target, `label` spoken and shown as its tooltip.
pub fn action_icon(id: impl Into<ElementId>, icon: impl Into<Icon>, label: impl Into<SharedString>) -> Button {
    sized_action_icon(id, icon, label, ICON, HIT)
}

/// [`action_icon`] with the glyph at `icon_size` in a `hit_size` circle.
pub fn sized_action_icon(
    id: impl Into<ElementId>,
    icon: impl Into<Icon>,
    label: impl Into<SharedString>,
    icon_size: f32,
    hit_size: f32,
) -> Button {
    let label = label.into();
    hit_target(id, hit_size)
        .child(icon.into().with_size(px(icon_size)))
        .accessibility_label(label.clone())
        .tooltip(label)
}

/// An empty ghost button filling a `hit_size` circle, for content the caller draws (a spinner while busy).
pub fn hit_target(id: impl Into<ElementId>, hit_size: f32) -> Button {
    // The glyph goes in as a child: gpui-kit sizes an `.icon(..)` from the button's size.
    Button::new(id).ghost().size(px(hit_size)).p_0().rounded_full()
}
