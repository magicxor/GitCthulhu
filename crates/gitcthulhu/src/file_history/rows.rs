//! One commit row of the file history list (09 §2.2): avatar, message, short hash and date.

use std::rc::Rc;

use gpui_kit::{
    App, ClickEvent, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use super::ids;
use crate::backend::{ByteSlice as _, FileHistoryEntry};
use crate::metrics::dp;
use crate::metrics::history as m;
use crate::theme::{TextStyleExt as _, Tokens};
use crate::widgets::TooltipExt as _;
use crate::widgets::avatar::Avatar;
use crate::widgets::date::{DateFormat, format_date, local_offset_minutes};

/// A history entry with the strings its row shows.
#[derive(Clone)]
pub struct HistoryItem {
    /// The backend entry (the diff target).
    pub entry: FileHistoryEntry,
    /// First message line.
    pub summary: SharedString,
    /// Seven-digit hash.
    pub short_id: SharedString,
    /// The date as shown (and in the tooltip).
    pub date: SharedString,
    /// Author name and e-mail (the avatar).
    pub name: SharedString,
    /// Author e-mail.
    pub email: SharedString,
}

impl HistoryItem {
    /// The row strings of `entry`, with the date formatted for `now`.
    pub fn new(entry: FileHistoryEntry, now: crate::backend::GitTime, format: &DateFormat) -> Self {
        let person = &entry.commit.author.person;
        let item = Self {
            summary: SharedString::from(entry.commit.summary.to_string()),
            short_id: SharedString::from(entry.commit.id.to_hex_with_len(7).to_string()),
            date: SharedString::default(),
            name: SharedString::from(person.name.to_str_lossy().into_owned()),
            email: SharedString::from(person.email.to_str_lossy().into_owned()),
            entry,
        };
        item.with_date(now, format)
    }

    /// The same item with the date re-formatted (settings changed).
    pub fn with_date(mut self, now: crate::backend::GitTime, format: &DateFormat) -> Self {
        let time = self.entry.commit.author.time;
        let offset = local_offset_minutes(now.seconds);
        // 09 §2.2: `toSmartSystemString()`: relative words per the setting, no time of day.
        self.date = SharedString::from(format_date(time, now, offset, format, false));
        self
    }
}

/// What the rows of one frame share.
pub(super) struct RowEnv {
    pub tokens: Tokens,
    pub on_select: Rc<dyn Fn(usize, &mut Window, &mut App)>,
}

/// Row `ix` of the list.
pub(super) fn row(env: &RowEnv, item: &HistoryItem, ix: usize) -> impl IntoElement + use<> {
    let t = &env.tokens;
    let on_select = env.on_select.clone();
    let hover = t.hover_layer();
    div()
        .id((ids::HISTORY_ROW, ix))
        .w_full()
        .h(dp(m::ROW_H))
        .py(dp(m::ROW_PAD_V))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_color(t.on_background)
        .hover(move |s| s.bg(hover))
        .on_click(move |_: &ClickEvent, window, cx| on_select(ix, window, cx))
        .child(div().flex_none().px(dp(m::ROW_PAD_H)).child(Avatar::new(
            item.name.clone(),
            item.email.clone(),
            m::AVATAR,
        )))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().text_body1().child(item.summary.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .text_body2()
                        .text_color(t.on_background_secondary)
                        .child(item.short_id.clone())
                        .child(div().flex_1())
                        .child(
                            div()
                                .id((ids::HISTORY_DATE, ix))
                                .px(dp(m::ROW_PAD_H))
                                .whitespace_nowrap()
                                .child(item.date.clone())
                                .delayed_tooltip(item.date.clone())
                                .test_support(),
                        ),
                ),
        )
        .test_support()
}
