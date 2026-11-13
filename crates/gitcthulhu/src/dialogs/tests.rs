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

/// Reports through `alive` whether the layer that owns it still exists (the kit drops a layer's builder when the
/// layer closes).
struct LayerProbe(Rc<std::cell::Cell<bool>>);

impl Drop for LayerProbe {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

fn open_probe_layer(
    window: &mut Window,
    cx: &mut App,
    label: &'static str,
) -> (LayerId, Rc<std::cell::Cell<bool>>) {
    let alive = Rc::new(std::cell::Cell::new(true));
    let probe = LayerProbe(alive.clone());
    let id = DialogFrame::open(
        window,
        cx,
        DialogSpec {
            width: 300.,
            est_height: 100.,
        },
        move |_, _| {
            let _probe = &probe;
            div().child(label).into_any_element()
        },
    );
    window.render_frame(cx);
    window.render_frame(cx);
    (id, alive)
}

#[gpui_kit::test]
fn closing_a_layer_by_id_leaves_the_dialog_above_it_open(cx: &mut TestAppContext) {
    let handle = mount(cx);
    cx.update_window(handle, |_, window, cx| {
        let (below, below_alive) = open_probe_layer(window, cx, "below");
        let (above, above_alive) = open_probe_layer(window, cx, "above");
        assert_ne!(below, above, "layers have distinct ids");

        close_layer(window, cx, below);
        window.render_frame(cx);
        assert!(!below_alive.get(), "the named layer is closed");
        assert!(above_alive.get(), "the layer above it stays open");
        assert!(window.has_active_dialog(cx));

        // A layer that is already gone is ignored: it must not close the one that is left.
        close_layer(window, cx, below);
        window.render_frame(cx);
        assert!(above_alive.get(), "closing a closed layer is a no-op");

        close_layer(window, cx, above);
        window.render_frame(cx);
        assert!(!above_alive.get());
        assert!(!window.has_active_dialog(cx), "no layer is left");
    })
    .expect("window");
}

/// Whether the registry still counts the layer `id` of `window` as open.
fn registered(window: &Window, cx: &mut App, id: LayerId) -> bool {
    let window_id = window.window_handle().window_id();
    cx.default_global::<Layers>()
        .stack(window_id)
        .iter()
        .any(|layer| layer.id == id)
}

#[gpui_kit::test]
fn the_registry_notices_layers_the_kit_closed_itself(cx: &mut TestAppContext) {
    let handle = mount(cx);
    cx.update_window(handle, |_, window, cx| {
        let (bottom, bottom_alive) = open_probe_layer(window, cx, "bottom");
        let (middle, _) = open_probe_layer(window, cx, "middle");
        let (top, _) = open_probe_layer(window, cx, "top");

        // Escape closes the top layer behind the registry's back.
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!registered(window, cx, top), "Escape closed the top layer");
        assert!(registered(window, cx, middle) && registered(window, cx, bottom));

        // Closing the middle layer by id now pops it (it is the top one) and does not touch the bottom layer.
        close_layer(window, cx, middle);
        window.render_frame(cx);
        assert!(!registered(window, cx, middle));
        assert!(
            bottom_alive.get() && registered(window, cx, bottom),
            "the bottom layer is untouched"
        );
        close_layer(window, cx, bottom);
        window.render_frame(cx);
        assert!(!window.has_active_dialog(cx), "no layer is left");
    })
    .expect("window");
}

#[gpui_kit::test]
fn closing_a_lower_layer_keeps_the_focus_in_the_dialog_above(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let state = cx
        .update_window(handle, |_, window, cx| {
            window.activate_window();
            let (below, below_alive) = open_probe_layer(window, cx, "below");
            let field = cx.new(|cx| OutlinedField::new("above-field", window, cx));
            let content = field.clone();
            let above = DialogFrame::open(
                window,
                cx,
                DialogSpec {
                    width: 300.,
                    est_height: 100.,
                },
                move |_, _| div().child(content.clone()).into_any_element(),
            );
            window.render_frame(cx);
            field.update(cx, |f, cx| f.focus(window, cx));
            window.render_frame(cx);
            window.input("ab", cx);
            close_layer(window, cx, below);
            (field, above, below_alive)
        })
        .expect("window");
    let (field, above, below_alive) = state;
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(!below_alive.get(), "the lower layer is closed");
        assert!(registered(window, cx, above), "the dialog above stays open");
        assert!(
            field.read(cx).border_state(window, cx) == crate::widgets::field::BorderState::Focused,
            "and its field keeps the keyboard"
        );
        window.input("c", cx);
        assert_eq!(
            field.read(cx).text(cx).as_ref(),
            "abc",
            "typing goes on where it was"
        );
    })
    .expect("window");
}
