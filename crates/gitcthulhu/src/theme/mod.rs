//! Theme tokens (Gitnuro's 28 colour tokens), derived colours, the active-theme global and runtime switching
//! (spec 21 §3.1, §3.3).
//!
//! The tokens are the single source of colour for every GitCthulhu widget. [`apply`] also projects them onto the
//! gpui-kit `Theme` ([`kit_map`]) so that kit components (inputs, scrollbars, dialogs) match.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, Rgba, black, white};
use serde::{Deserialize, Serialize};

use crate::settings::{self, Settings, SettingsGlobal};

pub mod builtin;
pub mod custom;
pub mod kit_map;
pub mod typography;

pub use typography::{TextKind, TextSpec, TextStyleExt};

/// Converts Gitnuro's `0xAARRGGBB` to an [`Hsla`].
pub fn argb_to_hsla(argb: u32) -> Hsla {
    let channel = |shift: u32| ((argb >> shift) & 0xFF) as f32 / 255.0;
    Hsla::from(Rgba {
        r: channel(16),
        g: channel(8),
        b: channel(0),
        a: channel(24),
    })
}

/// All colour tokens of a theme. Names are Gitnuro's tokens in snake_case (07 §13.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub primary: Hsla,
    pub primary_variant: Hsla,
    pub on_primary: Hsla,
    pub secondary: Hsla,
    pub on_secondary: Hsla,
    pub on_background: Hsla,
    pub on_background_secondary: Hsla,
    pub error: Hsla,
    pub on_error: Hsla,
    pub background: Hsla,
    pub background_selected: Hsla,
    pub surface: Hsla,
    pub secondary_surface: Hsla,
    pub tertiary_surface: Hsla,
    pub add_file: Hsla,
    pub deleted_file: Hsla,
    pub modified_file: Hsla,
    pub conflicting_file: Hsla,
    pub dialog_overlay: Hsla,
    pub normal_scrollbar: Hsla,
    pub hover_scrollbar: Hsla,
    pub diff_line_added: Hsla,
    pub diff_content_added: Hsla,
    pub diff_line_removed: Hsla,
    pub diff_content_removed: Hsla,
    pub diff_keyword: Hsla,
    pub diff_annotation: Hsla,
    pub diff_comment: Hsla,
    pub is_light: bool,
}

impl Tokens {
    /// Builds tokens from `(camelCase key, 0xAARRGGBB)` pairs (the built-in tables and the custom theme JSON).
    /// A key missing from `table` takes the Dark theme's value, so partial custom themes stay usable.
    pub fn from_argb(table: &[(&str, u32)], is_light: bool) -> Self {
        let color = |key: &str| {
            let lookup = |t: &[(&str, u32)]| t.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
            let argb = lookup(table)
                .or_else(|| lookup(builtin::DARK_ARGB))
                .unwrap_or(0xFFFF00FF);
            argb_to_hsla(argb)
        };
        Self {
            primary: color("primary"),
            primary_variant: color("primaryVariant"),
            on_primary: color("onPrimary"),
            secondary: color("secondary"),
            on_secondary: color("onSecondary"),
            on_background: color("onBackground"),
            on_background_secondary: color("onBackgroundSecondary"),
            error: color("error"),
            on_error: color("onError"),
            background: color("background"),
            background_selected: color("backgroundSelected"),
            surface: color("surface"),
            secondary_surface: color("secondarySurface"),
            tertiary_surface: color("tertiarySurface"),
            add_file: color("addFile"),
            deleted_file: color("deletedFile"),
            modified_file: color("modifiedFile"),
            conflicting_file: color("conflictingFile"),
            dialog_overlay: color("dialogOverlay"),
            normal_scrollbar: color("normalScrollbar"),
            hover_scrollbar: color("hoverScrollbar"),
            diff_line_added: color("diffLineAdded"),
            diff_content_added: color("diffContentAdded"),
            diff_line_removed: color("diffLineRemoved"),
            diff_content_removed: color("diffContentRemoved"),
            diff_keyword: color("diffKeyword"),
            diff_annotation: color("diffAnnotation"),
            diff_comment: color("diffComment"),
            is_light,
        }
    }

