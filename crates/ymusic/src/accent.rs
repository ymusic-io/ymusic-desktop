//! The accent color (`theme.accent`, `RRGGBB`, blank = the default orange) and its picker: preset
//! swatches, a hue slider and a hex field.

use gpui_kit::base::actions::Confirm;
use gpui_kit::base::slider::{SliderEvent, SliderState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::*;
use ym_i18n::{Str, t, tf};

use crate::a11y::{FocusList, SliderStop};

pub const ACCENT_KEY: &str = "theme.accent";
const DEFAULT_ACCENT: Rgb = Rgb::hex(0xE8732A);
/// The lightest an accent may be, so white text on it stays readable. The default sits right at it.
const MAX_ACCENT_LUMINANCE: f32 = 0.3;
const PRESETS: [Rgb; 8] = [
    DEFAULT_ACCENT,
    Rgb::hex(0xD32F2F),
    Rgb::hex(0xC2185B),
    Rgb::hex(0x7B1FA2),
    Rgb::hex(0x341C9A),
    Rgb::hex(0x1565C0),
    Rgb::hex(0x00796B),
    Rgb::hex(0x2E7D32),
];
const HUE_MAX: f32 = 359.99;

type OnPick = Box<dyn Fn(String, &mut App)>;

/// Gamma-encoded sRGB channels in 0..1.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rgb {
    red: f32,
    green: f32,
    blue: f32,
}

impl Rgb {
    const fn hex(value: u32) -> Self {
        Self {
            red: ((value >> 16) & 0xFF) as f32 / 255.,
            green: ((value >> 8) & 0xFF) as f32 / 255.,
            blue: (value & 0xFF) as f32 / 255.,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim_start_matches('#');
        if text.len() != 6 {
            return None;
        }
        u32::from_str_radix(text, 16).ok().map(Self::hex)
    }

    pub fn to_hex(self) -> String {
        let channel = |value: f32| ((value * 255. + 0.5) as i32).clamp(0, 255);
        format!("{:02X}{:02X}{:02X}", channel(self.red), channel(self.green), channel(self.blue))
    }

    pub fn hsla(self) -> Hsla {
        Rgba { r: self.red, g: self.green, b: self.blue, a: 1. }.into()
    }

    fn scaled(self, factor: f32) -> Self {
        Self { red: self.red * factor, green: self.green * factor, blue: self.blue * factor }
    }

    /// Relative luminance (sRGB transfer, Rec. 709 weights), as Compose's `Color.luminance()`.
    pub fn luminance(self) -> f32 {
        let linear = |value: f32| {
            if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * linear(self.red) + 0.7152 * linear(self.green) + 0.0722 * linear(self.blue)
    }

    /// Same hue and saturation, darkened until the luminance is at most [`MAX_ACCENT_LUMINANCE`].
    fn darkened(self) -> Self {
        if self.luminance() <= MAX_ACCENT_LUMINANCE {
            return self;
        }
        let (mut low, mut high) = (0f32, 1f32);
        for _ in 0..16 {
            let mid = (low + high) / 2.;
            if self.scaled(mid).luminance() <= MAX_ACCENT_LUMINANCE {
                low = mid;
            } else {
                high = mid;
            }
        }
        self.scaled(low)
    }

    /// `(hue 0..360, saturation 0..1, value 0..1)`.
    fn hsv(self) -> (f32, f32, f32) {
        let max = self.red.max(self.green).max(self.blue);
        let min = self.red.min(self.green).min(self.blue);
        let delta = max - min;
        let hue = if delta == 0. {
            0.
        } else if max == self.red {
            60. * ((self.green - self.blue) / delta).rem_euclid(6.)
        } else if max == self.green {
            60. * ((self.blue - self.red) / delta + 2.)
        } else {
            60. * ((self.red - self.green) / delta + 4.)
        };
        let saturation = if max == 0. { 0. } else { delta / max };
        (hue.clamp(0., HUE_MAX), saturation, max)
    }

    fn from_hsv(hue: f32, saturation: f32, value: f32) -> Self {
        let channel = |offset: f32| {
            let k = (offset + hue / 60.) % 6.;
            value - value * saturation * k.min(4. - k).clamp(0., 1.)
        };
        Self { red: channel(5.), green: channel(3.), blue: channel(1.) }
    }
}

/// The saved accent (`RRGGBB`) as drawn; blank or malformed is the default.
pub fn accent_color(saved: &str) -> Hsla {
    accent_rgb(saved).hsla()
}

fn accent_rgb(saved: &str) -> Rgb {
    Rgb::parse(saved).map(Rgb::darkened).unwrap_or(DEFAULT_ACCENT)
}

/// The highest value [`Rgb::darkened`] allows at this hue and saturation.
fn max_value(hue: f32, saturation: f32) -> f32 {
    Rgb::from_hsv(hue, saturation, 1.).darkened().hsv().2
}

pub struct AccentPicker {
    hue: f32,
    saturation: f32,
    value: f32,
    hue_slider: Entity<SliderState>,
    hex: Entity<InputState>,
    focus: FocusList,
    on_pick: OnPick,
    _subscriptions: Vec<Subscription>,
}

impl AccentPicker {
    /// `on_pick` gets the hex to save: blank when the pick is the default.
    pub fn open(saved: &str, on_pick: impl Fn(String, &mut App) + 'static, window: &mut Window, cx: &mut App) {
        let start = accent_rgb(saved);
        let view = cx.new(|cx| Self::new(start, Box::new(on_pick), window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(t(Str::SettingsAccentColor)).width(px(420.)).child(view.clone())
        });
    }

    fn new(start: Rgb, on_pick: OnPick, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (hue, saturation, value) = start.hsv();
        let hue_slider = cx.new(|_| SliderState::new().min(0.).max(HUE_MAX).step(1.).default_value(hue));
        let hex = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(start.to_hex(), window, cx);
            state
        });
        let slider_events = cx.subscribe_in(&hue_slider, window, |view, _, event: &SliderEvent, window, cx| {
            let (SliderEvent::Change(hue) | SliderEvent::Release(hue)) = event;
            view.hue = hue.end();
            let hex = view.picked().to_hex();
            view.hex.update(cx, |state, cx| state.set_value(hex, window, cx));
            cx.notify();
        });
        let hex_events = cx.subscribe_in(&hex, window, |view, input, event: &InputEvent, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let typed = input.read(cx).value().to_string();
            if let Some(color) = Rgb::parse(&typed)
                && color.darkened().to_hex() != view.picked().to_hex()
            {
                view.set_color(color, window, cx);
            }
        });
        Self {
            hue,
            saturation,
            value,
            hue_slider,
            hex,
            focus: FocusList::modal(None, cx),
            on_pick,
            _subscriptions: vec![slider_events, hex_events],
        }
    }

    fn picked(&self) -> Rgb {
        Rgb::from_hsv(self.hue, self.saturation, self.value.min(max_value(self.hue, self.saturation)))
    }

    fn set_color(&mut self, color: Rgb, window: &mut Window, cx: &mut Context<Self>) {
        let (hue, saturation, value) = color.darkened().hsv();
        (self.hue, self.saturation, self.value) = (hue, saturation, value);
        self.hue_slider.update(cx, |slider, cx| slider.set_value(hue, window, cx));
        cx.notify();
    }

    fn set_color_and_hex(&mut self, color: Rgb, window: &mut Window, cx: &mut Context<Self>) {
        self.set_color(color, window, cx);
        let hex = self.picked().to_hex();
        self.hex.update(cx, |state, cx| state.set_value(hex, window, cx));
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hex = self.picked().to_hex();
        let saved = if hex == DEFAULT_ACCENT.to_hex() { String::new() } else { hex };
        (self.on_pick)(saved, cx);
        window.close_dialog(cx);
    }
}

