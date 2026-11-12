use core::prelude::v1::test;

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, TestAppContext, div};

use super::*;
use crate::test_support::geometry::{assert_height, assert_width, bounds, init_visual, open_probe};

fn mount(cx: &mut TestAppContext) -> AnyWindowHandle {
    init_visual(cx);
    let (handle, _) = open_probe(cx, 1280., 720., |_, _| div().size_full().into_any_element());
    handle
}

#[test]
fn centered_margin_never_goes_above_16() {
    assert_eq!(centered_margin_top(720., 200.), 260., "centred");
    assert_eq!(
        centered_margin_top(300., 600.),
        16.,
        "taller than the viewport: 16 px from the top"
    );
}

#[gpui_kit::test]
fn kit_dialog_frame_geometry(cx: &mut TestAppContext) {
    let handle = mount(cx);
    cx.update_window(handle, |_, window, cx| {
        DialogFrame::open(
            window,
            cx,
            DialogSpec {
                width: 580.,
                est_height: 200.,
            },
            |_, _| div().w_full().h(dp(168.)).into_any_element(),
        );
        window.render_frame(cx);
        window.render_frame(cx);
        let frame = bounds(window, ids::DIALOG_FRAME);
        assert_eq!(
            frame.origin.x.as_f32(),
            (1280. - 580.) / 2.,
            "the dialog is horizontally centred"
        );
        assert_width(window, ids::DIALOG_FRAME, 580.);
        // 168 dp content + 2 x 16 dp (padding with the 1 dp border drawn over it, as in Compose): the kit adds no
        // border or padding of its own.
        assert_height(window, ids::DIALOG_FRAME, 168. + 32.);
        assert!(
            (frame.origin.y.as_f32() - (720. - 200.) / 2.).abs() <= 0.5,
            "vertically centred from the estimated height, got y {}",
            frame.origin.y.as_f32()
        );
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx), "Escape closes the dialog");
    })
    .expect("window");
}

fn open_single(
    window: &mut Window,
    cx: &mut App,
    accepted: Rc<RefCell<Vec<String>>>,
    initial: &str,
) {
    let mut config = SingleFieldConfig::new(
        AppIcon::Branch,
        "New branch",
        "Create",
        move |text, _, _| accepted.borrow_mut().push(text),
    );
    config.initial_text = initial.to_owned();
    config.subtitle = Some("Enter the name of the branch".into());
    SingleTextFieldDialog::open(window, cx, config);
    window.render_frame(cx);
    window.render_frame(cx);
}

#[gpui_kit::test]
fn single_field_dialog_is_332_wide_with_a_64_icon(cx: &mut TestAppContext) {
    let handle = mount(cx);
    cx.update_window(handle, |_, window, cx| {
        open_single(window, cx, Rc::default(), "");
        assert_width(window, ids::DIALOG_FRAME, 332.);
        assert_width(window, "single-field-input", 300.);
    })
    .expect("window");
}

#[gpui_kit::test]
fn single_field_dialog_enter_accepts_non_blank_text(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let accepted = Rc::new(RefCell::new(Vec::new()));
    cx.update_window(handle, |_, window, cx| {
        open_single(window, cx, accepted.clone(), "");
        window.press("enter", cx);
    })
    .expect("window");
    cx.update_window(handle, |_, window, cx| {
        assert!(
            accepted.borrow().is_empty(),
            "Enter does nothing while the text is blank"
        );
        assert!(window.has_active_dialog(cx), "the dialog stays open");
        window.render_frame(cx);
        window.input("feature/x", cx);
    })
    .expect("window");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("enter", cx);
    })
    .expect("window");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            accepted.borrow().as_slice(),
            ["feature/x"],
            "Enter accepts the typed text exactly once"
        );
        assert!(!window.has_active_dialog(cx), "accepting closes the dialog");
    })
    .expect("window");
}

#[gpui_kit::test]
fn single_field_dialog_primary_click_and_cancel(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let accepted = Rc::new(RefCell::new(Vec::new()));
    cx.update_window(handle, |_, window, cx| {
        open_single(window, cx, accepted.clone(), "main2");
        window.click("dialog-cancel", cx);
        window.render_frame(cx);
        assert!(accepted.borrow().is_empty(), "Cancel does not accept");
        assert!(!window.has_active_dialog(cx), "Cancel closes the dialog");
        open_single(window, cx, accepted.clone(), "main2");
        window.click("dialog-primary", cx);
        window.render_frame(cx);
        assert_eq!(
            accepted.borrow().as_slice(),
            ["main2"],
            "the primary button accepts"
        );
        assert!(!window.has_active_dialog(cx), "the dialog closed");
    })
    .expect("window");
}

#[gpui_kit::test]
fn escape_in_the_focused_field_closes_the_dialog(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let accepted = Rc::new(RefCell::new(Vec::new()));
    cx.update_window(handle, |_, window, cx| {
        open_single(window, cx, accepted.clone(), "draft");
    })
    .expect("window");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .expect("window");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            !window.has_active_dialog(cx),
            "Esc dismisses the dialog even while its text field has focus (09 §3.0)"
        );
        assert!(accepted.borrow().is_empty(), "dismissing does not accept");
    })
    .expect("window");
}

#[gpui_kit::test]
fn buttons_are_right_aligned_with_an_8_dp_gap(cx: &mut TestAppContext) {
    let handle = mount(cx);
    cx.update_window(handle, |_, window, cx| {
        open_single(window, cx, Rc::default(), "x");
        let cancel = bounds(window, "dialog-cancel");
        let primary = bounds(window, "dialog-primary");
        let frame = bounds(window, ids::DIALOG_FRAME);
        let gap = primary.origin.x.as_f32() - (cancel.origin.x + cancel.size.width).as_f32();
        assert!((gap - 8.).abs() <= 0.5, "Cancel and the primary button are 8 dp apart, got {gap}");
        let right_pad = (frame.origin.x + frame.size.width).as_f32()
            - (primary.origin.x + primary.size.width).as_f32();
        assert!(
            (right_pad - 16.).abs() <= 0.5,
            "the primary button sits 16 dp from the right edge (the border overlaps the padding), got {right_pad}"
        );
        let field = bounds(window, "single-field-input");
        assert!(
            ((field.origin.x - frame.origin.x).as_f32() - 16.).abs() <= 0.5,
            "the 300 dp field fills the content width exactly: 16 dp from the left edge, got {field:?} in {frame:?}"
        );
    })
    .expect("window");
}
