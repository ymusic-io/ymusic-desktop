//! The Sound panel: speed and pitch, the ten-band equalizer with presets, bass, virtualizer and
//! preamp. Every change applies at once and is saved by Playback.

use gpui_kit::base::slider::{SliderEvent, SliderState};
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::*;
use ym_i18n::{Str, t, tf};
use ym_mse::Tone;

use crate::a11y::{FocusList, SliderStop};
use crate::audio_effects::{AudioEffects, EQ_PRESETS, PITCH_RANGE, PREAMP_RANGE};
use crate::playback::Playback;

const BAND_LABELS: [&str; 10] = ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];
const EQ_MAX_DB: f32 = 12.0;
const BASS_MAX_DB: f32 = 12.0;
const SPEED_RANGE: (f32, f32) = (0.25, 2.0);
const VIRTUALIZER_MIN: f32 = 0.5;
const VIRTUALIZER_MAX: f32 = 2.0;

/// Two linear halves, so the neutral 1× sits mid-slider.
fn virtualizer_fraction(width: f32) -> f32 {
    if width <= 1.0 {
        (width - VIRTUALIZER_MIN) / (1.0 - VIRTUALIZER_MIN) / 2.0
    } else {
        0.5 + (width - 1.0) / (VIRTUALIZER_MAX - 1.0) / 2.0
    }
}

fn virtualizer_width(fraction: f32) -> f32 {
    let width = if fraction <= 0.5 {
        VIRTUALIZER_MIN + fraction * 2.0 * (1.0 - VIRTUALIZER_MIN)
    } else {
        1.0 + (fraction - 0.5) * 2.0 * (VIRTUALIZER_MAX - 1.0)
    };
    (width * 20.0).round() / 20.0
}

#[derive(Clone, Copy)]
enum Control {
    Speed,
    Pitch,
    Band(usize),
    Bass,
    Virtualizer,
    Preamp,
}

pub struct SoundPanel {
    playback: Entity<Playback>,
    speed: Entity<SliderState>,
    pitch: Entity<SliderState>,
    bands: Vec<Entity<SliderState>>,
    bass: Entity<SliderState>,
    virtualizer: Entity<SliderState>,
    preamp: Entity<SliderState>,
    focus: FocusList,
    _subscriptions: Vec<Subscription>,
}

impl SoundPanel {
    pub fn open(playback: Entity<Playback>, window: &mut Window, cx: &mut App) {
        let panel = cx.new(|cx| SoundPanel::new(playback, cx));
        window
            .open_dialog(cx, move |dialog, _, _| dialog.title(t(Str::SoundTitle)).width(px(620.)).child(panel.clone()));
    }

    fn new(playback: Entity<Playback>, cx: &mut Context<Self>) -> Self {
        let effects = playback.read(cx).effects;
        let slider = |cx: &mut Context<Self>, min: f32, max: f32, step: f32, value: f32| {
            cx.new(|_| SliderState::new().min(min).max(max).step(step).default_value(value))
        };
        let speed = slider(cx, SPEED_RANGE.0, SPEED_RANGE.1, 0.05, effects.speed);
        let pitch = slider(cx, PITCH_RANGE.0, PITCH_RANGE.1, 0.05, effects.pitch);
        let bands: Vec<_> = effects.tone.eq_db.iter().map(|db| slider(cx, -EQ_MAX_DB, EQ_MAX_DB, 1.0, *db)).collect();
        let bass = slider(cx, 0.0, BASS_MAX_DB, 1.0, effects.tone.bass_db);
        let virtualizer = slider(cx, 0.0, 1.0, 0.025, virtualizer_fraction(effects.tone.virtualizer));
        let preamp = slider(cx, PREAMP_RANGE.0, PREAMP_RANGE.1, 0.5, effects.tone.preamp_db);
        let mut subscriptions = vec![cx.observe(&playback, |_, _, cx| cx.notify())];
        let mut watch = |state: &Entity<SliderState>, control: Control, cx: &mut Context<Self>| {
            subscriptions.push(cx.subscribe(state, move |panel, _, event: &SliderEvent, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                panel.apply(control, value.end(), cx);
            }));
        };
        watch(&speed, Control::Speed, cx);
        watch(&pitch, Control::Pitch, cx);
        for (index, band) in bands.iter().enumerate() {
            watch(band, Control::Band(index), cx);
        }
        watch(&bass, Control::Bass, cx);
        watch(&virtualizer, Control::Virtualizer, cx);
        watch(&preamp, Control::Preamp, cx);
        let focus = FocusList::modal(None, cx);
        Self { playback, speed, pitch, bands, bass, virtualizer, preamp, focus, _subscriptions: subscriptions }
    }

    fn apply(&mut self, control: Control, value: f32, cx: &mut Context<Self>) {
        let mut effects = self.playback.read(cx).effects;
        match control {
            Control::Speed => effects.speed = value,
            Control::Pitch => effects.pitch = value,
            Control::Band(index) => effects.tone.eq_db[index] = value,
            Control::Bass => effects.tone.bass_db = value,
            Control::Virtualizer => effects.tone.virtualizer = virtualizer_width(value),
            Control::Preamp => effects.tone.preamp_db = value,
        }
        self.playback.update(cx, |playback, cx| playback.set_effects(effects, cx));
    }

    /// Moves every slider to `effects` and applies them.
    fn load(&mut self, effects: AudioEffects, window: &mut Window, cx: &mut Context<Self>) {
        let set = |state: &Entity<SliderState>, value: f32, window: &mut Window, cx: &mut Context<Self>| {
            state.update(cx, |slider, cx| slider.set_value(value, window, cx));
        };
        set(&self.speed, effects.speed, window, cx);
        set(&self.pitch, effects.pitch, window, cx);
        for (band, db) in self.bands.clone().iter().zip(effects.tone.eq_db) {
            set(band, db, window, cx);
        }
        set(&self.bass, effects.tone.bass_db, window, cx);
        set(&self.virtualizer, virtualizer_fraction(effects.tone.virtualizer), window, cx);
        set(&self.preamp, effects.tone.preamp_db, window, cx);
        self.playback.update(cx, |playback, cx| playback.set_effects(effects, cx));
    }
}