impl Render for AccentPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let picked = self.picked();
        let selected_hex = picked.to_hex();
        let name = t(Str::SettingsAccentColor);
        let mut swatches = h_flex().flex_wrap().gap_2();
        for (index, preset) in PRESETS.iter().enumerate() {
            let color = preset.darkened();
            let selected = color.to_hex() == selected_hex;
            let preset = *preset;
            let swatch = div()
                .id(("accent-preset", index))
                .role(Role::RadioButton)
                .aria_label(format!("{name}, {}", tf(Str::PlayAgainTrackOf, &[&(index + 1), &PRESETS.len()])))
                .aria_toggled(if selected { Toggled::True } else { Toggled::False })
                .size(px(32.))
                .p(px(4.))
                .rounded_full()
                .border_2()
                .border_color(if selected { theme.foreground } else { transparent_black() })
                .cursor_pointer()
                .child(div().size_full().rounded_full().bg(color.hsla()))
                .on_click(cx.listener(move |view, _, window, cx| view.set_color_and_hex(preset, window, cx)))
                .on_action(
                    cx.listener(move |view, _: &Confirm, window, cx| view.set_color_and_hex(preset, window, cx)),
                );
            swatches = swatches.child(self.focus.item(index, swatch, cx));
        }
        let hue_stops: Vec<Hsla> =
            (0..=6).map(|step| Rgb::from_hsv((step as f32 * 60.).min(HUE_MAX), 1., 1.).hsla()).collect();
        let mut hue_bar = h_flex().h(px(10.)).rounded(px(5.)).overflow_hidden();
        for pair in hue_stops.windows(2) {
            hue_bar = hue_bar.child(div().flex_1().h_full().bg(linear_gradient(
                90.,
                linear_color_stop(pair[0], 0.),
                linear_color_stop(pair[1], 1.),
            )));
        }
        let stops = PRESETS.len();
        let reset = Button::new("accent-reset")
            .small()
            .label(t(Str::SoundReset))
            .on_click(cx.listener(|view, _, window, cx| view.set_color_and_hex(DEFAULT_ACCENT, window, cx)));
        let ok = Button::new("accent-ok")
            .small()
            .primary()
            .label(t(Str::SoundOk))
            .on_click(cx.listener(|view, _, window, cx| view.confirm(window, cx)));
        self.focus.container(
            v_flex()
                .gap_4()
                .child(swatches)
                .child(v_flex().gap_2().child(hue_bar).child(self.focus.slider(
                    stops + 2,
                    &self.hue_slider,
                    SliderStop::new("accent-hue", name),
                    Slider::new(&self.hue_slider).accessibility_label(name),
                    cx,
                )))
                .child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(
                            div().size(px(36.)).rounded_full().border_1().border_color(theme.border).bg(picked.hsla()),
                        )
                        .child(div().text_color(theme.muted_foreground).font_family("monospace").child("#"))
                        .child(div().w(px(120.)).child(Input::new(&self.hex).small()))
                        .child(div().flex_1()),
                )
                .child(
                    h_flex().justify_end().gap_2().child(self.focus.button(stops, reset, cx)).child(self.focus.button(
                        stops + 1,
                        ok,
                        cx,
                    )),
                ),
            0..stops + 2,
            cx,
        )
    }
}
