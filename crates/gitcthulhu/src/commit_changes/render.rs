//! Rendering of the commit changes view (08 §5.1): files column on top, message box and author footer below.

use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::base::SelectableText;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
    uniform_list,
};

use super::rows::blame_revision;
use super::{
    AVATAR_SIZE, COMMIT_CHANGES_CONTEXT, CommitChangesView, FOOTER_H, LIST_PAD_BOTTOM, Loaded,
    MESSAGE_H, SelectNextFile, SelectPreviousFile, ids,
};
use crate::backend::{ByteSlice as _, GitTime};
use crate::icons::AppIcon;
use crate::menus::{self, MenuKind, MenuTarget};
use crate::metrics::{self, dp};
use crate::tab::ui::FileList;
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};
use crate::widgets::avatar::Avatar;
use crate::widgets::date::{DateFormat, format_date, local_offset_minutes};
use crate::widgets::file_row::{DirectoryRow, FileRow};
use crate::widgets::file_tree::TreeRow;
use crate::widgets::scroll::list_scrollbar;
use crate::widgets::{HeaderBar34, IconButton, LinearProgress, TooltipText, observed};

/// Id of the list element (scroll state) and of its scrollbar.
const LIST_ELEMENT: &str = "commit-changes-rows";
const SCROLLBAR: &str = "commit-changes-scrollbar";
const MESSAGE_SCROLL: &str = "commit-changes-message-scroll";
const MESSAGE_TEXT: &str = "commit-changes-message-text";
const AUTHOR: &str = "commit-changes-author";
const DATE: &str = "commit-changes-date";

