//! The "Update {version} available" button of the bottom bar (07 §7, 21 §5.6).
//!
//! Gitnuro's own slot: a `SecondaryButton` (primary background) before the "Version x.y" link. A click opens the
//! download; the right-click menu (the GitCthulhu addition) snoozes the notice.

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _,
    Styled as _, Window, div,
};

use super::{SNOOZE_DAY_HOURS, SNOOZE_WEEK_HOURS, ids, notice, snooze};
use crate::backend::update::UpdateNotice;
use crate::menus::{self, MenuItem};
use crate::shell::platform;
use crate::widgets::SecondaryButton;

/// The button text.
pub fn label(version: &str) -> String {
    format!("Update {version} available")
}

/// The slot content for the current state.
pub(super) fn slot(_window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let found = notice(cx)?;
    Some(button(&found))
}

fn button(found: &UpdateNotice) -> AnyElement {
    let url = found.url.clone();
    div()
        .flex_none()
        .on_mouse_down(MouseButton::Right, |event, window, cx| {
            menus::open_context_menu(window, cx, event.position, snooze_menu());
        })
        .child(
            SecondaryButton::new(ids::NOTICE, label(&found.version))
                .on_click(move |_, cx| open_download(cx, &url)),
        )
        .into_any_element()
}

/// The entries of the notice's context menu.
pub fn snooze_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::action(ids::REMIND_TOMORROW, "Remind me tomorrow", |_, cx| {
            snooze(cx, SNOOZE_DAY_HOURS)
        }),
        MenuItem::action(ids::REMIND_WEEK, "Remind me in a week", |_, cx| {
            snooze(cx, SNOOZE_WEEK_HOURS)
        }),
    ]
}

/// Opens the download (or release page) in the browser. Only `https` links are opened.
pub(super) fn open_download(cx: &mut App, url: &str) {
    if url.starts_with("https://") {
        platform::open_url(cx, url);
    } else {
        tracing::warn!("not opening the update link {url}: it is not an https link");
    }
}
