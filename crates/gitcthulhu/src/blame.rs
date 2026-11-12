//! The blame view and its minimized bar (owned by G8; 09 §2.1). The tab creates it while `RepoUi::blame` is set; it
//! renders the full view in place of the log/diff column or, when minimized, the 52 dp bar under that column.
//!
//! Loading: one blame read per `(path, rev)` through a [`ReadSlot`]; the syntax tokens follow in a second read so
//! the rows appear first. A failed read closes the view and shows an error toast ("File blaming failed"). Opening a
//! single-file diff minimizes a full blame (09 §1.1).

use std::rc::Rc;

use gpui_kit::{
    App, Context, Entity, FocusHandle, Font, FontFeatures, FontStyle, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Render, SharedString, Styled as _, Subscription,
    UniformListScrollHandle, Window, div, uniform_list,
};

use crate::app_view::show_toast;
use crate::backend::{BlameRev, ByteSlice as _, GitErrorKind, RelaPath};
use crate::diff::highlight::Highlighter;
use crate::fonts::FONT_MONO;
use crate::metrics::blame as m;
use crate::metrics::{dp, dpx};
use crate::notifications::ToastKind;
use crate::tab::ui::{BlameTarget, CloseableView, RepoUi, RepoUiEvent};
use crate::tab::{RepoTabEvent, TabCtx};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::scroll::list_scrollbar;
use crate::widgets::{IconButton, ObservedExt as _, PrimaryButton, observed};
use crate::{icons::AppIcon, tab::read::ReadSlot};

pub mod doc;
mod header;
mod rows;
mod selection;

pub(crate) use header::view_header;
pub(crate) use rows::token_color;

#[cfg(test)]
mod tests;

use doc::BlameDoc;
use rows::RowEnv;
use selection::{CodeColumn, SelectionBridge};

gpui_kit::actions!(
    gitcthulhu_blame,
    [
        /// Copies the selected code (full source lines; the kit's window copy trims the text).
        BlameCopy,
    ]
);

/// Key context of the blame view (focused by a click into it).
pub const BLAME_CONTEXT: &str = "Blame";

/// Element ids of the blame view.
pub mod ids {
    /// The blame view (full or minimized).
    pub const BLAME: &str = "blame-view";
    /// The header bar of the full view.
    pub const BLAME_HEADER: &str = "blame-header";
    /// The header's close button.
    pub const BLAME_CLOSE: &str = "blame-close";
    /// The virtual list of lines.
    pub const BLAME_LIST: &str = "blame-list";
    /// Gutter cell of line `ix` (tuple id).
    pub const BLAME_GUTTER: &str = "blame-gutter";
    /// Commit cell of line `ix` (tuple id).
    pub const BLAME_COMMIT: &str = "blame-commit";
    /// Code cell of line `ix` (tuple id).
    pub const BLAME_CODE: &str = "blame-code";
    /// "Show" button of the minimized bar.
    pub const BLAME_SHOW: &str = "blame-show";
    /// Close button of the minimized bar.
    pub const BLAME_MINIMIZED_CLOSE: &str = "blame-minimized-close";
    /// The path text of the header.
    pub const BLAME_TITLE: &str = "blame-title";
}

/// Opens the blame of `path` at `rev` (replaces the centre pane; Esc closes it). The menu entries "Blame file" of
/// the status lists and the commit-file list call this.
pub fn open(ctx: &TabCtx, path: RelaPath, rev: BlameRev, cx: &mut App) {
    ctx.ui.update(cx, |ui, cx| {
        ui.blame = Some(BlameTarget {
            path,
            rev,
            minimized: false,
        });
        ui.push_view(CloseableView::Blame, cx);
        cx.emit(RepoUiEvent::CenterChanged);
    });
}

/// Closes the blame (and drops its Esc entry).
pub fn close(ctx: &TabCtx, cx: &mut App) {
    ctx.ui.update(cx, close_in);
}

/// Closes the blame from one of its buttons and gives the keyboard back to the frame (the view, which may hold
/// the focus, is dropped).
fn close_and_refocus(ctx: &TabCtx, window: &mut Window, cx: &mut App) {
    close(ctx, cx);
    ctx.with_tab(window, cx, |tab, window, cx| tab.focus(window, cx));
}

