//! Settings: appearance, playback, downloads, account, privacy and about.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, Theme, ThemeMode, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_engine::yt::YtAccountInfo;
use ym_i18n::{Str, t, tf};
use ym_settings::SettingsStore;

use crate::downloads::{DOWNLOAD_LOCATION_KEY, download_folder};
use crate::graph::{AppGraph, on_core};
use crate::signin_dialog::SignInDialog;
use crate::sponsor_block::SPONSOR_BLOCK_KEY;

pub const THEME_KEY: &str = "app.theme";
pub const LANGUAGE_KEY: &str = "app.uiLanguage";
const CRASH_REPORTS_KEY: &str = "telemetry.crashReports";
const SOURCE_CODE_URL: &str = "https://github.com/ymusic-io/YMusicV4";

/// `system`, `light` or `dark`.
pub fn apply_theme(choice: &str, cx: &mut App) {
    match choice {
        "light" => Theme::change(ThemeMode::Light, None, cx),
        "dark" => Theme::change(ThemeMode::Dark, None, cx),
        _ => Theme::sync_system_appearance(None, cx),
    }
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

pub struct SettingsView {
    playback: Entity<crate::playback::Playback>,
    account: Option<YtAccountInfo>,
    signing_out: bool,
    scroll: ScrollHandle,
}

impl SettingsView {
    pub fn new(playback: Entity<crate::playback::Playback>, cx: &mut Context<Self>) -> Self {
        let mut view = Self { playback, account: None, signing_out: false, scroll: ScrollHandle::new() };
        view.refresh_account(cx);
        view
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
        let graph = cx.global::<AppGraph>();
        let (settings, log) = (graph.settings.clone(), graph.log.fork("settings"));
        graph.tokio.spawn_blocking(move || {
            if let Err(error) = settings.put_raw(key, value) {
                log.warn(&format!("{key} could not be saved: {error}"), None);
            }
        });
        cx.notify();
    }

    fn set_theme(&mut self, choice: &'static str, cx: &mut Context<Self>) {
        self.put(THEME_KEY, choice.to_owned(), cx);
        apply_theme(choice, cx);
    }

    fn set_language(&mut self, tag: String, cx: &mut Context<Self>) {
        if tag.is_empty() {
            ym_i18n::use_system_locale();
        } else {
            ym_i18n::set_locale(&tag);
        }
        self.put(LANGUAGE_KEY, tag, cx);
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

fn heading(label: &'static str, cx: &App) -> impl IntoElement {
    div().pt_4().pb_1().text_sm().font_semibold().text_color(cx.theme().muted_foreground).child(label)
}

fn row(title: impl Into<SharedString>, hint: Option<String>, control: impl IntoElement, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .gap_4()
        .py_2()
        .border_b_1()
        .border_color(theme.border)
        .child(
            v_flex().flex_1().min_w_0().child(div().text_sm().child(title.into())).when_some(hint, |column, hint| {
                column.child(div().text_xs().text_color(theme.muted_foreground).child(hint))
            }),
        )
        .child(div().flex_shrink_0().child(control))
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

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let graph = cx.global::<AppGraph>();
        let settings = graph.settings.clone();
        let theme_choice = saved(settings.as_ref(), THEME_KEY);
        let language = saved(settings.as_ref(), LANGUAGE_KEY);
        let sponsor_block = saved(settings.as_ref(), SPONSOR_BLOCK_KEY) != "false";
        let crash_reports = saved(settings.as_ref(), CRASH_REPORTS_KEY) != "false";
        let folder: Option<PathBuf> = download_folder(settings.as_ref());
        let versions = format!(
            "{} · {} · {}",
            tf(Str::SettingsVersionsApp, &[&env!("CARGO_PKG_VERSION"), &crate::graph::APP_VERSION]),
            tf(Str::SettingsVersionsEngine, &[&graph.engines.backend_version()]),
            tf(Str::SettingsVersionsPlayer, &[&graph.player_source.embedded().1]),
        );

        let theme_label = t(match theme_choice.as_str() {
            "light" => Str::SettingsThemeLight,
            "dark" => Str::SettingsThemeDark,
            _ => Str::SettingsThemeSystem,
        });
        let entity = cx.entity().downgrade();
        let theme_menu = Button::new("theme").small().label(theme_label).dropdown_menu(move |mut menu, _, _| {
            for (choice, label) in [
                ("system", Str::SettingsThemeSystem),
                ("light", Str::SettingsThemeLight),
                ("dark", Str::SettingsThemeDark),
            ] {
                let view = entity.clone();
                menu = menu.item(PopupMenuItem::new(t(label)).on_click(move |_, _, cx| {
                    view.update(cx, |view, cx| view.set_theme(choice, cx)).ok();
                }));
            }
            menu
        });

        let language_label =
            if language.is_empty() { t(Str::SettingsThemeSystem).to_owned() } else { language_name(&language) };
        let entity = cx.entity().downgrade();
        let language_menu =
            Button::new("language").small().label(language_label).dropdown_menu(move |mut menu, _, _| {
                menu = menu.scrollable(true).max_h(px(360.));
                let system = entity.clone();
                menu = menu.item(PopupMenuItem::new(t(Str::SettingsThemeSystem)).on_click(move |_, _, cx| {
                    system.update(cx, |view, cx| view.set_language(String::new(), cx)).ok();
                }));
                for tag in ym_i18n::available_locales() {
                    let view = entity.clone();
                    menu = menu.item(PopupMenuItem::new(language_name(tag)).on_click(move |_, _, cx| {
                        view.update(cx, |view, cx| view.set_language(tag.to_owned(), cx)).ok();
                    }));
                }
                menu
            });

        let account_control: AnyElement = match &self.account {
            Some(account) if account.signed_in => Button::new("sign-out")
                .small()
                .label(t(Str::AccountSignOut))
                .loading(self.signing_out)
                .on_click(cx.listener(|view, _, _, cx| view.sign_out(cx)))
                .into_any_element(),
            _ => {
                let entity = cx.entity().downgrade();
                Button::new("sign-in")
                    .small()
                    .primary()
                    .label(t(Str::SigninButton))
                    .on_click(move |_, window, cx| {
                        let view = entity.clone();
                        SignInDialog::open(window, cx, move |cx| {
                            view.update(cx, |view, cx| view.refresh_account(cx)).ok();
                        });
                    })
                    .into_any_element()
            }
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

        let content = v_flex()
            .max_w(px(720.))
            .w_full()
            .child(div().text_xl().font_semibold().child(t(Str::SectionSettings)))
            .child(heading(t(Str::SettingsSectionAppearance), cx))
            .child(row(t(Str::SettingsTheme), None, theme_menu, cx))
            .child(row(t(Str::SettingsLanguage), None, language_menu, cx))
            .child(heading(t(Str::SettingsSectionPlayback), cx))
            .child(row(
                t(Str::SettingsSound),
                Some(t(Str::SettingsSoundHint).to_owned()),
                Button::new("sound").small().label(t(Str::SoundTitle)).on_click(cx.listener(|view, _, window, cx| {
                    crate::sound_panel::SoundPanel::open(view.playback.clone(), window, cx)
                })),
                cx,
            ))
            .child(row(
                t(Str::SettingsSponsorBlock),
                Some(t(Str::SettingsSponsorBlockHint).to_owned()),
                Switch::new("sponsor-block").checked(sponsor_block).on_click(
                    cx.listener(|view, checked: &bool, _, cx| view.put(SPONSOR_BLOCK_KEY, checked.to_string(), cx)),
                ),
                cx,
            ))
            .child(heading(t(Str::SettingsSectionDownloads), cx))
            .child(row(
                t(Str::MenuDownloadLocation),
                Some(
                    folder
                        .map(|folder| folder.display().to_string())
                        .unwrap_or_else(|| t(Str::DownloadLocationNotSet).to_owned()),
                ),
                Button::new("download-folder")
                    .small()
                    .label(t(Str::MenuDownloadLocation))
                    .on_click(cx.listener(|view, _, _, cx| view.choose_folder(cx))),
                cx,
            ))
            .child(heading(t(Str::SettingsSectionDevices), cx))
            .child(row(account_title, account_hint, account_control, cx))
            .child(heading(t(Str::SettingsSectionPrivacy), cx))
            .child(row(
                t(Str::SettingsCrashReports),
                Some(t(Str::SettingsCrashReportsHint).to_owned()),
                Switch::new("crash-reports").checked(crash_reports).on_click(
                    cx.listener(|view, checked: &bool, _, cx| view.put(CRASH_REPORTS_KEY, checked.to_string(), cx)),
                ),
                cx,
            ))
            .child(heading(t(Str::SettingsSectionAbout), cx))
            .child(row(t(Str::SettingsVersions), Some(versions), div(), cx))
            .child(row(
                t(Str::SettingsSourceCode),
                Some(SOURCE_CODE_URL.to_owned()),
                Button::new("source")
                    .small()
                    .ghost()
                    .label(t(Str::SettingsSourceCode))
                    .on_click(|_, _, cx| cx.open_url(SOURCE_CODE_URL)),
                cx,
            ));

        div()
            .id("settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(div().p_4().flex().justify_center().child(content))
    }
}
