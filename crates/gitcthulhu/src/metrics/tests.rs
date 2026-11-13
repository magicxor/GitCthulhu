use core::prelude::v1::test;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px, rems};

use super::*;
use crate::settings;
use crate::test_support::geometry::{
    assert_height, assert_rect, assert_width, init_visual, open_probe,
};
use crate::widgets::observed;

use gpui_kit::{IntoElement as _, ParentElement as _, Styled as _};

#[test]
fn dp_is_rems_of_the_base_size() {
    assert_eq!(dp(14.), rems(1.), "14 dp is exactly one rem");
    assert_eq!(dp(38.), rems(38. / BASE_REM), "dp divides by the base rem");
}

#[test]
fn line_height_presets_match_gitnuro() {
    assert_eq!(
        (SPACED.file_row, SPACED.log_row, SPACED.side_item),
        (38., 38., 36.),
        "07 §13.4 spaced preset"
    );
    assert_eq!(
        (COMPACT.file_row, COMPACT.log_row, COMPACT.side_item),
        (34., 34., 34.),
        "07 §13.4 compact preset"
    );
}

#[test]
fn shared_constants_match_the_spec_table() {
    assert_eq!(
        (TOOLBAR_H, TAB_STRIP_H, BOTTOM_BAR_H, HEADER_BAR_H),
        (64., 36., 32., 34.),
        "shell heights (07 §2, §5.1, §7; 08 §0.5)"
    );
    assert_eq!(
        (SIDE_PANE_W, RIGHT_PANE_W, SPLIT_GUTTER, PANE_MIN_W),
        (220., 330., 8., 150.),
        "pane widths (07 §5.3)"
    );
    assert_eq!((TOAST_H, TOAST_MS), (36., 2000), "toasts (07 §8)");
    assert_eq!(
        (SWITCH_W, SWITCH_H, CHECKBOX_SLOT),
        (48., 28., 28.),
        "switch and checkbox (07 §10.4, 09 §0.3)"
    );
}

fn shell(window_size: (f32, f32), cx: &mut TestAppContext) -> gpui_kit::AnyWindowHandle {
    let (handle, _) = open_probe(cx, window_size.0, window_size.1, |_, _| {
        observed("shell")
            .size_full()
            .flex()
            .flex_col()
            .child(observed("toolbar").w_full().h(dp(TOOLBAR_H)).flex_none())
            .child(
                observed("row")
                    .flex_1()
                    .flex()
                    .child(observed("side").w(dp(SIDE_PANE_W)).h_full().flex_none())
                    .child(observed("center").flex_1().h_full()),
            )
            .child(observed("bottom").w_full().h(dp(BOTTOM_BAR_H)).flex_none())
            .into_any_element()
    });
    handle
}

#[gpui_kit::test]
fn dp_equals_px_at_scale_1(cx: &mut TestAppContext) {
    init_visual(cx);
    let handle = shell((1280., 720.), cx);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_rect(window, "toolbar", 0., 0., 1280., 64.);
        assert_width(window, "side", 220.);
        assert_height(window, "bottom", 32.);
        assert_rect(window, "bottom", 0., 688., 1280., 32.);
        assert_eq!(
            window.rem_size(),
            px(14.),
            "the rem size is the 14 px base at scale 1"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn scale_125_scales_toolbar(cx: &mut TestAppContext) {
    init_visual(cx);
    let handle = shell((1280., 720.), cx);
    cx.update(|cx| settings::update(cx, |s| s.scale = 1.25));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_height(window, "toolbar", TOOLBAR_H * 1.25);
        assert_width(window, "side", SIDE_PANE_W * 1.25);
    })
    .expect("window");
}

#[gpui_kit::test]
fn line_heights_follow_the_setting(cx: &mut TestAppContext) {
    init_visual(cx);
    cx.update(|cx| {
        assert_eq!(line_heights(cx), SPACED, "default is spaced");
        settings::update(cx, |s| s.lines_height = LinesHeight::Compact);
        assert_eq!(
            line_heights(cx),
            COMPACT,
            "compact after the setting changed"
        );
    });
}