fn close_in(ui: &mut RepoUi, cx: &mut Context<RepoUi>) {
    if ui.blame.take().is_none() {
        return;
    }
    ui.views.remove(CloseableView::Blame);
    cx.emit(RepoUiEvent::ViewStackChanged);
    cx.emit(RepoUiEvent::CenterChanged);
    cx.notify();
}

/// Shows the minimized blame in full again.
fn expand(ctx: &TabCtx, cx: &mut App) {
    ctx.ui.update(cx, |ui, cx| {
        let Some(blame) = ui.blame.as_mut().filter(|b| b.minimized) else {
            return;
        };
        blame.minimized = false;
        ui.push_view(CloseableView::Blame, cx);
        cx.emit(RepoUiEvent::CenterChanged);
    });
}

/// The blame of `RepoUi::blame`.
pub struct BlameView {
    ctx: TabCtx,
    /// What is loaded or loading.
    target: Option<(RelaPath, BlameRev)>,
    doc: Option<BlameDoc>,
    load: ReadSlot,
    highlight: ReadSlot,
    scroll: UniformListScrollHandle,
    bridge: SelectionBridge,
    /// Focused by a click into the view: the `Blame` key context (copy) and Gitnuro's focusable root (09 §2.1).
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl BlameView {
    /// The blame view of the tab of `ctx`.
    pub fn new(ctx: TabCtx, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&ctx.ui, |this, _, event: &RepoUiEvent, cx| {
                this.on_ui_event(event, cx)
            }),
            cx.subscribe(&ctx.events, |this, _, event: &RepoTabEvent, cx| {
                // A new log means HEAD may have moved: lines that were uncommitted are committed now.
                if matches!(event, RepoTabEvent::LogReloaded { .. })
                    && matches!(this.target, Some((_, BlameRev::Worktree)))
                {
                    this.reload(cx);
                }
            }),
        ];
        let bridge = SelectionBridge::new(cx);
        let mut view = Self {
            ctx,
            target: None,
            doc: None,
            load: ReadSlot::default(),
            highlight: ReadSlot::default(),
            scroll: UniformListScrollHandle::new(),
            bridge,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        view.sync(cx);
        view
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The file and revision being shown or loaded.
    pub fn target(&self) -> Option<&(RelaPath, BlameRev)> {
        self.target.as_ref()
    }

    /// The loaded blame (`None` while loading).
    pub fn doc(&self) -> Option<&BlameDoc> {
        self.doc.as_ref()
    }

    fn on_ui_event(&mut self, event: &RepoUiEvent, cx: &mut Context<Self>) {
        match event {
            RepoUiEvent::CenterChanged | RepoUiEvent::ViewStackChanged => self.sync(cx),
            RepoUiEvent::FilesChanged => self.minimize_for_diff(cx),
            _ => {}
        }
        cx.notify();
    }

    /// 09 §1.1: opening a single-file diff minimizes an open blame.
    fn minimize_for_diff(&mut self, cx: &mut Context<Self>) {
        let ui = self.ctx.ui.clone();
        let model = self.ctx.model.clone();
        let diff_open = {
            let ui = ui.read(cx);
            ui.blame.as_ref().is_some_and(|b| !b.minimized)
                && ui.diff_target(model.read(cx)).is_some()
        };
        if diff_open {
            ui.update(cx, |ui, cx| {
                if let Some(blame) = ui.blame.as_mut() {
                    blame.minimized = true;
                }
                cx.emit(RepoUiEvent::CenterChanged);
                cx.notify();
            });
        }
    }

    /// Starts the read when `RepoUi::blame` names a file other than the one loaded.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let want = self
            .ctx
            .ui
            .read(cx)
            .blame
            .as_ref()
            .map(|b| (b.path.clone(), b.rev));
        if want == self.target {
            return;
        }
        self.target = want;
        self.doc = None;
        self.bridge.set_doc(None);
        self.highlight.cancel();
        self.load.cancel();
        self.reload(cx);
    }

