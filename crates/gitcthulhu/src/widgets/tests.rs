use core::prelude::v1::test;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::backend::ChangeKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyElement, AppContext as _, ElementId, Entity, ParentElement as _, TestAppContext, div, point,
    px,
};

use super::field::{BorderState, OutlinedField};
use super::file_row::FileRow;
use super::split::{SplitWidths, ThreePaneSplit};
use super::*;
use crate::icons::AppIcon;
use crate::ids;
use crate::menus::MenuItem;
use crate::metrics::dp;
use crate::settings::{self, LinesHeight, UiState};
use crate::test_support::geometry::{
    assert_height, assert_rect, assert_width, bounds, init_visual, open_probe,
};
use crate::test_support::input::click_with;
use crate::theme::ActiveTokens as _;

fn probe(
    cx: &mut TestAppContext,
    size: (f32, f32),
    mut build: impl FnMut() -> AnyElement + 'static,
) -> gpui_kit::AnyWindowHandle {
    init_visual(cx);
    open_probe(cx, size.0, size.1, move |_, _| build()).0
}

#[gpui_kit::test]
fn split_clamps_and_persists(cx: &mut TestAppContext) {
    init_visual(cx);
    cx.update(|cx| settings::state::init(cx, UiState::default()));
    let widths = Rc::new(Cell::new(SplitWidths::default()));
    let state = widths.clone();
    let (handle, _) = open_probe(cx, 1280., 720., move |_, cx| {
        let me = cx.entity().downgrade();
        let current = state.clone();
        let on_change_state = state.clone();
        ThreePaneSplit::new(
            Some(observed("first-content").size_full().into_any_element()),
            observed("center-content").size_full(),
            observed("third-content").size_full(),
            current.get(),
        )
        .on_change(move |next, _, cx| {
            on_change_state.set(next);
            me.update(cx, |_, cx| cx.notify()).ok();
        })
        .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_rect(window, ids::PANE_FIRST, 0., 0., 220., 720.);
        assert_rect(window, ids::GUTTER_FIRST, 220., 0., 8., 720.);
        assert_rect(window, ids::PANE_CENTER, 228., 0., 1280. - 220. - 330. - 16., 720.);
        assert_rect(window, ids::GUTTER_THIRD, 1280. - 338., 0., 8., 720.);
        assert_rect(window, ids::PANE_THIRD, 1280. - 330., 0., 330., 720.);

        let grab = bounds(window, ids::GUTTER_FIRST).center();
        window.drag(grab, point(grab.x + px(900.), grab.y), cx);
        assert_eq!(widths.get().first, 512., "the first pane clamps at 40 % of the 1280 dp split");
        assert_eq!(widths.get().third, 330., "the other pane is untouched");

        let grab = bounds(window, ids::GUTTER_FIRST).center();
        window.drag(grab, point(px(2.), grab.y), cx);
        assert_eq!(widths.get().first, 150., "the first pane clamps at 150 dp");
        window.render_frame(cx);
        assert_width(window, ids::PANE_FIRST, 150.);

        let grab = bounds(window, ids::GUTTER_THIRD).center();
        window.drag(grab, point(grab.x - px(40.), grab.y), cx);
        assert_eq!(
            widths.get().third,
            370.,
            "dragging the right gutter 40 dp left grows the right pane by exactly 40 dp (measured from the press, \
             not from where GPUI recognised the drag)"
        );
        window.render_frame(cx);
        assert_width(window, ids::PANE_THIRD, 370.);
        let grab = bounds(window, ids::GUTTER_THIRD).center();
        window.drag(grab, point(px(1270.), grab.y), cx);
        assert_eq!(widths.get().third, 150., "the right pane clamps at 150 dp");

        let stored = settings::state::get(cx);
        assert_eq!(
            (stored.first_pane_width, stored.third_pane_width),
            (150., 150.),
            "the widths are persisted to UiState when a drag ends"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn split_commits_once_per_drag_and_not_on_clicks(cx: &mut TestAppContext) {
    init_visual(cx);
    cx.update(|cx| settings::state::init(cx, UiState::default()));
    let state = Rc::new(Cell::new(SplitWidths::default()));
    let commits = Rc::new(RefCell::new(Vec::new()));
    let log = commits.clone();
    let (handle, _) = open_probe(cx, 1280., 720., move |_, cx| {
        let me = cx.entity().downgrade();
        let on_change_state = state.clone();
        let log = log.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(observed("elsewhere").w_full().h(dp(40.)).flex_none())
            .child(
                div().flex_1().child(
                    ThreePaneSplit::new(
                        Some(observed("first-content").size_full().into_any_element()),
                        observed("center-content").size_full(),
                        observed("third-content").size_full(),
                        state.get(),
                    )
                    .on_change(move |next, _, cx| {
                        on_change_state.set(next);
                        me.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .on_commit(move |w, _, _| log.borrow_mut().push(w)),
                ),
            )
            .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("elsewhere", cx);
        window.click(ids::GUTTER_FIRST, cx);
        assert!(
            commits.borrow().is_empty(),
            "clicks (on the gutter or anywhere else) are not drag ends"
        );
        let grab = bounds(window, ids::GUTTER_FIRST).center();
        window.drag(grab, point(grab.x + px(30.), grab.y), cx);
        assert_eq!(
            commits.borrow().as_slice(),
            [SplitWidths {
                first: 250.,
                third: 330.
            }],
            "one commit with the final widths per drag"
        );
        window.click("elsewhere", cx);
        assert_eq!(commits.borrow().len(), 1, "later clicks commit nothing");
    })
    .expect("window");
}

#[gpui_kit::test]
fn v_resizer_follows_the_pointer_and_clamps(cx: &mut TestAppContext) {
    init_visual(cx);
    let height = Rc::new(Cell::new(240.0_f32));
    let commits = Rc::new(RefCell::new(Vec::new()));
    let (state, log) = (height.clone(), commits.clone());
    let (handle, _) = open_probe(cx, 800., 720., move |_, cx| {
        let me = cx.entity().downgrade();
        let on_change_state = state.clone();
        let log = log.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1())
            .child(
                VResizer::new("terminal-resizer", state.get(), 120., 400.)
                    .on_change(move |h, _, cx| {
                        on_change_state.set(h);
                        me.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .on_commit(move |h, _, _| log.borrow_mut().push(h)),
            )
            .child(observed("panel").w_full().h(dp(state.get())).flex_none())
            .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let grab = bounds(window, "terminal-resizer").center();
        window.drag(grab, point(grab.x, grab.y - px(50.)), cx);
        assert_eq!(
            height.get(),
            290.,
            "dragging 50 dp up grows the panel by exactly 50 dp"
        );
        window.render_frame(cx);
        assert_height(window, "panel", 290.);
        let grab = bounds(window, "terminal-resizer").center();
        window.drag(grab, point(grab.x, px(1.)), cx);
        assert_eq!(height.get(), 400., "the panel clamps at its maximum");
        assert_eq!(
            commits.borrow().as_slice(),
            [290., 400.],
            "each drag commits its final height once"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn split_without_first_pane_has_no_first_gutter(cx: &mut TestAppContext) {
    let handle = probe(cx, (1280., 720.), || {
        ThreePaneSplit::new(
            None,
            observed("c").size_full(),
            observed("t").size_full(),
            SplitWidths::default(),
        )
        .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find(ids::PANE_FIRST).is_none(),
            "the first pane is absent"
        );
        assert!(
            window.try_find(ids::GUTTER_FIRST).is_none(),
            "and so is its gutter"
        );
        assert_rect(window, ids::PANE_CENTER, 0., 0., 1280. - 330. - 8., 720.);
    })
    .expect("window");
}

#[gpui_kit::test]
fn checkbox_hit_area_is_content_width(cx: &mut TestAppContext) {
    let toggled = Rc::new(RefCell::new(Vec::new()));
    let log = toggled.clone();
    let handle = probe(cx, (800., 300.), move || {
        let log = log.clone();
        observed("parent")
            .w(dp(400.))
            .flex()
            .flex_col()
            .child(
                CheckboxText::new("amend", false, "Amend previous commit")
                    .on_toggle(move |value, _, _| log.borrow_mut().push(value)),
            )
            .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_height(window, "amend", 28.);
        let row = bounds(window, "amend");
        assert!(
            row.size.width.as_f32() > 28. && row.size.width.as_f32() < 300.,
            "the hit area is as wide as its content (28 dp slot + caption), not the 400 dp parent: {}",
            row.size.width.as_f32()
        );
        window.click("amend", cx);
        assert_eq!(toggled.borrow().as_slice(), [true], "clicking the row requests the new value");
    })
    .expect("window");
}

#[gpui_kit::test]
fn outlined_field_border_states(cx: &mut TestAppContext) {
    init_visual(cx);
    let slot: Rc<RefCell<Option<Entity<OutlinedField>>>> = Rc::default();
    let build_slot = slot.clone();
    let (handle, _) = open_probe(cx, 600., 200., move |window, cx| {
        let field = build_slot
            .borrow_mut()
            .get_or_insert_with(|| cx.new(|cx| OutlinedField::new("field", window, cx)))
            .clone();
        div().w(dp(300.)).child(field).into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let field = slot.borrow().clone().expect("field created");
        let tokens = *cx.tokens();
        assert_height(window, "field", 38.);
        assert_eq!(
            field.read(cx).border_state(window, cx),
            BorderState::Unfocused,
            "unfocused at rest"
        );
        assert_eq!(
            BorderState::Unfocused.color(&tokens),
            tokens.field_border(),
            "unfocused colour"
        );

        field.update(cx, |f, cx| f.focus(window, cx));
        window.render_frame(cx);
        assert_eq!(
            field.read(cx).border_state(window, cx),
            BorderState::Focused,
            "focused after focus()"
        );
        assert_eq!(
            BorderState::Focused.color(&tokens),
            tokens.primary_variant,
            "focused colour"
        );

        field.update(cx, |f, cx| f.set_disabled(true, cx));
        assert_eq!(
            field.read(cx).border_state(window, cx),
            BorderState::Disabled,
            "disabled wins over focus"
        );
        assert_eq!(
            BorderState::Disabled.color(&tokens),
            tokens.field_border_disabled(),
            "disabled colour"
        );

        field.update(cx, |f, cx| f.set_error(true, cx));
        assert_eq!(
            field.read(cx).border_state(window, cx),
            BorderState::Error,
            "error wins over disabled"
        );
        assert_eq!(
            BorderState::Error.color(&tokens),
            tokens.error,
            "error colour"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn outlined_field_text_starts_12_dp_from_the_edge(cx: &mut TestAppContext) {
    init_visual(cx);
    let slot: Rc<RefCell<Option<Entity<OutlinedField>>>> = Rc::default();
    let build_slot = slot.clone();
    let (handle, _) = open_probe(cx, 600., 200., move |window, cx| {
        let field = build_slot
            .borrow_mut()
            .get_or_insert_with(|| cx.new(|cx| OutlinedField::new("field", window, cx)))
            .clone();
        div().p(dp(20.)).w(dp(340.)).child(field).into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        let field = slot.borrow().clone().expect("field created");
        field.update(cx, |f, cx| f.set_text("Title", window, cx));
        window.render_frame(cx);
        window.render_frame(cx);
        let frame = bounds(window, "field");
        let text = field
            .read(cx)
            .text_bounds(cx)
            .expect("the text was laid out");
        let inset = (text.origin.x - frame.origin.x).as_f32();
        assert!(
            (inset - 12.).abs() <= 0.5,
            "07 §10.1 / 09 §0.3: padding h12 with the 2 dp border drawn over it, got {inset}"
        );
        let end = (frame.right() - text.right()).as_f32();
        assert!(
            (end - 12.).abs() <= 0.5,
            "the text ends 12 dp before the right edge too, got {end}"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn outlined_field_reports_text_and_changes(cx: &mut TestAppContext) {
    init_visual(cx);
    let slot: Rc<RefCell<Option<Entity<OutlinedField>>>> = Rc::default();
    let build_slot = slot.clone();
    let (handle, _) = open_probe(cx, 600., 200., move |window, cx| {
        let field = build_slot
            .borrow_mut()
            .get_or_insert_with(|| cx.new(|cx| OutlinedField::new("field", window, cx)))
            .clone();
        div().w(dp(300.)).child(field).into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let field = slot.borrow().clone().expect("field created");
        field.update(cx, |f, cx| f.set_text("hello", window, cx));
        assert_eq!(
            field.read(cx).text(cx).as_ref(),
            "hello",
            "set_text is readable back"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn buttons_have_the_gitnuro_sizes(cx: &mut TestAppContext) {
    let handle = probe(cx, (800., 600.), || {
        div()
            .flex()
            .flex_col()
            .items_start()
            .child(
                observed("wrap-primary")
                    .flex()
                    .child(PrimaryButton::new("primary", "Commit")),
            )
            .child(
                observed("wrap-secondary")
                    .flex()
                    .child(SecondaryButton::new("secondary", "Stage")),
            )
            .child(MenuButton::new("menu-button", AppIcon::Open, "Open"))
            .child(
                ExtendedMenuButton::new("pull", AppIcon::Download, "Pull").dropdown(
                    vec![MenuItem::action("fetch", "Fetch all", |_, _| {})],
                    true,
                ),
            )
            .child(
                observed("switch-wrap")
                    .flex()
                    .child(AppSwitch::new("switch", true)),
            )
            .child(HeaderBar34::new().id("header").child("Staged"))
            .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let primary = bounds(window, "primary").size.height.as_f32();
        assert!(
            (primary - 29.).abs() <= 0.5,
            "PrimaryButton is about 29 dp high, got {primary}"
        );
        let secondary = bounds(window, "secondary").size.height.as_f32();
        assert!(
            (secondary - 24.).abs() <= 0.5,
            "SecondaryButton is about 24 dp high, got {secondary}"
        );
        assert_width(window, "menu-button", 56.);
        assert_height(window, "menu-button", 56.);
        assert_width(window, "pull", 64.);
        assert_height(window, "pull", 56.);
        assert_width(
            window,
            ElementId::from((ElementId::from("pull"), "chevron")),
            16.,
        );
        assert_width(window, "switch", 48.);
        assert_height(window, "switch", 28.);
        assert_height(window, "header", 34.);
    })
    .expect("window");
}

#[gpui_kit::test]
fn extended_button_chevron_is_a_separate_click_target(cx: &mut TestAppContext) {
    let main_clicks = Rc::new(Cell::new(0));
    let fetches = Rc::new(Cell::new(0));
    let (m, f) = (main_clicks.clone(), fetches.clone());
    init_visual(cx);
    let host = cx.update(crate::menus::MenuHost::install);
    let layer = host.clone();
    let (handle, _) = open_probe(cx, 800., 600., move |_, _| {
        let (m, f) = (m.clone(), f.clone());
        div()
            .relative()
            .size_full()
            .child(
                ExtendedMenuButton::new("pull", AppIcon::Download, "Pull")
                    .on_click(move |_, _| m.set(m.get() + 1))
                    .dropdown(
                        vec![MenuItem::action("fetch", "Fetch all", move |_, _| {
                            f.set(f.get() + 1)
                        })],
                        false,
                    ),
            )
            .child(layer.clone())
            .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(ElementId::from((ElementId::from("pull"), "main")), cx);
        assert_eq!(
            main_clicks.get(),
            1,
            "the 48 dp main part runs the main action"
        );
        assert!(!host.read(cx).is_open(), "and does not open the dropdown");

        window.click(ElementId::from((ElementId::from("pull"), "chevron")), cx);
        window.render_frame(cx);
        assert_eq!(
            main_clicks.get(),
            1,
            "the chevron does not run the main action"
        );
        assert!(host.read(cx).is_open(), "the chevron opens the dropdown");
        let button = bounds(window, "pull");
        let menu = bounds(window, ids::MENU);
        assert!(
            (menu.origin.x - (button.origin.x + button.size.width - px(16.)))
                .as_f32()
                .abs()
                <= 1.0,
            "the popup is anchored at the chevron strip's left edge, got {menu:?} for {button:?}"
        );
        assert!(
            (menu.origin.y - (button.origin.y + button.size.height))
                .as_f32()
                .abs()
                <= 1.0,
            "the popup starts at the button's bottom edge"
        );
        window.click("menu-item-fetch", cx);
        assert_eq!(fetches.get(), 1, "the dropdown item ran");
    })
    .expect("window");
}

#[gpui_kit::test]
fn disabled_menu_button_ignores_clicks(cx: &mut TestAppContext) {
    let clicks = Rc::new(Cell::new(0));
    let counter = clicks.clone();
    let handle = probe(cx, (400., 300.), move || {
        let counter = counter.clone();
        MenuButton::new("open", AppIcon::Open, "Open")
            .enabled(false)
            .on_click(move |_, _| counter.set(counter.get() + 1))
            .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("open", cx);
        assert_eq!(
            clicks.get(),
            0,
            "a disabled toolbar button has no click handler"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn clicks_with_modifiers_reach_file_rows(cx: &mut TestAppContext) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    let handle = probe(cx, (600., 300.), move || {
        let log = log.clone();
        div()
            .child(
                FileRow::new("row-a", "src/lib.rs", ChangeKind::Modified)
                    .on_click(move |ev, _, _| log.borrow_mut().push(ev.modifiers().secondary())),
            )
            .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_height(window, "row-a", 38.);
        click_with(
            window,
            cx,
            "row-a",
            gpui_kit::Modifiers::secondary_key(),
            gpui_kit::MouseButton::Left,
        );
        assert_eq!(
            seen.borrow().as_slice(),
            [true],
            "the secondary modifier is visible to the handler"
        );
    })
    .expect("window");
}

#[gpui_kit::test]
fn file_row_height_follows_the_lists_spacing(cx: &mut TestAppContext) {
    let handle = probe(cx, (600., 300.), || {
        div()
            .child(FileRow::new("row-a", "a.txt", ChangeKind::Added))
            .into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_height(window, "row-a", 38.);
        settings::update(cx, |s| s.lines_height = LinesHeight::Compact);
        window.render_frame(cx);
        assert_height(window, "row-a", 34.);
    })
    .expect("window");
}

#[test]
fn a_hovered_selected_file_row_keeps_its_selection_colour() {
    for tokens in [
        crate::theme::builtin::dark(),
        crate::theme::builtin::light(),
    ] {
        assert_eq!(
            super::file_row::hover_background(true, &tokens),
            with_layer(tokens.background_selected, tokens.hover_layer()),
            "08 §4.4: the state layer lies over backgroundSelected"
        );
        assert_ne!(
            super::file_row::hover_background(true, &tokens),
            tokens.hover_layer(),
            "not the bare hover layer, which would look unselected"
        );
        assert_eq!(
            super::file_row::hover_background(false, &tokens),
            tokens.hover_layer(),
            "an unselected row has no background of its own under the layer"
        );
    }
}

#[gpui_kit::test]
fn filter_dropdown_selects_and_closes_on_outside_press(cx: &mut TestAppContext) {
    init_visual(cx);
    let slot: Rc<RefCell<Option<Entity<FilterDropdown>>>> = Rc::default();
    let build_slot = slot.clone();
    let (handle, _) = open_probe(cx, 800., 600., move |window, cx| {
        let dropdown = build_slot
            .borrow_mut()
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    FilterDropdown::new(
                        "upstream",
                        vec![
                            "origin/main".into(),
                            "origin/dev".into(),
                            "fork/main".into(),
                        ],
                        None,
                        window,
                        cx,
                    )
                })
            })
            .clone();
        div()
            .size_full()
            .p(dp(20.))
            .child(observed("outside").w(dp(100.)).h(dp(30.)))
            .child(dropdown)
            .into_any_element()
    });
    let dropdown = slot.borrow().clone().expect("dropdown created");
    let events = Rc::new(RefCell::new(Vec::new()));
    let log = events.clone();
    cx.update(|cx| {
        cx.subscribe(
            &dropdown,
            move |_, event: &super::dropdown::FilterDropdownEvent, _| {
                log.borrow_mut().push(event.clone())
            },
        )
        .detach();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("upstream-button", cx);
        window.render_frame(cx);
        assert!(
            dropdown.read(cx).is_open(),
            "clicking the button opens the popup"
        );
        let button = bounds(window, "upstream-button");
        let popup = bounds(window, "upstream-popup");
        assert!(
            (popup.origin.y - (button.origin.y + button.size.height))
                .as_f32()
                .abs()
                <= 0.5
                && (popup.origin.x - button.origin.x).as_f32().abs() <= 0.5,
            "the popup hangs from the button's bottom-left corner: {popup:?} under {button:?}"
        );
        window.click("upstream-option-1", cx);
        window.render_frame(cx);
        assert_eq!(
            dropdown.read(cx).selected(),
            Some(1),
            "the clicked option is selected"
        );
        assert!(!dropdown.read(cx).is_open(), "choosing closes the popup");

        window.click("upstream-button", cx);
        window.render_frame(cx);
        assert!(dropdown.read(cx).is_open(), "reopened");
        window.click("outside", cx);
        window.render_frame(cx);
        assert!(
            !dropdown.read(cx).is_open(),
            "a press anywhere outside the popup closes it"
        );
    })
    .expect("window");
    assert_eq!(
        events.borrow().as_slice(),
        [super::dropdown::FilterDropdownEvent::Selected(1)],
        "one selection event"
    );
}

#[gpui_kit::test]
fn filter_dropdown_scrolls_to_every_option(cx: &mut TestAppContext) {
    init_visual(cx);
    let slot: Rc<RefCell<Option<Entity<FilterDropdown>>>> = Rc::default();
    let build_slot = slot.clone();
    let (handle, _) = open_probe(cx, 800., 720., move |window, cx| {
        let dropdown = build_slot
            .borrow_mut()
            .get_or_insert_with(|| {
                cx.new(|cx| {
                    let options = (0..60)
                        .map(|i| gpui_kit::SharedString::from(format!("origin/branch-{i:02}")))
                        .collect();
                    FilterDropdown::new("upstream", options, None, window, cx)
                })
            })
            .clone();
        div()
            .size_full()
            .p(dp(20.))
            .child(dropdown)
            .into_any_element()
    });
    let dropdown = slot.borrow().clone().expect("dropdown created");
    let events = Rc::new(RefCell::new(Vec::new()));
    let log = events.clone();
    cx.update(|cx| {
        cx.subscribe(
            &dropdown,
            move |_, event: &super::dropdown::FilterDropdownEvent, _| {
                log.borrow_mut().push(event.clone())
            },
        )
        .detach();
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("upstream-button", cx);
        window.render_frame(cx);
        let popup = bounds(window, "upstream-popup");
        assert!(
            popup.size.height.as_f32() <= 320.5,
            "the card is at most 320 dp high (07 §10.3), got {popup:?}"
        );
        assert_height(window, "upstream-option-0", 48.);
        assert!(
            window.try_find("upstream-option-59").is_none(),
            "rows out of view are not built"
        );
        window.scroll(
            "upstream-option-0",
            gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
            cx,
        );
        window.render_frame(cx);
        let last = bounds(window, "upstream-option-59");
        assert!(
            last.top() >= popup.top() && last.bottom() <= popup.bottom() + px(0.5),
            "the last option scrolls into the card: {last:?} in {popup:?}"
        );
        window.click("upstream-option-59", cx);
        window.render_frame(cx);
        assert_eq!(
            dropdown.read(cx).selected(),
            Some(59),
            "the last option can be chosen"
        );
    })
    .expect("window");
    assert_eq!(
        events.borrow().as_slice(),
        [super::dropdown::FilterDropdownEvent::Selected(59)],
        "choosing the last option emits its index"
    );
}

#[gpui_kit::test]
fn outlined_field_emits_change_and_enter(cx: &mut TestAppContext) {
    init_visual(cx);
    let slot: Rc<RefCell<Option<Entity<OutlinedField>>>> = Rc::default();
    let build_slot = slot.clone();
    let events = Rc::new(RefCell::new(Vec::new()));
    let (handle, _) = open_probe(cx, 600., 200., move |window, cx| {
        let field = build_slot
            .borrow_mut()
            .get_or_insert_with(|| cx.new(|cx| OutlinedField::new("field", window, cx)))
            .clone();
        div().w(dp(300.)).child(field).into_any_element()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let field = slot.borrow().clone().expect("field created");
        let log = events.clone();
        cx.subscribe(&field, move |_, event: &super::field::FieldEvent, _| {
            log.borrow_mut().push(event.clone())
        })
        .detach();
        field.update(cx, |f, cx| f.focus(window, cx));
        window.input("ab", cx);
        window.press("enter", cx);
    })
    .expect("window");
    // Events are delivered when the update that caused them ends.
    let events = events.borrow();
    assert!(
        events.contains(&super::field::FieldEvent::Changed),
        "typing emits Changed, got {events:?}"
    );
    assert_eq!(
        events.last(),
        Some(&super::field::FieldEvent::Enter),
        "Enter emits Enter last, got {events:?}"
    );
}