fn rate(value: f32) -> String {
    format!("{}×", (value * 100.0).round() / 100.0)
}

impl Render for SoundPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let effects = self.playback.read(cx).effects;
        let preset = EQ_PRESETS
            .iter()
            .find(|(_, bands)| *bands == effects.tone.eq_db)
            .map(|(label, _)| t(*label))
            .unwrap_or_else(|| t(Str::SoundPresetCustom));
        let label = |text: String| div().w(px(110.)).text_sm().child(text);
        let slider_row = |index: usize, name: String, state: &Entity<SliderState>, value: String, cx: &App| {
            let stop = SliderStop {
                value_text: Some(value.clone().into()),
                ..SliderStop::new(("sound-slider", index), name.clone())
            };
            h_flex()
                .gap_3()
                .items_center()
                .child(label(name.clone()))
                .child(self.focus.slider(index, state, stop, Slider::new(state).accessibility_label(name), cx).flex_1())
                .child(div().w(px(56.)).text_xs().text_color(theme.muted_foreground).child(value))
        };
        let bands = self.bands.len();
        let (preset_stop, bass_stop) = (2, 3 + bands);

        let panel = cx.entity().downgrade();
        let presets =
            Button::new("preset").small().label(preset).track_focus(&self.focus.handle(preset_stop, cx)).dropdown_menu(
                move |mut menu, _, _| {
                    for (label, bands) in EQ_PRESETS {
                        let panel = panel.clone();
                        menu = menu.item(PopupMenuItem::new(t(label)).on_click(move |_, window, cx| {
                            panel
                                .update(cx, |panel, cx| {
                                    let mut effects = panel.playback.read(cx).effects;
                                    effects.tone.eq_db = bands;
                                    panel.load(effects, window, cx);
                                })
                                .ok();
                        }));
                    }
                    menu
                },
            );

        let mut faders = h_flex().gap_2().justify_between().h(px(180.));
        for (index, band) in self.bands.iter().enumerate() {
            faders = faders.child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{:+}", effects.tone.eq_db[index] as i32)),
                    )
                    .child({
                        let name = format!("{}, {}", t(Str::SoundEq), BAND_LABELS[index]);
                        let stop = SliderStop {
                            value_text: Some(tf(Str::SoundDb, &[&effects.tone.eq_db[index]]).into()),
                            vertical: true,
                            ..SliderStop::new(("eq-band", index), name.clone())
                        };
                        let slider = Slider::new(band).vertical().accessibility_label(name);
                        self.focus.slider(3 + index, band, stop, slider, cx).flex_1()
                    })
                    .child(div().text_xs().child(BAND_LABELS[index])),
            );
        }

        v_flex()
            .gap_3()
            .child(div().text_sm().font_semibold().child(t(Str::SoundPlayback)))
            .child(slider_row(0, t(Str::SoundSpeed).to_owned(), &self.speed, rate(effects.speed), cx))
            .child(slider_row(1, tf(Str::SoundPitch, &[&""]).trim().to_owned(), &self.pitch, rate(effects.pitch), cx))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().font_semibold().child(t(Str::SoundEq)))
                    .child(self.focus.wrap(preset_stop, presets, cx)),
            )
            .child(faders)
            .child(slider_row(
                bass_stop,
                t(Str::SoundBass).to_owned(),
                &self.bass,
                tf(Str::SoundDb, &[&effects.tone.bass_db]),
                cx,
            ))
            .child(slider_row(
                bass_stop + 1,
                t(Str::SoundVirtualizerTitle).to_owned(),
                &self.virtualizer,
                rate(effects.tone.virtualizer),
                cx,
            ))
            .child(slider_row(
                bass_stop + 2,
                t(Str::A11yEqPreamp).to_owned(),
                &self.preamp,
                tf(Str::SoundDb, &[&effects.tone.preamp_db]),
                cx,
            ))
            .child(h_flex().justify_end().child(self.focus.button(
                bass_stop + 3,
                Button::new("reset").small().label(t(Str::SoundReset)).on_click(cx.listener(|panel, _, window, cx| {
                    let speed_pitch = panel.playback.read(cx).effects;
                    let defaults = AudioEffects { tone: Tone::default(), ..speed_pitch };
                    panel.load(AudioEffects { speed: 1.0, pitch: 1.0, ..defaults }, window, cx);
                })),
                cx,
            )))
    }
}
