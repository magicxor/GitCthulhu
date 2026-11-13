use core::prelude::v1::test;

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{App, Entity, TestAppContext, div, point, px};

use super::*;
use crate::test_support::geometry::{assert_height, bounds, init_visual, open_probe};

fn target(kind: MenuKind, cx: &mut App) -> MenuTarget {
    MenuTarget {
        kind,
        tab: crate::test_support::inert_tab_ctx(cx),
        branch: None,
        upstream: None,
        commit: None,
        path: None,
        is_current: false,
        checked_out_elsewhere: false,
    }
}

fn labels(items: &[MenuItem]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            MenuItem::Action { id, .. } => id.to_string(),
            MenuItem::Separator => "---".to_owned(),
        })
        .collect()
}

fn ext_one(_: &MenuTarget, items: &mut Vec<MenuItem>, _: &App) {
    items.push(MenuItem::action("x1", "Extra one", |_, _| {}));
}

fn ext_two(_: &MenuTarget, items: &mut Vec<MenuItem>, _: &App) {
    items.push(MenuItem::action("x2", "Extra two", |_, _| {}));
}

fn ext_other_kind(_: &MenuTarget, items: &mut Vec<MenuItem>, _: &App) {
    items.push(MenuItem::action("nope", "Other menu", |_, _| {}));
}

#[gpui_kit::test]
fn menu_extender_inserted_at_slot_with_separator(cx: &mut TestAppContext) {
    cx.update(|cx| {
        register_extender(cx, MenuKind::LocalBranch, MenuSlot::AfterCheckout, ext_one);
        register_extender(cx, MenuKind::LocalBranch, MenuSlot::AfterCheckout, ext_two);
        register_extender(cx, MenuKind::LocalBranch, MenuSlot::End, ext_other_kind);
        register_extender(cx, MenuKind::Tag, MenuSlot::AfterCheckout, ext_other_kind);

        let target = target(MenuKind::LocalBranch, cx);
        let mut items = vec![MenuItem::action("checkout", "Checkout branch", |_, _| {})];
        extend(cx, MenuSlot::AfterCheckout, &target, &mut items);
        items.push(MenuItem::action("rename", "Rename branch", |_, _| {}));
        extend(cx, MenuSlot::AfterRenameGroup, &target, &mut items);
        assert_eq!(
            labels(&items),
            ["checkout", "x1", "x2", "---", "rename"],
            "extenders run in registration order at their slot, followed by one separator; other slots and kinds add nothing"
        );
        extend(cx, MenuSlot::End, &target, &mut items);
        assert_eq!(
            labels(&items).last().map(String::as_str),
            Some("nope"),
            "the End slot extender of the same kind was applied, with no trailing separator"
        );
    });
}

#[gpui_kit::test]
fn extend_without_any_registration_is_a_no_op(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let mut items = vec![MenuItem::Separator];
        let commit_target = target(MenuKind::Commit, cx);
        extend(cx, MenuSlot::End, &commit_target, &mut items);
        assert_eq!(
            items.len(),
            1,
            "nothing was added and no separator appended"
        );
    });
}

fn mount_host(cx: &mut TestAppContext) -> (gpui_kit::AnyWindowHandle, Entity<MenuHost>) {
    init_visual(cx);
    let host = cx.update(MenuHost::install);
    let layer = host.clone();
    let (handle, _) = open_probe(cx, 1280., 720., move |_, _| {
        div()
            .relative()
            .size_full()
            .child(layer.clone())
            .into_any_element()
    });
    (handle, host)
}

#[gpui_kit::test]
fn menu_item_height_and_min_width(cx: &mut TestAppContext) {
    let (handle, _host) = mount_host(cx);
    cx.update_window(handle, |_, window, cx| {
        let items = vec![
            MenuItem::action("a", "Short", |_, _| {}).with_icon(AppIcon::Open),
            MenuItem::Separator,
            MenuItem::action("b", "Another entry", |_, _| {}),
        ];
        open_context_menu(window, cx, point(px(100.), px(100.)), items);
        window.render_frame(cx);
        let a = bounds(window, "menu-item-a");
        let menu = bounds(window, ids::MENU);
        assert!(
            (a.size.height.as_f32() - 32.).abs() <= 0.5,
            "a menu item is about 32 dp high (8 + 13 x 1.21 + 8), got {}",
            a.size.height.as_f32()
        );
        assert!(
            menu.size.width.as_f32() >= metrics::MENU_MIN_W,
            "the menu is at least 180 dp wide, got {}",
            menu.size.width.as_f32()
        );
        // Two items, one 10 dp separator (2 + 2 x 4) and the 2 dp dark-theme border on both sides.
        let expected = 2. * a.size.height.as_f32() + 10. + 4.;
        assert!(
            (menu.size.height.as_f32() - expected).abs() <= 1.0,
            "menu height {} should be {expected}",
            menu.size.height.as_f32()
        );
        assert_eq!(menu.origin.x, px(100.), "the menu opens at the pointer");
        assert_height(window, "menu-item-b", a.size.height.as_f32());
    })
    .expect("window");
}

#[gpui_kit::test]
fn clicking_an_item_runs_it_and_closes_the_menu(cx: &mut TestAppContext) {
    let (handle, host) = mount_host(cx);
    let clicked = Rc::new(RefCell::new(0));
    let counter = clicked.clone();
    cx.update_window(handle, |_, window, cx| {
        let items = vec![MenuItem::action("go", "Go", move |_, _| {
            *counter.borrow_mut() += 1
        })];
        open_context_menu(window, cx, point(px(10.), px(10.)), items);
        window.render_frame(cx);
        assert!(host.read(cx).is_open(), "the menu is open");
        window.click("menu-item-go", cx);
        assert_eq!(*clicked.borrow(), 1, "the handler ran once");
        assert!(!host.read(cx).is_open(), "clicking an item closes the menu");
    })
    .expect("window");
}

#[gpui_kit::test]
fn disabled_items_do_not_run(cx: &mut TestAppContext) {
    let (handle, host) = mount_host(cx);
    let clicked = Rc::new(RefCell::new(0));
    let counter = clicked.clone();
    cx.update_window(handle, |_, window, cx| {
        let items = vec![
            MenuItem::action("off", "Off", move |_, _| *counter.borrow_mut() += 1).enabled(false),
        ];
        open_context_menu(window, cx, point(px(10.), px(10.)), items);
        window.render_frame(cx);
        window.click("menu-item-off", cx);
        assert_eq!(*clicked.borrow(), 0, "a disabled item has no handler");
        assert!(host.read(cx).is_open(), "and does not close the menu");
    })
    .expect("window");
}

#[gpui_kit::test]
fn escape_closes_the_menu(cx: &mut TestAppContext) {
    let (handle, host) = mount_host(cx);
    cx.update_window(handle, |_, window, cx| {
        open_context_menu(
            window,
            cx,
            point(px(10.), px(10.)),
            vec![MenuItem::action("a", "A", |_, _| {})],
        );
        window.render_frame(cx);
        window.press("escape", cx);
        assert!(!host.read(cx).is_open(), "Esc dismisses the menu");
    })
    .expect("window");
}

#[gpui_kit::test]
fn empty_menus_do_not_open(cx: &mut TestAppContext) {
    let (handle, host) = mount_host(cx);
    cx.update_window(handle, |_, window, cx| {
        open_context_menu(window, cx, point(px(10.), px(10.)), Vec::new());
        assert!(!host.read(cx).is_open(), "nothing to show, nothing opens");
    })
    .expect("window");
}