impl CommitChangesView {
    pub(super) fn render_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        self.ensure_rows(cx);
        let tokens = *cx.tokens();
        observed(ids::COMMIT_CHANGES)
            .size_full()
            .pr(dp(metrics::PANE_EDGE_PAD))
            .pb(dp(metrics::PANE_EDGE_PAD))
            .flex()
            .flex_col()
            .child(self.files_column(&tokens, window, cx))
            .child(self.message_and_footer(&tokens, cx))
    }

    fn files_column(
        &mut self,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let _ = window;
        let view = cx.entity().downgrade();
        let tree = self.rows_tree;
        let toggle_tree = {
            let view = view.clone();
            move |_: &mut Window, cx: &mut gpui_kit::App| {
                view.update(cx, |v, cx| v.toggle_tree_mode(cx)).ok();
            }
        };
        let toggle_search = {
            move |window: &mut Window, cx: &mut gpui_kit::App| {
                view.update(cx, |v, cx| v.toggle_search(window, cx)).ok();
            }
        };
        let header = HeaderBar34::new()
            .id(ids::HEADER)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl(dp(16.))
                    .pr(dp(8.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_body2()
                    .text_color(tokens.on_background)
                    .child("Files changed"),
            )
            .child(
                IconButton::new(
                    ids::TOGGLE_TREE,
                    if tree { AppIcon::List } else { AppIcon::Tree },
                )
                .on_click(toggle_tree),
            )
            .child(IconButton::new(ids::TOGGLE_SEARCH, AppIcon::Search).on_click(toggle_search));

        // 08 §5.1: the 4 dp bottom padding lies outside the background, so the gap above the message box shows the
        // pane's `surface` colour (the files column and the message box would otherwise merge).
        let column = observed(ids::FILES)
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .bg(tokens.background)
            .child(header)
            .children(self.search.as_ref().map(|s| s.field.clone()))
            .when(self.show_progress, |col| {
                col.child(
                    observed(ids::PROGRESS)
                        .w_full()
                        .flex_none()
                        .child(LinearProgress::new().color(tokens.primary)),
                )
            })
            .child(self.list(tokens, cx));
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .pb(dp(LIST_PAD_BOTTOM))
            .key_context(COMMIT_CHANGES_CONTEXT)
            .on_action(cx.listener(|v, _: &SelectPreviousFile, _, cx| v.move_selection(false, cx)))
            .on_action(cx.listener(|v, _: &SelectNextFile, _, cx| v.move_selection(true, cx)))
            .child(div().track_focus(&self.focus).size_0())
            .child(column)
    }

    /// The scrolling list of rows (virtualised: only the visible rows are built), or the error line.
    fn list(&mut self, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let container = observed(ids::LIST).relative().flex_1().min_h_0().w_full();
        if let Some(error) = &self.error {
            return container
                .child(
                    observed(ids::ERROR)
                        .w_full()
                        .p(dp(16.))
                        .text_body2()
                        .text_color(tokens.error)
                        .child(error.clone()),
                )
                .into_any_element();
        }
        let Some(loaded) = &self.loaded else {
            return container.into_any_element();
        };
        let rows = self.rows.clone();
        let files = loaded.files.clone();
        let tree = self.rows_tree;
        let current = self.list_is_current();
        let selected_commit = loaded.key.0;
        let first_parent = loaded.first_parent;
        let selected = self
            .ctx
            .ui
            .read(cx)
            .files
            .single(FileList::CommitFiles)
            .cloned();
        let ctx = Rc::new(self.ctx.clone());
        let view = cx.entity().downgrade();
        let focus = self.focus.clone();
        let count = rows.len();
        let list = uniform_list(LIST_ELEMENT, count, move |range, _window, _cx| {
            range
                .filter_map(|ix| {
                    Some(match rows.rows.get(ix)? {
                        TreeRow::File { index, depth, .. } => {
                            let file = files.get(*rows.visible.get(*index)?)?;
                            let path = file.path.clone();
                            let ui = ctx.ui.clone();
                            let click_path = path.clone();
                            let menu_ctx = ctx.clone();
                            let menu_path = path.clone();
                            let menu_commit = blame_revision(file, selected_commit, first_parent);
                            let click_focus = focus.clone();
                            let row = FileRow::new((ids::ROW, ix), path.to_vec(), file.kind)
                                .selected(selected.as_ref() == Some(&path))
                                .on_click(move |event: &ClickEvent, window, cx| {
                                    // Single selection only (08 §5.2): the modifiers are ignored by
                                    // `click_file` for this list; a stale list (the next commit is still
                                    // loading) is inert.
                                    if !current {
                                        return;
                                    }
                                    let path = click_path.clone();
                                    ui.update(cx, |ui, cx| {
                                        ui.click_file(
                                            FileList::CommitFiles,
                                            path.clone(),
                                            event.modifiers(),
                                            std::slice::from_ref(&path),
                                            cx,
                                        )
                                    });
                                    // Up / Down move the selection from here on.
                                    window.focus(&click_focus, cx);
                                })
                                .on_context_menu(move |event, window, cx| {
                                    if !current {
                                        return;
                                    }
                                    let target = MenuTarget {
                                        kind: MenuKind::CommitFile,
                                        tab: (*menu_ctx).clone(),
                                        branch: None,
                                        upstream: None,
                                        commit: Some(menu_commit),
                                        path: Some(menu_path.clone()),
                                        is_current: false,
                                        checked_out_elsewhere: false,
                                    };
                                    let items = menus::commit_files::commit_file(&target, cx);
                                    menus::open_context_menu(window, cx, event.position, items);
                                });
                            if tree { row.tree(*depth) } else { row }.into_any_element()
                        }
                        TreeRow::Dir {
                            path,
                            name,
                            depth,
                            expanded,
                        } => {
                            let view = view.clone();
                            let dir = path.clone();
                            let toggle_focus = focus.clone();
                            DirectoryRow::new(
                                (ids::DIR_ROW, ix),
                                SharedString::from(name.to_str_lossy().into_owned()),
                                *depth,
                                *expanded,
                            )
                            .on_toggle(move |window, cx| {
                                window.focus(&toggle_focus, cx);
                                view.update(cx, |v, cx| v.toggle_dir(dir.clone(), cx)).ok();
                            })
                            .into_any_element()
                        }
                    })
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .size_full();
        container
            .child(list)
            .child(list_scrollbar(SCROLLBAR, &self.scroll))
            .into_any_element()
    }

    /// The 120 dp message box and the 72 dp author footer (08 §5.1).
    fn message_and_footer(&self, tokens: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let message = observed(ids::MESSAGE)
            .h(dp(MESSAGE_H))
            .w_full()
            .flex_none()
            .overflow_hidden()
            .child(
                div()
                    .id(MESSAGE_SCROLL)
                    .size_full()
                    .overflow_y_scroll()
                    .p(dp(8.))
                    .text_body1()
                    .text_color(tokens.on_background)
                    .children(
                        self.loaded
                            .as_ref()
                            .map(|l| SelectableText::new(MESSAGE_TEXT, l.message.clone())),
                    ),
            );
        let footer = observed(ids::FOOTER)
            .h(dp(FOOTER_H))
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .bg(tokens.tertiary_surface)
            .children(
                self.loaded
                    .as_ref()
                    .map(|loaded| self.footer_content(loaded, tokens, cx)),
            );
        div()
            .flex_none()
            .w_full()
            .rounded(dp(metrics::RADIUS))
            .overflow_hidden()
            .bg(tokens.background)
            .flex()
            .flex_col()
            .child(message)
            .child(footer)
    }

    fn footer_content(
        &self,
        loaded: &Loaded,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let format = cx
            .try_global::<crate::settings::SettingsGlobal>()
            .map(|g| DateFormat::from_settings(&g.0))
            .unwrap_or_default();
        let absolute = DateFormat {
            relative: false,
            ..format.clone()
        };
        let now = GitTime {
            seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            offset_minutes: 0,
        };
        let time = loaded.committer_time;
        let offset = local_offset_minutes(time.seconds);
        let date = format_date(time, now, offset, &format, true);
        let date_tooltip = format_date(time, now, offset, &absolute, true);

        let hash = div()
            .id(ids::HASH)
            .cursor_pointer()
            .text_body2()
            .text_color(tokens.on_background_secondary)
            .child(loaded.short_hash.clone())
            .on_click(move |_, _, cx| {
                view.update(cx, |v, cx| v.copy_hash(cx)).ok();
            })
            .test_support();
        let hash_row = div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .child(hash)
            .when(self.copied, |row| {
                row.child(
                    observed(ids::COPIED)
                        .pl(dp(4.))
                        .text_caption()
                        .text_color(tokens.primary_variant)
                        .child("Copied!"),
                )
            })
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .px(dp(16.))
                    .text_body2()
                    .text_color(tokens.on_background_secondary)
                    .child(TooltipText::new(DATE, date, date_tooltip)),
            );
        let author_row = div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_body2()
            .text_color(tokens.on_background)
            .child(TooltipText::new(
                AUTHOR,
                loaded.author_name.clone(),
                loaded.author_email.clone(),
            ));
        div()
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .child(
                div().flex_none().px(dp(16.)).child(
                    observed(ids::AVATAR)
                        .size(dp(AVATAR_SIZE))
                        .child(Avatar::new(
                            loaded.author_name.clone(),
                            loaded.author_email.clone(),
                            AVATAR_SIZE,
                        )),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .child(author_row)
                    .child(hash_row),
            )
    }
}
