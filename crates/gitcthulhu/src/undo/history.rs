//! The undo history popover (21 §5.1, 17 §4.5): a 360 x 420 dp panel under the Undo chevron with the paged list of
//! the oplog (newest first), a context menu per row ("Restore to before / after this operation") and the footer
//! with "Clear undo history". The list is virtualised and read in pages of [`PAGE_SIZE`] entries.

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BoxShadow, Context, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, TestSupportExt as _, UniformListScrollHandle, Window,
    div, point, px, uniform_list,
};

use super::dims;
use super::ids;
use super::texts;
use crate::backend::{
    EntryId, EntryStatus, OplogEntry, PageRequest, RefreshSet, RestoreTarget, SnapshotSide,
};
use crate::icons::icon;
use crate::menus::{self, MenuItem};
use crate::metrics::{self, dp};
use crate::settings::{self, SettingsGlobal};
use crate::tab::TabCtx;
use crate::tab::model::RepoModelEvent;
use crate::tab::read::ReadSlot;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::date::DateFormat;
use crate::widgets::scroll::list_scrollbar;
use crate::widgets::{ObservedExt as _, TextLink, observed};

/// Entries per page (17 §4.4: `PageRequest { limit: 50 }`).
pub const PAGE_SIZE: usize = 50;
/// The next page is requested when the last visible row is this close to the end of the loaded rows.
const PAGE_AHEAD: usize = 10;
/// Key context of the popover (Escape closes it).
pub const POPOVER_CONTEXT: &str = "UndoHistory";
/// Footer note (21 §5.1).
pub const FOOTER_NOTE: &str = "Undo history is stored in the repository under refs/gitcthulhu and is copied by git push --mirror";

gpui_kit::actions!(
    gitcthulhu_undo,
    [
        /// Closes the undo history popover (Escape).
        ClosePopover
    ]
);

/// What the popover tells its owner.
pub enum PopoverEvent {
    /// The popover wants to go away.
    Close,
}

/// How far the first page is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    /// The first page is being read.
    Loading,
    /// At least the first page is shown.
    Ready,
    /// Reading failed (or the oplog is unavailable); the text is shown instead of the list.
    Failed(SharedString),
}

/// The popover entity.
pub struct HistoryPopover {
    ctx: TabCtx,
    entries: Arc<Vec<OplogEntry>>,
    next: Option<EntryId>,
    load: Load,
    paging: bool,
    page_size: usize,
    now_ms: i64,
    date_format: DateFormat,
    slot: ReadSlot,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PopoverEvent> for HistoryPopover {}

impl HistoryPopover {
    /// A popover for the tab of `ctx` that starts reading the first page. `page_size` is [`PAGE_SIZE`] outside
    /// tests.
    pub fn new(
        ctx: TabCtx,
        page_size: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            // An operation, undo or restore that finishes while the popover is open changes the list.
            cx.subscribe(&ctx.model, |this, _, ev: &RepoModelEvent, cx| {
                if ev.0.intersects(RefreshSet::UNDO) {
                    this.reload(cx);
                }
            }),
            cx.observe_global::<SettingsGlobal>(|this, cx| {
                this.date_format = DateFormat::from_settings(&settings::get_or_default(cx));
                cx.notify();
            }),
        ];
        let date_format = DateFormat::from_settings(&settings::get_or_default(cx));
        let mut popover = Self {
            ctx,
            entries: Arc::new(Vec::new()),
            next: None,
            load: Load::Loading,
            paging: false,
            page_size: page_size.clamp(1, 200),
            now_ms: texts::now_ms(),
            date_format,
            slot: ReadSlot::default(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        popover.reload(cx);
        popover
    }

    /// Moves the keyboard focus into the popover.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    /// Asks the owner to remove the popover.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(PopoverEvent::Close);
    }

    /// The loaded entries, newest first.
    pub fn entries(&self) -> &[OplogEntry] {
        &self.entries
    }

    /// Whether more entries can be read.
    pub fn has_more(&self) -> bool {
        self.next.is_some()
    }

    /// How far reading is.
    pub fn load_state(&self) -> &Load {
        &self.load
    }

    /// (Re)reads the first page.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.read(None, cx);
    }

