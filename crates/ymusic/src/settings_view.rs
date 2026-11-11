//! Settings: appearance, playback, downloads, account, privacy and about.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme, Colorize, IconName, Sizable, StyledExt, Theme, ThemeMode, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_engine::yt::{YtAccountInfo, YtLocales};
use ym_i18n::{Str, t, tf};
use ym_settings::SettingsStore;

use crate::accent::{ACCENT_KEY, AccentPicker, accent_color};
use crate::caption_style::{CaptionLook, CaptionStyleDialog};
use crate::downloads::{DOWNLOAD_LOCATION_KEY, download_folder};
use crate::feedback_view::link;
use crate::graph::{AppGraph, on_core};
use crate::settings_lists::{Licenses, RegionPicker};
use crate::sponsor_block::SPONSOR_BLOCK_KEY;

pub const THEME_KEY: &str = "app.theme";
pub const LANGUAGE_KEY: &str = "app.uiLanguage";
const CRASH_REPORTS_KEY: &str = "telemetry.crashReports";
const STATS_KEY: &str = "telemetry.stats";
pub const SHOW_FPS_KEY: &str = "debug.showFps";
const SOURCE_CODE_URL: &str = "https://github.com/ymusic-io/YMusicV4";
const TERMS_URL: &str = "https://github.com/ymusic-io/YMusicV4/blob/master/TERMS.md";
const CHANGELOG_URL: &str = "https://github.com/ymusic-io/YMusicV4/blob/master/CHANGELOG.md";

/// The saved theme: unset is dark (as in the JVM app), blank follows the system.
fn saved_theme(settings: &dyn SettingsStore) -> String {
    settings.get_raw(THEME_KEY).ok().flatten().unwrap_or_else(|| "dark".to_owned())
}

pub fn apply_saved_theme(settings: &dyn SettingsStore, cx: &mut App) {
    apply_theme(&saved_theme(settings), &saved(settings, ACCENT_KEY), cx);
}

/// `light`, `dark`, `amoled` (dark on pure black) or anything else for the system's, with the
/// accent (`RRGGBB`, blank = default) on every primary surface.
fn apply_theme(choice: &str, accent: &str, cx: &mut App) {
    match choice {
        "light" => Theme::change(ThemeMode::Light, None, cx),
        "dark" | "amoled" => Theme::change(ThemeMode::Dark, None, cx),
        _ => Theme::sync_system_appearance(None, cx),
    }
    let accent = accent_color(accent);
    let on_accent = white();
    let amoled = choice == "amoled";
    Theme::update(cx, |theme| {
        theme.primary = accent;
        theme.primary_hover = accent.lighten(0.1);
        theme.primary_active = accent.darken(0.1);
        theme.primary_foreground = on_accent;
        theme.button_primary = accent;
        theme.button_primary_hover = accent.lighten(0.1);
        theme.button_primary_active = accent.darken(0.1);
        theme.button_primary_foreground = on_accent;
        theme.sidebar_primary = accent;
        theme.sidebar_primary_foreground = on_accent;
        theme.progress_bar = accent;
        theme.slider_bar = accent;
        theme.switch_thumb = on_accent;
        if amoled {
            let ground = black();
            theme.background = ground;
            theme.sidebar = ground;
            theme.popover = ground;
            theme.colors.list = ground;
            theme.tab_bar = ground;
            theme.title_bar = ground;
            theme.accordion = ground;
        }
    });
}