    /// (Re)reads the blame of the current target, keeping the old rows until the new ones arrive.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some((path, rev)) = self.target.clone() else {
            return;
        };
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            return;
        };
        self.load.start(
            cx,
            |this| &mut this.load,
            // The document's strings are built here too, off the UI thread.
            move |cancel| {
                crate::backend::history::blame(&repo, &path, rev, cancel).map(BlameDoc::new)
            },
            |this, result, cx| this.apply_blame(result, cx),
        );
    }

    fn apply_blame(
        &mut self,
        result: Result<BlameDoc, crate::backend::GitError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(doc) => {
                self.bridge.set_doc(Some(doc.clone()));
                self.doc = Some(doc);
                self.start_highlight(cx);
            }
            Err(err) if err.kind() == GitErrorKind::Cancelled => {}
            Err(err) => {
                // 09 §2.1: the state resets and the error goes to the global error path; reads show a toast.
                show_toast(
                    cx,
                    Some(self.ctx.id),
                    ToastKind::Error,
                    format!("File blaming failed: {err}"),
                    None,
                );
                self.target = None;
                close(&self.ctx, cx);
            }
        }
        cx.notify();
    }

    fn start_highlight(&mut self, cx: &mut Context<Self>) {
        let (Some(doc), Some((path, _))) = (self.doc.as_ref(), self.target.as_ref()) else {
            return;
        };
        let result = doc.result.clone();
        let work_doc = doc.clone();
        let path = path.to_str_lossy().into_owned();
        self.highlight.start(
            cx,
            |this| &mut this.highlight,
            move |cancel| {
                let refs: Vec<&str> = work_doc.texts.iter().map(|l| l.as_ref()).collect();
                Highlighter::global().highlight(&path, &refs, cancel)
            },
            move |this, tokens, cx| {
                let bridge = this.bridge.clone();
                if let Some(doc) = this
                    .doc
                    .as_mut()
                    .filter(|d| std::sync::Arc::ptr_eq(&d.result, &result))
                {
                    doc.syntax = std::sync::Arc::new(tokens);
                    bridge.set_doc(Some(doc.clone()));
                    cx.notify();
                }
            },
        );
    }

    /// The advance of the 14 sp mono font, measured (the gutter is `digits` of it wide).
    fn mono_advance(window: &Window) -> Pixels {
        let size = dpx(m::CODE_TEXT, window);
        let system = window.text_system();
        let font = Font {
            family: FONT_MONO.into(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
        };
        let id = system.resolve_font(&font);
        system
            .advance(id, size, '0')
            .map(|s| s.width)
            .unwrap_or(size * m::MONO_ADVANCE_EM)
    }

    /// Copies the selection to the clipboard (Ctrl+C and the menu's Copy). Unlike the kit's window copy, the text
    /// is not trimmed: the first selected line keeps its indentation.
    fn copy(bridge: &SelectionBridge, cx: &mut App) {
        let text = bridge.selected_text(cx);
        if !text.is_empty() {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
        }
    }

    fn menu(view: Entity<Self>) -> Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)> {
        Rc::new(move |event, window, cx| {
            let (has_selection, bridge) =
                view.read_with(cx, |v, cx| (v.bridge.has_selection(cx), v.bridge.clone()));
            let copy_bridge = bridge.clone();
            let items = crate::menus::diff_selection::selection_menu(
                has_selection,
                Rc::new(move |_, cx| Self::copy(&copy_bridge, cx)),
                Rc::new(move |window, cx| {
                    bridge.select_all(cx);
                    window.refresh();
                }),
            );
            crate::menus::open_context_menu(window, cx, event.position, items);
        })
    }

    fn render_minimized(&self, target: &BlameTarget, cx: &App) -> gpui_kit::AnyElement {
        let tokens = *cx.tokens();
        let ctx_show = self.ctx.clone();
        let ctx_close = self.ctx.clone();
        let path = SharedString::from(target.path.to_str_lossy().into_owned());
        div()
            // The focus may be in the view when it gets minimized: keep it inside the frame.
            .track_focus(&self.focus)
            .w_full()
            .h(dp(m::MINIMIZED_H))
            .flex()
            .items_center()
            .bg(tokens.surface)
            .text_color(tokens.on_background)
            .child(
                div()
                    .flex_shrink(1.)
                    .min_w_0()
                    .px(dp(m::MINIMIZED_PAD_H))
                    .child(div().text_caption().child("Minimized file blame"))
                    .child(div().truncate().text_body2().child(path)),
            )
            .child(div().flex_1())
            .child(
                PrimaryButton::new(ids::BLAME_SHOW, "Show")
                    .on_click(move |_, cx| expand(&ctx_show, cx)),
            )
            .child(
                // Gitnuro: `contentDescription "Close blame"` (accessibility only, no tooltip).
                div().px(dp(m::MINIMIZED_PAD_H)).child(
                    IconButton::new(ids::BLAME_MINIMIZED_CLOSE, AppIcon::Close)
                        .icon_size(m::CLOSE_ICON)
                        .on_click(move |window, cx| close_and_refocus(&ctx_close, window, cx)),
                ),
            )
            .observed(ids::BLAME)
    }
}