    /// Reads the page after the loaded ones (no-op while reading or at the end).
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        // Any running read blocks paging: starting a page would cancel a reload of the first page (after an
        // operation changed the history) and append to the outdated list instead.
        if self.paging || self.slot.is_running() || !matches!(self.load, Load::Ready) {
            return;
        }
        if let Some(before) = self.next {
            self.read(Some(before), cx);
        }
    }

    fn read(&mut self, before: Option<EntryId>, cx: &mut Context<Self>) {
        let Some(handle) = self.ctx.backend.handle().cloned() else {
            self.load = Load::Failed("Undo history is not available for this tab".into());
            cx.notify();
            return;
        };
        self.paging = before.is_some();
        let limit = self.page_size;
        self.slot.start(
            cx,
            |this| &mut this.slot,
            move |_| {
                handle.oplog_list(PageRequest {
                    before,
                    limit,
                    include_bookkeeping: false,
                })
            },
            move |this, result, cx| {
                this.paging = false;
                match result {
                    Ok(page) => {
                        this.now_ms = texts::now_ms();
                        this.next = page.next;
                        if before.is_some() {
                            let mut all = (*this.entries).clone();
                            all.extend(page.entries);
                            this.entries = Arc::new(all);
                        } else {
                            this.entries = Arc::new(page.entries);
                        }
                        this.load = Load::Ready;
                    }
                    Err(text) => this.load = Load::Failed(text),
                }
                cx.notify();
            },
        );
    }

    /// The context menu of `entry`'s row.
    fn menu_items(&self, entry: &OplogEntry, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let id = entry.id;
        let popover = cx.weak_entity();
        let item = |key: &'static str, label: &'static str, after: bool| {
            let ctx = self.ctx.clone();
            let popover = popover.clone();
            let side = if after {
                SnapshotSide::After
            } else {
                SnapshotSide::Before
            };
            MenuItem::action(key, label, move |window, cx| {
                popover.update(cx, |p, cx| p.dismiss(cx)).ok();
                ctx.with_tab(window, cx, |tab, window, cx| {
                    tab.restore_to(RestoreTarget { entry: id, side }, window, cx);
                });
            })
            .enabled(texts::can_restore(entry, after))
        };
        vec![
            item(
                "undo-restore-before",
                "Restore to before this operation",
                false,
            ),
            item(
                "undo-restore-after",
                "Restore to after this operation",
                true,
            ),
        ]
    }

    fn row(&self, ix: usize, entry: &OplogEntry, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        let undone = matches!(
            entry.status,
            EntryStatus::Undone { .. } | EntryStatus::Superseded
        );
        let items = texts::has_snapshot(entry).then(|| self.menu_items(entry, cx));
        let title_color = if undone {
            tokens.on_background_secondary
        } else {
            tokens.on_background
        };
        let badge = |text: &'static str| {
            div()
                .flex_none()
                .px(dp(dims::BADGE_PAD_H))
                .rounded(dp(metrics::RADIUS))
                .bg(tokens.secondary_surface)
                .text_caption()
                .text_color(tokens.on_background_secondary)
                .whitespace_nowrap()
                .child(text)
        };
        div()
            .id((ids::UNDO_HISTORY_ROW, ix))
            .w_full()
            .h(dp(dims::ROW_H))
            .flex()
            .flex_row()
            .items_center()
            .gap(dp(dims::ROW_GAP))
            .px(dp(dims::ROW_PAD_H))
            .hover(move |s| s.bg(tokens.hover_layer()))
            .child(div().flex_none().child(icon(
                texts::kind_icon(entry.kind),
                dims::ROW_ICON,
                tokens.on_background_secondary,
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_body2()
                    .text_color(title_color)
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(SharedString::from(entry.title.clone())),
            )
            .child(
                div()
                    .flex_none()
                    .text_caption()
                    .text_color(tokens.on_background_secondary)
                    .whitespace_nowrap()
                    .child(SharedString::from(texts::relative_time(
                        entry.finished_at_ms,
                        self.now_ms,
                        &self.date_format,
                    ))),
            )
            .children(texts::badges(entry).into_iter().map(badge))
            .when_some(items, |row, items| {
                row.on_mouse_down(
                    MouseButton::Right,
                    move |event: &MouseDownEvent, window, cx| {
                        menus::open_context_menu(window, cx, event.position, items.clone());
                        cx.stop_propagation();
                    },
                )
            })
            .test_support()
            .into_any_element()
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        let message = |id: &'static str, text: SharedString| {
            observed(id)
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p(dp(dims::ROW_PAD_H))
                .text_body2()
                .text_color(tokens.on_background_secondary)
                .child(text)
                .into_any_element()
        };
        match &self.load {
            Load::Loading => message(ids::UNDO_HISTORY_EMPTY, "Loading\u{2026}".into()),
            Load::Failed(text) => message(ids::UNDO_HISTORY_EMPTY, text.clone()),
            Load::Ready if self.entries.is_empty() => {
                message(ids::UNDO_HISTORY_EMPTY, "No undo history yet".into())
            }
            Load::Ready => {
                let view = cx.weak_entity();
                let entries = self.entries.clone();
                let count = entries.len();
                let more = self.next.is_some();
                let list = uniform_list(ids::UNDO_HISTORY_LIST, count, {
                    move |range, window, cx| {
                        if more && range.end + PAGE_AHEAD >= count {
                            let view = view.clone();
                            window.defer(cx, move |_, cx| {
                                view.update(cx, |this, cx| this.load_more(cx)).ok();
                            });
                        }
                        range
                            .filter_map(|ix| {
                                let entry = entries.get(ix)?;
                                view.update(cx, |this, cx| this.row(ix, entry, cx)).ok()
                            })
                            .collect::<Vec<_>>()
                    }
                })
                .track_scroll(&self.scroll)
                .size_full();
                observed(ids::UNDO_HISTORY_LIST)
                    .relative()
                    .size_full()
                    .child(list)
                    .child(list_scrollbar("undo-history-scrollbar", &self.scroll))
                    .into_any_element()
            }
        }
    }

    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        let popover = cx.weak_entity();
        let ctx = self.ctx.clone();
        div()
            .flex_none()
            .w_full()
            .flex()
            .flex_col()
            .gap(dp(dims::FOOTER_GAP))
            .px(dp(dims::ROW_PAD_H))
            .py(dp(dims::FOOTER_PAD_V))
            .border_t(dp(dims::FOOTER_BORDER))
            .border_color(tokens.divider())
            .child(div().text_body2().child(
                TextLink::new(ids::UNDO_HISTORY_CLEAR, "Clear undo history").on_click(
                    move |window, cx| {
                        popover.update(cx, |p, cx| p.dismiss(cx)).ok();
                        ctx.with_tab(window, cx, |tab, window, cx| {
                            tab.clear_undo_history(window, cx);
                        });
                    },
                ),
            ))
            .child(
                div()
                    .id(ids::UNDO_HISTORY_NOTE)
                    .text_caption()
                    .text_color(tokens.on_background_secondary)
                    .child(FOOTER_NOTE)
                    .test_support(),
            )
            .into_any_element()
    }
}

impl Focusable for HistoryPopover {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for HistoryPopover {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let body = self.body(cx);
        let footer = self.footer(cx);
        div()
            .key_context(POPOVER_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ClosePopover, _, cx| {
                this.dismiss(cx);
                cx.stop_propagation();
            }))
            .on_action(
                cx.listener(|this, _: &gpui_kit::base::input::Escape, _, cx| {
                    this.dismiss(cx);
                    cx.stop_propagation();
                }),
            )
            // Presses inside the popover must not reach the dismissing backdrop behind it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .w(dp(dims::POPOVER_W))
            .h(dp(dims::POPOVER_H))
            .flex()
            .flex_col()
            .rounded(dp(metrics::RADIUS))
            .bg(tokens.background)
            .overflow_hidden()
            .shadow(vec![BoxShadow {
                color: tokens.menu_shadow(),
                offset: point(px(0.), px(2.)),
                blur_radius: px(5.),
                spread_radius: px(1.),
                inset: false,
            }])
            .when(!tokens.is_light, |m| {
                m.border(dp(2.)).border_color(tokens.popup_border())
            })
            .child(div().flex_1().min_h_0().w_full().child(body))
            .child(footer)
            .observed(ids::UNDO_HISTORY)
    }
}