/// Writes a setting off the UI thread, then redraws every window so its readers see it.
pub fn save_setting(key: &'static str, value: String, cx: &mut App) {
    let graph = cx.global::<AppGraph>();
    let (settings, log) = (graph.settings.clone(), graph.log.fork("settings"));
    let write = graph.tokio.spawn_blocking(move || settings.put_raw(key, value));
    cx.spawn(async move |cx| {
        match write.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => log.warn(&format!("R93000 setting {key} not saved"), Some(error.as_ref())),
            Err(error) => log.warn(&format!("R93000 setting {key} not saved"), Some(&error)),
        }
        cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

/// The saved UI language, else the system's.
pub fn apply_language(settings: &dyn SettingsStore) {
    match settings.get_raw(LANGUAGE_KEY).ok().flatten().filter(|tag| !tag.is_empty()) {
        Some(tag) => {
            ym_i18n::set_locale(&tag);
        }
        None => {
            ym_i18n::use_system_locale();
        }
    }
}

fn saved(settings: &dyn SettingsStore, key: &str) -> String {
    settings.get_raw(key).ok().flatten().unwrap_or_default()
}

/// The content locale changed: locale-shaped feeds refetch.
pub struct LocaleChanged;

pub struct SettingsView {
    playback: Entity<crate::playback::Playback>,
    account: Option<YtAccountInfo>,
    signing_out: bool,
    /// The working region and the region menu, from the engine; None until the first answer.
    pub locales: Option<YtLocales>,
    pub locales_loading: bool,
    scroll: ScrollHandle,
}

impl EventEmitter<LocaleChanged> for SettingsView {}

impl SettingsView {
    pub fn new(playback: Entity<crate::playback::Playback>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            playback,
            account: None,
            signing_out: false,
            locales: None,
            locales_loading: false,
            scroll: ScrollHandle::new(),
        };
        view.refresh_account(cx);
        view.load_locales(cx);
        view
    }

    fn load_locales(&mut self, cx: &mut Context<Self>) {
        self.locales_loading = true;
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine(), graph.log.fork("settings"));
        let load = on_core(cx, async move {
            let result = engine.locale_options(&log).await;
            if let Err(error) = &result {
                log.warn("locale options fetch failed", Some(error));
            }
            result
        });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |view, cx| {
                view.locales_loading = false;
                if let Some(Ok(locales)) = result {
                    view.locales = Some(locales);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Tells the engine about a region pick (`Some`) or a language change (`None`), then re-asks for
    /// the menus (their labels follow the locale) and has the feeds refetch. A failed notify still
    /// refreshes, as the engine may have applied part of it.
    fn apply_locale(&mut self, gl: Option<String>, window: Option<AnyWindowHandle>, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine(), graph.log.fork("settings"));
        let apply = on_core(cx, async move {
            let result = engine.apply_locale(gl.as_deref(), &log).await;
            if let Err(error) = &result {
                log.warn("applying the content locale failed", Some(error));
            }
            result
        });
        cx.spawn(async move |this, cx| {
            let result = apply.await;
            this.update(cx, |view, cx| {
                if let (Some(Err(error)), Some(window)) = (&result, window) {
                    let message = crate::feed::error_text(error);
                    window.update(cx, |_, window, cx| window.push_notification(message, cx)).ok();
                }
                view.load_locales(cx);
                cx.emit(LocaleChanged);
            })
            .ok();
        })
        .detach();
    }

    /// Debug: shows the last rows.
    pub fn scroll_to_end(&self) {
        self.scroll.scroll_to_bottom();
    }

    pub fn pick_region(&mut self, gl: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(locales) = &mut self.locales {
            locales.gl = gl.clone();
        }
        cx.notify();
        self.apply_locale(Some(gl), Some(window.window_handle()), cx);
    }

    pub fn refresh_account(&mut self, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine(), graph.log.fork("settings"));
        let load = on_core(cx, async move { engine.account_info(false, &log).await });
        cx.spawn(async move |this, cx| {
            if let Some(Ok(account)) = load.await {
                this.update(cx, |view, cx| {
                    view.account = Some(account);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn put(&self, key: &'static str, value: String, cx: &mut Context<Self>) {
        save_setting(key, value, cx);
        cx.notify();
    }

    fn set_theme(&mut self, choice: &'static str, cx: &mut Context<Self>) {
        let accent = saved(cx.global::<AppGraph>().settings.as_ref(), ACCENT_KEY);
        apply_theme(choice, &accent, cx);
        self.put(THEME_KEY, choice.to_owned(), cx);
    }

    fn set_accent(&mut self, hex: String, cx: &mut Context<Self>) {
        let theme = saved_theme(cx.global::<AppGraph>().settings.as_ref());
        apply_theme(&theme, &hex, cx);
        self.put(ACCENT_KEY, hex, cx);
    }

    fn set_language(&mut self, tag: String, window: &mut Window, cx: &mut Context<Self>) {
        if tag.is_empty() {
            ym_i18n::use_system_locale();
        } else {
            ym_i18n::set_locale(&tag);
        }
        // Saved before the notify: the engine re-reads the language from settings.
        let graph = cx.global::<AppGraph>();
        let (settings, log) = (graph.settings.clone(), graph.log.fork("settings"));
        let write = graph.tokio.spawn_blocking(move || settings.put_raw(LANGUAGE_KEY, tag));
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            match write.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => log.warn("R93001 UI language not saved", Some(error.as_ref())),
                Err(error) => log.warn("R93001 UI language not saved", Some(&error)),
            }
            this.update(cx, |view, cx| view.apply_locale(None, Some(handle), cx)).ok();
        })
        .detach();
        cx.refresh_windows();
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let pick = rfd::AsyncFileDialog::new().set_title(t(Str::FolderPickerTitle)).pick_folder();
        cx.spawn(async move |this, cx| {
            let Some(folder) = pick.await else { return };
            this.update(cx, |view, cx| {
                if let Ok(uri) = url::Url::from_directory_path(folder.path()) {
                    view.put(DOWNLOAD_LOCATION_KEY, uri.to_string(), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.signing_out = true;
        let graph = cx.global::<AppGraph>();
        let (jar, engines, log) = (graph.cookies.clone(), graph.engines.clone(), graph.log.fork("signout"));
        let done = on_core(cx, async move { crate::signin::sign_out(jar, engines, &log).await });
        cx.spawn(async move |this, cx| {
            let result = done.await;
            this.update(cx, |view, cx| {
                view.signing_out = false;
                if let Some(Err(error)) = result {
                    cx.global::<AppGraph>().log.warn("sign-out failed", Some(&error));
                }
                view.refresh_account(cx);
            })
            .ok();
        })
        .detach();
    }
}

/// A header inset 32 (card margin + row padding) over a rounded card of borderless rows.
fn section(label: &'static str, rows: Vec<AnyElement>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .child(
            div()
                .id(label)
                .role(Role::Heading)
                .px_8()
                .pt_6()
                .pb_2()
                .text_sm()
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(v_flex().mx_4().rounded(px(12.)).bg(theme.secondary).overflow_hidden().children(rows))
}

fn row(title: impl Into<SharedString>, hint: Option<String>, control: impl IntoElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .gap_4()
        .px_4()
        .py(px(14.))
        .items_center()
        .child(
            v_flex().flex_1().min_w_0().child(div().text_sm().child(title.into())).when_some(hint, |column, hint| {
                column.child(div().pt(px(2.)).text_xs().text_color(theme.muted_foreground).child(hint))
            }),
        )
        .child(div().flex_shrink_0().child(control))
        .into_any_element()
}

fn language_name(tag: &str) -> String {
    let base = tag.split(['-', '_']).next().unwrap_or(tag);
    let name = match base {
        "en" => "English",
        "vi" => "Tiếng Việt",
        "es" => "Español",
        "pt" => "Português",
        "fr" => "Français",
        "de" => "Deutsch",
        "it" => "Italiano",
        "ru" => "Русский",
        "uk" => "Українська",
        "pl" => "Polski",
        "tr" => "Türkçe",
        "ar" => "العربية",
        "fa" => "فارسی",
        "hi" => "हिन्दी",
        "id" => "Bahasa Indonesia",
        "ms" => "Bahasa Melayu",
        "th" => "ไทย",
        "ja" => "日本語",
        "ko" => "한국어",
        "zh" => "中文",
        "nl" => "Nederlands",
        "sv" => "Svenska",
        "cs" => "Čeština",
        "ro" => "Română",
        "hu" => "Magyar",
        "el" => "Ελληνικά",
        "he" => "עברית",
        "bn" => "বাংলা",
        _ => return tag.to_owned(),
    };
    if tag.len() > base.len() { format!("{name} ({})", &tag[base.len() + 1..]) } else { name.to_owned() }
}

fn link_row(id: &'static str, title: &'static str, url: String, cx: &App) -> AnyElement {
    row(
        title,
        None,
        Button::new(id)
            .small()
            .ghost()
            .icon(IconName::ExternalLink)
            .accessibility_label(title)
            .tooltip(url.clone())
            .on_click(move |_, _, cx| cx.open_url(&url)),
        cx,
    )
}

impl SettingsView {
    fn toggle(
        &self,
        id: &'static str,
        key: &'static str,
        title: &'static str,
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let checked = saved(cx.global::<AppGraph>().settings.as_ref(), key) != "false";
        self.toggle_with(id, key, title, hint, checked, cx)
    }

    fn toggle_with(
        &self,
        id: &'static str,
        key: &'static str,
        title: &'static str,
        hint: &'static str,
        checked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        row(
            title,
            Some(hint.to_owned()),
            Switch::new(id)
                .checked(checked)
                .accessibility_label(title)
                .on_click(cx.listener(move |view, checked: &bool, _, cx| view.put(key, checked.to_string(), cx))),
            cx,
        )
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let graph = cx.global::<AppGraph>();
        let settings = graph.settings.clone();
        let theme_choice = saved_theme(settings.as_ref());
        let accent = saved(settings.as_ref(), ACCENT_KEY);
        let language = saved(settings.as_ref(), LANGUAGE_KEY);
        let show_fps = saved(settings.as_ref(), SHOW_FPS_KEY) == "true";
        let caption_look = CaptionLook::load(settings.as_ref());
        let folder: Option<PathBuf> = download_folder(settings.as_ref());
        let versions = format!(
            "{} · {} · {}",
            tf(Str::SettingsVersionsApp, &[&env!("CARGO_PKG_VERSION"), &crate::graph::APP_VERSION]),
            tf(Str::SettingsVersionsEngine, &[&graph.engines.backend_version()]),
            tf(Str::SettingsVersionsPlayer, &[&graph.player_source.embedded().1]),
        );
        let changelog_url = link(graph, "changelogUrl", CHANGELOG_URL);
        let source_url = link(graph, "sourceCodeUrl", SOURCE_CODE_URL);
        let terms_url = link(graph, "termsUrl", TERMS_URL);

        let theme_label = t(match theme_choice.as_str() {
            "light" => Str::SettingsThemeLight,
            "dark" => Str::SettingsThemeDark,
            "amoled" => Str::SettingsThemeAmoled,
            _ => Str::SettingsThemeSystem,
        });
        let entity = cx.entity().downgrade();
        let theme_menu =
            Button::new("theme").small().ghost().label(theme_label).dropdown_menu(move |mut menu, _, _| {
                for (choice, label) in [
                    ("", Str::SettingsThemeSystem),
                    ("light", Str::SettingsThemeLight),
                    ("dark", Str::SettingsThemeDark),
                    ("amoled", Str::SettingsThemeAmoled),
                ] {
                    let view = entity.clone();
                    menu = menu.item(PopupMenuItem::new(t(label)).on_click(move |_, _, cx| {
                        view.update(cx, |view, cx| view.set_theme(choice, cx)).ok();
                    }));
                }
                menu
            });

        let entity = cx.entity().downgrade();
        let accent_button = Button::new("accent")
            .small()
            .ghost()
            .accessibility_label(t(Str::SettingsAccentColor))
            .child(div().size(px(20.)).rounded_full().bg(accent_color(&accent)))
            .on_click(move |_, window, cx| {
                let view = entity.clone();
                AccentPicker::open(
                    &accent,
                    move |hex, cx| {
                        view.update(cx, |view, cx| view.set_accent(hex, cx)).ok();
                    },
                    window,
                    cx,
                );
            });

        let language_label =
            if language.is_empty() { t(Str::SettingsThemeSystem).to_owned() } else { language_name(&language) };
        let entity = cx.entity().downgrade();
        let language_menu =
            Button::new("language").small().ghost().label(language_label).dropdown_menu(move |mut menu, _, _| {
                menu = menu.scrollable(true).max_h(px(360.));
                let system = entity.clone();
                menu = menu.item(PopupMenuItem::new(t(Str::SettingsThemeSystem)).on_click(move |_, window, cx| {
                    system.update(cx, |view, cx| view.set_language(String::new(), window, cx)).ok();
                }));
                for tag in ym_i18n::available_locales() {
                    let view = entity.clone();
                    menu = menu.item(PopupMenuItem::new(language_name(tag)).on_click(move |_, window, cx| {
                        view.update(cx, |view, cx| view.set_language(tag.to_owned(), window, cx)).ok();
                    }));
                }
                menu
            });

        let region_label = self
            .locales
            .as_ref()
            .map(|locales| {
                locales
                    .regions
                    .iter()
                    .find(|region| region.code.eq_ignore_ascii_case(&locales.gl))
                    .map(|region| region.label.clone())
                    .unwrap_or_else(|| locales.gl.clone())
            })
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| t(Str::SettingsContentRegion).to_owned());
        let region_button = Button::new("region")
            .small()
            .ghost()
            .label(region_label)
            .loading(self.locales.is_none() && self.locales_loading)
            .on_click(cx.listener(|_, _, window, cx| RegionPicker::open(cx.entity(), window, cx)));

        let account_control: AnyElement = match &self.account {
            Some(account) if account.signed_in => Button::new("sign-out")
                .small()
                .label(t(Str::AccountSignOut))
                .loading(self.signing_out)
                .on_click(cx.listener(|view, _, _, cx| view.sign_out(cx)))
                .into_any_element(),
            _ => Button::new("sign-in")
                .small()
                .primary()
                .label(t(Str::SigninButton))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::shortcuts::OpenSignIn), cx))
                .into_any_element(),
        };
        let account_title = match &self.account {
            Some(account) if account.signed_in => {
                account.name.clone().or_else(|| account.email.clone()).unwrap_or_default()
            }
            _ => t(Str::SigninPrefaceTitle).to_owned(),
        };
        let account_hint = match &self.account {
            Some(account) if account.signed_in => account.email.clone().or_else(|| account.handle.clone()),
            _ => Some(t(Str::SigninPrefaceSubtitle).to_owned()),
        };

        let appearance = vec![
            row(t(Str::SettingsTheme), None, theme_menu, cx),
            row(t(Str::SettingsAccentColor), None, accent_button, cx),
            row(t(Str::SettingsLanguage), None, language_menu, cx),
        ];
        let content = vec![row(
            t(Str::SettingsContentRegion),
            Some(t(Str::SettingsContentLocaleHint).to_owned()),
            region_button,
            cx,
        )];
        let playback = vec![
            row(
                t(Str::SettingsSound),
                Some(t(Str::SettingsSoundHint).to_owned()),
                Button::new("sound").small().ghost().label(t(Str::SoundTitle)).on_click(cx.listener(
                    |view, _, window, cx| crate::sound_panel::SoundPanel::open(view.playback.clone(), window, cx),
                )),
                cx,
            ),
            row(
                t(Str::SettingsCaptionStyle),
                Some(t(Str::SettingsCaptionStyleHint).to_owned()),
                Button::new("caption-style")
                    .small()
                    .ghost()
                    .accessibility_label(t(Str::SettingsCaptionStyle))
                    .child(crate::caption_style::sample(caption_look))
                    .on_click(|_, window, cx| CaptionStyleDialog::open(window, cx)),
                cx,
            ),
            self.toggle(
                "sponsor-block",
                SPONSOR_BLOCK_KEY,
                t(Str::SettingsSponsorBlock),
                t(Str::SettingsSponsorBlockHint),
                cx,
            ),
        ];
        let downloads = vec![row(
            t(Str::MenuDownloadLocation),
            Some(
                folder
                    .map(|folder| folder.display().to_string())
                    .unwrap_or_else(|| t(Str::DownloadLocationNotSet).to_owned()),
            ),
            Button::new("download-folder")
                .small()
                .ghost()
                .label(t(Str::MenuDownloadLocation))
                .on_click(cx.listener(|view, _, _, cx| view.choose_folder(cx))),
            cx,
        )];
        let account = vec![row(account_title, account_hint, account_control, cx)];
        let privacy = vec![
            self.toggle(
                "crash-reports",
                CRASH_REPORTS_KEY,
                t(Str::SettingsCrashReports),
                t(Str::SettingsCrashReportsHint),
                cx,
            ),
            self.toggle("stats", STATS_KEY, t(Str::SettingsStats), t(Str::SettingsStatsHint), cx),
        ];
        let about = vec![
            row(t(Str::SettingsVersions), Some(versions), div(), cx),
            link_row("changelog", t(Str::SettingsChangelog), changelog_url, cx),
            link_row("source", t(Str::SettingsSourceCode), source_url, cx),
            link_row("terms", t(Str::SettingsTerms), terms_url, cx),
            row(
                t(Str::SettingsLicenses),
                None,
                Button::new("licenses")
                    .small()
                    .ghost()
                    .icon(IconName::ChevronRight)
                    .accessibility_label(t(Str::SettingsLicenses))
                    .on_click(|_, window, cx| Licenses::open(window, cx)),
                cx,
            ),
        ];
        let debug = vec![self.toggle_with(
            "show-fps",
            SHOW_FPS_KEY,
            t(Str::SettingsShowFps),
            t(Str::SettingsShowFpsHint),
            show_fps,
            cx,
        )];
        let content = v_flex()
            .w_full()
            .pb_6()
            .child(section(t(Str::SettingsSectionAppearance), appearance, cx))
            .child(section(t(Str::SettingsSectionContent), content, cx))
            .child(section(t(Str::SettingsSectionPlayback), playback, cx))
            .child(section(t(Str::SettingsSectionDownloads), downloads, cx))
            .child(section(t(Str::SettingsSectionDevices), account, cx))
            .child(section(t(Str::SettingsSectionPrivacy), privacy, cx))
            .child(section(t(Str::SettingsSectionAbout), about, cx))
            .child(section(t(Str::SettingsDebug), debug, cx));

        div().id("settings-scroll").size_full().overflow_y_scroll().track_scroll(&self.scroll).child(content)
    }
}
