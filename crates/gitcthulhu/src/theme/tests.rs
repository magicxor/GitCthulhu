use core::prelude::v1::test;

use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{TestAppContext, px};

use super::*;
use crate::settings;
use crate::test_support::geometry::init_visual;

#[test]
fn argb_conversion_keeps_alpha_and_channels() {
    let c = argb_to_hsla(0xAA566F5A).to_rgb();
    assert!((c.a - 170. / 255.).abs() < 1e-3, "alpha is the top byte");
    assert!((c.r - 0x56 as f32 / 255.).abs() < 1e-3, "red");
    assert!((c.g - 0x6F as f32 / 255.).abs() < 1e-3, "green");
    assert!((c.b - 0x5A as f32 / 255.).abs() < 1e-3, "blue");
}

#[test]
fn builtin_tables_match_the_gitnuro_values() {
    let dark = builtin::dark();
    assert!(!dark.is_light, "dark is not light");
    assert_eq!(dark.primary, argb_to_hsla(0xFF0169CB), "dark primary");
    assert_eq!(dark.background, argb_to_hsla(0xFF0F0F0F), "dark background");
    assert_eq!(dark.surface, argb_to_hsla(0xFF1F1F22), "dark surface");
    let light = builtin::light();
    assert!(light.is_light, "light is light");
    assert_eq!(
        light.on_primary,
        argb_to_hsla(0xFFFFFFFF),
        "light onPrimary is masked to white"
    );
    assert_eq!(
        light.background_selected,
        argb_to_hsla(0xC0CEE1F2),
        "light backgroundSelected keeps its alpha"
    );
    assert_eq!(builtin::DARK_ARGB.len(), 28, "28 colour tokens");
    assert_eq!(builtin::LIGHT_ARGB.len(), 28, "28 colour tokens");
}

#[test]
fn derived_colours_follow_the_formulas() {
    let dark = builtin::dark();
    assert_eq!(
        dark.divider(),
        dark.primary_variant.alpha(0.2),
        "divider is primaryVariant @ 20 %"
    );
    assert_eq!(
        dark.field_border(),
        dark.on_background_secondary.alpha(0.4),
        "field border @ 40 %"
    );
    assert_eq!(
        dark.field_border_disabled().a,
        0.2,
        "disabled border @ 20 %"
    );
    assert_eq!(dark.disabled_content().a, 0.5, "disabled content @ 50 %");
    assert_eq!(dark.menu_icon().a, 0.8, "menu icon @ 80 %");
    assert_eq!(dark.hover_layer().a, 0.04, "hover layer @ 4 %");
    assert_eq!(dark.press_layer().a, 0.10, "dark press layer @ 10 %");
    assert_eq!(
        builtin::light().press_layer().a,
        0.12,
        "light press layer @ 12 %"
    );
    assert!(dark.hover_layer().l > 0.9, "dark hover layer is white");
    assert!(
        builtin::light().hover_layer().l < 0.1,
        "light hover layer is black"
    );
}

#[test]
fn custom_theme_parses_partial_json_over_dark() {
    let tokens =
        parse_custom(r##"{"primary":"FF112233","isLight":true,"background":"#FFFFFFFF"}"##)
            .expect("valid custom theme");
    assert_eq!(
        tokens.primary,
        argb_to_hsla(0xFF112233),
        "given key is used"
    );
    assert!(tokens.is_light, "isLight is read");
    assert_eq!(
        tokens.surface,
        builtin::dark().surface,
        "missing keys fall back to dark"
    );
    assert!(
        parse_custom("not json").is_err(),
        "invalid JSON is an error"
    );
    assert!(parse_custom("[1]").is_err(), "a non-object is an error");
}

#[gpui_kit::test]
fn theme_switch_updates_tokens(cx: &mut TestAppContext) {
    init_visual(cx);
    cx.update(|cx| {
        assert_eq!(
            cx.tokens().background,
            builtin::dark().background,
            "starts dark"
        );
        assert!(Theme::global(cx).is_dark(), "kit theme starts dark");

        apply(cx, ThemeKind::Light, "").expect("light applies");
        assert_eq!(
            cx.tokens().background,
            builtin::light().background,
            "tokens are light"
        );
        assert_eq!(cx.theme_g().kind, ThemeKind::Light, "kind is light");
        assert_eq!(Theme::global(cx).mode, ThemeMode::Light, "kit mode follows");
        assert_eq!(
            Theme::global(cx).colors.background,
            builtin::light().background,
            "kit background is the light token"
        );
        assert_eq!(
            Theme::global(cx).colors.primary,
            builtin::light().primary,
            "kit primary is mapped"
        );

        let err = apply(cx, ThemeKind::Custom, "garbage").expect_err("bad custom theme");
        assert!(!err.0.is_empty(), "the error explains the problem");
        assert_eq!(
            cx.theme_g().kind,
            ThemeKind::Dark,
            "an unusable custom theme falls back to dark"
        );

        apply(
            cx,
            ThemeKind::Custom,
            r#"{"primary":"FF010203","isLight":false}"#,
        )
        .expect("custom applies");
        assert_eq!(cx.theme_g().kind, ThemeKind::Custom, "custom is active");
        assert_eq!(
            cx.tokens().primary,
            argb_to_hsla(0xFF010203),
            "custom token is used"
        );
    });
}

#[gpui_kit::test]
fn settings_changes_retheme(cx: &mut TestAppContext) {
    init_visual(cx);
    cx.update(|cx| settings::update(cx, |s| s.theme = ThemeKind::Light));
    cx.update(|cx| {
        assert_eq!(
            cx.theme_g().kind,
            ThemeKind::Light,
            "the observer applied the new theme"
        );
        settings::update(cx, |s| s.scale = 2.0);
    });
    cx.update(|cx| {
        assert_eq!(
            Theme::global(cx).font_size,
            px(28.),
            "scale 2.0 doubles the rem base"
        )
    });
}

#[gpui_kit::test]
fn kit_map_sets_scrollbar_always(cx: &mut TestAppContext) {
    init_visual(cx);
    cx.update(|cx| {
        let theme = Theme::global(cx);
        assert_eq!(
            theme.scrollbar_mode,
            ScrollbarMode::Always,
            "Gitnuro scrollbars are always visible"
        );
        assert_eq!(theme.font_family.as_ref(), "Inter", "UI font");
        assert_eq!(
            theme.mono_font_family.as_ref(),
            "GeistMono NFM",
            "mono font"
        );
        assert_eq!(theme.font_size, px(14.), "rem base at scale 1");
        assert_eq!(theme.mono_font_size, px(13.), "mono size");
        assert_eq!(theme.radius, px(4.), "radius");
        assert_eq!(theme.radius_lg, px(8.), "large radius");
        let tokens = builtin::dark();
        assert_eq!(
            theme.colors.scrollbar_thumb, tokens.normal_scrollbar,
            "thumb colour"
        );
        assert_eq!(
            theme.colors.scrollbar_thumb_hover, tokens.hover_scrollbar,
            "hover thumb colour"
        );
        assert_eq!(theme.colors.overlay, tokens.dialog_overlay, "dialog scrim");
        assert_eq!(
            theme.colors.list_active, tokens.background_selected,
            "selected rows"
        );
        assert_eq!(theme.colors.accent, tokens.primary_variant, "accent");
    });
}

#[test]
fn lanes_repeat_after_six() {
    assert_eq!(builtin::lanes().len(), 6, "six lane colours");
    assert_eq!(builtin::lanes()[0], argb_to_hsla(0xFF42A5F5), "lane 0");
}