    /// Frame dividers: `primary_variant` at 20 % (07 §13.2).
    pub fn divider(&self) -> Hsla {
        self.primary_variant.alpha(0.2)
    }

    /// Unfocused field border: `on_background_secondary` at 40 %.
    pub fn field_border(&self) -> Hsla {
        self.on_background_secondary.alpha(0.4)
    }

    /// Disabled field border and text: `on_background_secondary` at 20 %.
    pub fn field_border_disabled(&self) -> Hsla {
        self.on_background_secondary.alpha(0.2)
    }

    /// Tooltip and menu border (drawn in dark themes only), toast border, menu separator: `on_background` at 20 %.
    pub fn popup_border(&self) -> Hsla {
        self.on_background.alpha(0.2)
    }

    /// Dialog border: `on_background` at 10 %.
    pub fn dialog_border(&self) -> Hsla {
        self.on_background.alpha(0.1)
    }

    /// Disabled toolbar icon and text: `on_background` at 50 %.
    pub fn disabled_content(&self) -> Hsla {
        self.on_background.alpha(0.5)
    }

    /// Context-menu icons: `on_background_secondary` at 80 %.
    pub fn menu_icon(&self) -> Hsla {
        self.on_background_secondary.alpha(0.8)
    }

    /// Ripple hover layer: white at 4 % (dark) or black at 4 % (light).
    pub fn hover_layer(&self) -> Hsla {
        self.layer_base().alpha(0.04)
    }

    /// Ripple press layer: white at 10 % (dark) or black at 12 % (light).
    pub fn press_layer(&self) -> Hsla {
        self.layer_base()
            .alpha(if self.is_light { 0.12 } else { 0.10 })
    }

    /// Menu drop shadow: `#20000000`.
    pub fn menu_shadow(&self) -> Hsla {
        argb_to_hsla(0x20000000)
    }

    fn layer_base(&self) -> Hsla {
        if self.is_light { black() } else { white() }
    }
}

/// Which theme is active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeKind {
    /// The built-in dark theme.
    #[default]
    Dark,
    /// The built-in light theme.
    Light,
    /// The user's custom theme JSON from the settings.
    Custom,
}

/// A custom theme could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomThemeError(pub String);

impl std::fmt::Display for CustomThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid custom theme: {}", self.0)
    }
}

impl std::error::Error for CustomThemeError {}

/// The active theme: tokens plus the app-owned palette (the kit theme schema is closed, so lane and chip colours
/// live here).
#[derive(Clone, Debug)]
pub struct ThemeGlobal {
    /// The theme in effect (`Dark` when a requested custom theme was unusable).
    pub kind: ThemeKind,
    /// The theme the settings asked for.
    pub requested: ThemeKind,
    pub tokens: Tokens,
    /// Commit-graph lane colours (08 §0.1).
    pub lanes: [Hsla; 6],
    /// Remote branch chip colour (08 §2.12).
    pub remote_chip: Hsla,
    /// The UI scale this theme was projected with.
    pub scale: f32,
    /// The custom theme JSON this theme was built from (empty unless `kind` is `Custom`).
    pub custom_json: String,
}

impl Global for ThemeGlobal {}

/// Access to the active tokens from `App` (and anything that derefs to it, such as `Context<T>`).
pub trait ActiveTokens {
    /// The active tokens.
    fn tokens(&self) -> &Tokens;
    /// The colour of commit-graph `lane` (the palette repeats).
    fn lane(&self, lane: u16) -> Hsla;
    /// The theme global.
    fn theme_g(&self) -> &ThemeGlobal;
}

impl ActiveTokens for App {
    fn tokens(&self) -> &Tokens {
        &self.global::<ThemeGlobal>().tokens
    }

    fn lane(&self, lane: u16) -> Hsla {
        let lanes = &self.global::<ThemeGlobal>().lanes;
        lanes[lane as usize % lanes.len()]
    }

    fn theme_g(&self) -> &ThemeGlobal {
        self.global::<ThemeGlobal>()
    }
}