impl Render for BlameView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let target = self.ctx.ui.read(cx).blame.clone();
        let Some(target) = target else {
            return observed(ids::BLAME).size_full().into_any_element();
        };
        if self.target.as_ref().map(|(p, r)| (p, r)) != Some((&target.path, &target.rev)) {
            // The field changed without an event: catch up after this frame.
            let view = cx.entity();
            window.defer(cx, move |_, cx| {
                view.update(cx, |v, cx| {
                    v.sync(cx);
                    cx.notify();
                })
            });
        }
        if target.minimized {
            return self.render_minimized(&target, cx);
        }

        let ctx = self.ctx.clone();
        let path = SharedString::from(target.path.to_str_lossy().into_owned());
        let body = match self.doc.clone() {
            Some(doc) => {
                let advance = Self::mono_advance(window);
                let gutter_w = advance * doc.digits as f32 + dpx(2. * m::GUTTER_PAD_H, window);
                let env = Rc::new(RowEnv {
                    tokens,
                    ctx: self.ctx.clone(),
                    bridge: self.bridge.clone(),
                    gutter_w,
                    selection: tokens.primary.opacity(m::SELECTION_ALPHA),
                    on_menu: Self::menu(cx.entity()),
                });
                let count = doc.line_count();
                let list = uniform_list(ids::BLAME_LIST, count, move |range, _, _| {
                    range
                        .map(|ix| rows::row(&env, &doc, ix))
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.scroll)
                .size_full();
                let code_left = gutter_w + dpx(m::COMMIT_CELL_W, window);
                let row_h = dpx(m::ROW_H, window);
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(div().size_full().child(CodeColumn::new(
                        list,
                        self.bridge.clone(),
                        code_left,
                        row_h,
                        self.scroll.clone(),
                    )))
                    .child(list_scrollbar("blame-scrollbar", &self.scroll))
                    .into_any_element()
            }
            // 09 §2.1: no loading indicator.
            None => div().flex_1().into_any_element(),
        };

        let focus = self.focus.clone();
        let bridge = self.bridge.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(tokens.background)
            .key_context(BLAME_CONTEXT)
            .track_focus(&self.focus)
            .on_action(move |_: &BlameCopy, _, cx| Self::copy(&bridge, cx))
            // 09 §2.1: the root is focusable; a click into it focuses it (Ctrl+C; Esc still reaches the frame).
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&focus, cx)
            })
            .child(view_header(
                ids::BLAME_HEADER,
                ids::BLAME_CLOSE,
                ids::BLAME_TITLE,
                path,
                move |window, cx| close_and_refocus(&ctx, window, cx),
                cx,
            ))
            .child(body)
            .observed(ids::BLAME)
    }
}

/// Registers the blame view's key bindings: Ctrl+C (Cmd+C) copies the selected code. Esc is the frame's
/// `CloseLastView` (the blame is a closeable view).
pub fn register(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "secondary-c",
        BlameCopy,
        Some(BLAME_CONTEXT),
    )]);
}
