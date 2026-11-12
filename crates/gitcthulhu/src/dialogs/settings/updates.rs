//! The Updates settings page (D0.5; owned by X5; spec 21 §5.4). Shown only when update checks are possible (the build
//! has a manifest URL, `update::settings_available`).
//!
//! * "Check for updates automatically": the `update_check` setting (a check at startup and every 24 hours),
//! * "Update channel" (Release / Nightly): only for builds whose manifest URL has a `{channel}` placeholder; the
//!   services read the channel when they start, so a change applies after a restart,
//! * "Check for updates" with the "Check now" button and the state of the last check.
//!
//! The page is a plain function (the dialog hosts it as a list of rows), so the channel picker is a pair of chips
//! rather than the dialog's in-view dropdown (21 §6 deviation).

use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
    transparent_black,
};

use super::{SettingRow, ids as dialog_ids, sm};
use crate::backend::GitTime;
use crate::backend::update::nightly_channel_available;
use crate::metrics::{self, dp};
use crate::settings::{self, UpdateChannel};
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};
use crate::update::{self, CheckKind};
use crate::widgets::date::{DateFormat, format_date, local_offset_minutes};
use crate::widgets::{AppSwitch, PrimaryButton};

/// Row keys and element ids of the page (tests, QA).
pub mod ids {
    /// Row "Check for updates automatically" (its switch is `settings::ids::switch(AUTO)`).
    pub const AUTO: &str = "update-auto";
    /// Row "Update channel".
    pub const CHANNEL: &str = "update-channel";
    /// The "Release" chip.
    pub const CHANNEL_RELEASE: &str = "update-channel-release";
    /// The "Nightly" chip.
    pub const CHANNEL_NIGHTLY: &str = "update-channel-nightly";
    /// Row "Check for updates".
    pub const CHECK: &str = "update-check-row";
    /// The "Check now" button.
    pub const CHECK_NOW: &str = "update-check-now";
}

/// Height of a channel chip (the height of a text field of the dialog).
const CHIP_H: f32 = 32.;
/// Inner padding of the chip text.
const CHIP_PAD_H: f32 = 8.;

/// The page content: the setting rows of the category.
pub fn page(_window: &mut Window, cx: &mut App) -> AnyElement {
    content(cx, nightly_channel_available())
}

/// The rows, with the channel picker when `with_channel` (a build with a nightly manifest).
pub(crate) fn content(cx: &mut App, with_channel: bool) -> AnyElement {
    let current = settings::get_or_default(cx);
    let snapshot = update::snapshot(cx);
    let mut rows = vec![
        SettingRow::new(ids::AUTO, "Check for updates automatically")
            .subtitle("Looks for a new version of GitCthulhu when it starts and then once a day")
            .control(
                AppSwitch::new(dialog_ids::switch(ids::AUTO), current.update_check)
                    .on_toggle(|value, _, cx| settings::update(cx, |s| s.update_check = value)),
            )
            .into_any_element(),
    ];
    if with_channel {
        rows.push(
            SettingRow::new(ids::CHANNEL, "Update channel")
                .subtitle("Which releases to look for. Applies after GitCthulhu restarts")
                .control(channel_chips(*cx.tokens(), current.update_channel))
                .into_any_element(),
        );
    }
    rows.push(check_row(&snapshot, &current));
    div()
        .w_full()
        .flex()
        .flex_col()
        .children(rows)
        .into_any_element()
}

/// The "Check for updates" row.
fn check_row(snapshot: &update::UpdateSnapshot, current: &settings::Settings) -> AnyElement {
    let version = env!("CARGO_PKG_VERSION");
    let subtitle: SharedString = if snapshot.checking {
        "Checking for updates…".into()
    } else if let Some(found) = &snapshot.notice {
        format!("GitCthulhu {} is available", found.version).into()
    } else if snapshot.last_checked.is_some() {
        format!("GitCthulhu {version} is up to date").into()
    } else {
        format!("GitCthulhu {version} is installed").into()
    };
    let mut row = SettingRow::new(ids::CHECK, "Check for updates").subtitle(subtitle);
    if let Some(at) = snapshot.last_checked {
        row = row.note(format!("Last checked: {}", format_time(at, current)), false);
    }
    if let Some(reason) = &snapshot.last_error {
        row = row.note(format!("The last check failed: {reason}"), true);
    }
    row.control(
        PrimaryButton::new(ids::CHECK_NOW, "Check now")
            .enabled(!snapshot.checking)
            .on_click(|_, cx| update::check_now(cx, CheckKind::Manual)),
    )
    .into_any_element()
}

/// `at` in the user's date format, with the time.
fn format_time(at: SystemTime, current: &settings::Settings) -> String {
    let seconds = |t: SystemTime| {
        t.duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    };
    let time = GitTime {
        seconds: seconds(at),
        offset_minutes: local_offset_minutes(seconds(at)),
    };
    let now = GitTime {
        seconds: seconds(SystemTime::now()),
        offset_minutes: time.offset_minutes,
    };
    format_date(
        time,
        now,
        time.offset_minutes,
        &DateFormat::from_settings(current),
        true,
    )
}

/// The Release / Nightly picker.
fn channel_chips(tokens: Tokens, selected: UpdateChannel) -> AnyElement {
    div()
        .w(dp(sm::DROPDOWN_W))
        .flex()
        .flex_row()
        .child(chip(
            tokens,
            ids::CHANNEL_RELEASE,
            "Release",
            UpdateChannel::Release,
            selected,
        ))
        .child(chip(
            tokens,
            ids::CHANNEL_NIGHTLY,
            "Nightly",
            UpdateChannel::Nightly,
            selected,
        ))
        .into_any_element()
}

fn chip(
    tokens: Tokens,
    id: &'static str,
    label: &'static str,
    channel: UpdateChannel,
    selected: UpdateChannel,
) -> impl gpui_kit::IntoElement {
    let on = channel == selected;
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .h(dp(CHIP_H))
        .px(dp(CHIP_PAD_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded(dp(metrics::RADIUS))
        .border(dp(1.))
        .border_color(if on {
            tokens.primary
        } else {
            tokens.field_border()
        })
        .bg(if on {
            tokens.primary
        } else {
            transparent_black()
        })
        .text_color(if on {
            tokens.on_primary
        } else {
            tokens.on_background
        })
        .cursor_pointer()
        .text_body1()
        .whitespace_nowrap()
        .child(label)
        .on_click(move |_, _, cx| settings::update(cx, |s| s.update_channel = channel))
        .test_support()
}