/// Tokens of a custom theme JSON: Gitnuro's format, 28 camelCase colour keys as ARGB hex strings plus `isLight`.
/// Missing colour keys take the Dark value; malformed values are errors. (Export and the settings UI belong to
/// `custom.rs`.)
pub fn parse_custom(json: &str) -> Result<Tokens, CustomThemeError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| CustomThemeError(e.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| CustomThemeError("expected a JSON object".into()))?;
    let mut table: Vec<(String, u32)> = Vec::new();
    for (key, value) in object {
        // Unknown keys are ignored; a known colour key with an unusable value is an error (the settings UI
        // reports it instead of silently showing the Dark colour).
        if !custom::is_colour_key(key) {
            continue;
        }
        let argb = value.as_str().and_then(custom::parse_argb).ok_or_else(|| {
            CustomThemeError(format!(
                "`{key}` must be an ARGB hex string such as \"FF0070D8\""
            ))
        })?;
        table.push((key.clone(), argb));
    }
    let table: Vec<(&str, u32)> = table.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let is_light = match object.get("isLight") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| CustomThemeError("`isLight` must be true or false".into()))?,
    };
    Ok(Tokens::from_argb(&table, is_light))
}

fn tokens_for(kind: ThemeKind, custom_json: &str) -> (ThemeKind, Tokens, Option<CustomThemeError>) {
    match kind {
        ThemeKind::Dark => (ThemeKind::Dark, builtin::dark(), None),
        ThemeKind::Light => (ThemeKind::Light, builtin::light(), None),
        ThemeKind::Custom => match parse_custom(custom_json) {
            Ok(tokens) => (ThemeKind::Custom, tokens, None),
            Err(err) => (ThemeKind::Dark, builtin::dark(), Some(err)),
        },
    }
}

fn install(
    cx: &mut App,
    requested: ThemeKind,
    custom_json: &str,
    scale: f32,
) -> Result<(), CustomThemeError> {
    let (kind, tokens, error) = tokens_for(requested, custom_json);
    let mode = if tokens.is_light {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    // The mode change loads the kit's registered theme and overwrites colours, so the mapping is a second update
    // (gpui-component theme/mod.rs `Theme::update`).
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| kit_map::apply(theme, &tokens, scale));
    cx.set_global(ThemeGlobal {
        kind,
        requested,
        tokens,
        lanes: builtin::lanes(),
        remote_chip: builtin::remote_chip(),
        scale,
        custom_json: if requested == ThemeKind::Custom {
            custom_json.to_owned()
        } else {
            String::new()
        },
    });
    match error {
        None => Ok(()),
        Some(err) => Err(err),
    }
}

/// Installs the theme from `settings` (call after `gpui_kit::init` and the font registration). An invalid custom
/// theme falls back to Dark; the error is logged.
pub fn init(cx: &mut App, settings: &Settings) {
    if let Err(err) = install(cx, settings.theme, &settings.custom_theme, settings.scale) {
        tracing::warn!("{err}; using the dark theme");
    }
}

/// Switches the theme at runtime: sets [`ThemeGlobal`], maps it onto the kit theme and refreshes every window.
///
/// With `ThemeKind::Custom` and unusable JSON the Dark theme is applied and the error is returned so the caller can
/// show "Failed trying to save the custom theme".
pub fn apply(cx: &mut App, kind: ThemeKind, custom_json: &str) -> Result<(), CustomThemeError> {
    let scale = cx
        .try_global::<SettingsGlobal>()
        .map(|s| s.0.scale)
        .unwrap_or(1.0);
    install(cx, kind, custom_json, scale)
}

/// Re-applies the theme whenever the theme, custom theme or scale settings change. Call once at startup after
/// [`init`].
pub fn observe(cx: &mut App) {
    cx.observe_global::<SettingsGlobal>(|cx| {
        let settings = settings::get(cx).clone();
        let (kind, custom, scale) = {
            let current = cx.global::<ThemeGlobal>();
            (
                current.requested,
                current.custom_json.clone(),
                current.scale,
            )
        };
        let wanted_custom = if settings.theme == ThemeKind::Custom {
            settings.custom_theme.as_str()
        } else {
            ""
        };
        let changed = settings.theme != kind || wanted_custom != custom || settings.scale != scale;
        if changed
            && let Err(err) = install(cx, settings.theme, &settings.custom_theme, settings.scale)
        {
            tracing::warn!("{err}; using the dark theme");
        }
    })
    .detach();
}

#[cfg(test)]
mod tests;
