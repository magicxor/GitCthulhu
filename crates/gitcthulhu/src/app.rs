//! Application startup.
//!
//! G0a provides the visual-foundation host: logging, settings and state, fonts, theme and one native window with
//! the themed root. G0b replaces this file with the full startup of spec 21 §2.2 (backend runtime, services,
//! actions, `AppView`); the order of the calls below is already the final one.

use gpui_kit::{
    AppContext as _, Context, IntoElement, Render, Styled as _, Window, WindowOptions, div,
};

use crate::assets::AppAssets;
use crate::settings::{self, Loaded, Settings, UiState};
use crate::theme::ActiveTokens as _;
use crate::{fonts, logging, paths, theme};

struct ThemedRoot;

impl Render for ThemedRoot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.tokens().surface)
    }
}

/// Starts the application and blocks until the last window is closed.
pub fn run() {
    logging::init();
    if let Err(err) = paths::ensure_dirs() {
        tracing::warn!("cannot create the application directories: {err}");
    }
    let settings = match Settings::load_from(&paths::settings_file()) {
        Loaded::Corrupt { value, backup } => {
            tracing::warn!("settings.json was corrupt; backup: {backup:?}");
            value
        }
        loaded => loaded.into_value(),
    };
    let ui_state = UiState::load_from(&paths::state_file()).into_value();

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            fonts::register(cx);
            settings::init(cx, settings.clone());
            settings::state::init(cx, ui_state);
            theme::init(cx, &settings);
            theme::observe(cx);
            let options = WindowOptions {
                titlebar: Some(gpui_kit::TitlebarOptions {
                    title: Some("GitCthulhu".into()),
                    ..Default::default()
                }),
                app_id: Some("gitcthulhu".into()),
                ..Default::default()
            };
            if let Err(err) = gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| ThemedRoot)) {
                tracing::error!("cannot open the main window: {err}");
                cx.quit();
                return;
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    settings::flush(cx);
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}
