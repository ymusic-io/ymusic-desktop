//! The loading spinner. The app runs with reduce motion on (no enter/exit transitions), which also
//! freezes gpui's animations, but a spinner is a progress indicator, not decoration: this one turns
//! by the clock and asks for the next frame itself.

use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::{Icon, IconName, Sizable, Size};
use gpui_kit::*;

const TURN_MS: u128 = 900;

#[derive(IntoElement)]
pub struct Spinner {
    size: Size,
}

impl Spinner {
    pub fn new() -> Self {
        Self { size: Size::Medium }
    }

    pub fn small(mut self) -> Self {
        self.size = Size::Small;
        self
    }

    pub fn large(mut self) -> Self {
        self.size = Size::Large;
        self
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for Spinner {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let millis =
            SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_millis()).unwrap_or_default();
        let turn = (millis % TURN_MS) as f32 / TURN_MS as f32;
        window.request_animation_frame();
        div()
            .child(Icon::new(IconName::Loader).with_size(self.size).transform(Transformation::rotate(percentage(turn))))
    }
}
